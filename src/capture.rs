use crate::config::RuntimeConfig;
use crate::error::{ErrorInfo, ErrorKind, ProxyResult};
use crate::event::ProxyEvent;
use crate::tls::{
    BackendTlsMetadata, FrontendTlsMetadata, backend_tls_metadata, frontend_tls_metadata,
};
use crate::writer::ParquetWriterHandle;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use rama::extensions::{ExtensionsRef, InputExtensions};
use rama::http::{
    HeaderMap, HeaderValue, Request, Response, StatusCode, StreamingBody, Version, header,
};
use rama::net::stream::{ClientSocketInfo, SocketInfo};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, ready};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct SharedExchangeCapture(Arc<Mutex<ExchangeCapture>>);

impl SharedExchangeCapture {
    pub fn new(cfg: &RuntimeConfig, req: &Request) -> Self {
        let started_at = OffsetDateTime::now_utc();
        let socket_info = req.extensions().get::<SocketInfo>().cloned();
        let client_from_headers = cfg
            .trust_proxy_headers
            .then(|| forwarded_client_ip(req.headers()))
            .flatten();
        let scheme_from_headers = cfg
            .trust_proxy_headers
            .then(|| forwarded_proto(req.headers()))
            .flatten();
        let frontend_tls = frontend_tls_metadata(req.extensions());
        let authority = request_authority(req);
        let client_ip = client_from_headers.or_else(|| {
            socket_info
                .as_ref()
                .map(|info| info.peer_addr().ip().to_string())
        });
        let client_port = socket_info.as_ref().map(|info| info.peer_addr().port());
        let proxy_local_ip = socket_info
            .as_ref()
            .and_then(|info| info.local_addr().copied())
            .map(|addr| addr.ip().to_string());
        let proxy_local_port = socket_info
            .as_ref()
            .and_then(|info| info.local_addr().copied())
            .map(|addr| addr.port())
            .or(Some(cfg.listen_addr.port()));

        Self(Arc::new(Mutex::new(ExchangeCapture {
            event_id: Uuid::new_v4(),
            connection_id: Uuid::new_v4(),
            request_start_ts: started_at,
            client_ip,
            client_port,
            proxy_local_ip,
            proxy_local_port,
            frontend_server_name: Some(cfg.frontend_domain.clone()),
            frontend_http_version: Some(format_http_version(req.version())),
            frontend_scheme: scheme_from_headers.unwrap_or_else(|| "https".to_owned()),
            backend_url: cfg.backend_url.to_string(),
            backend_host: cfg.backend_host.clone(),
            backend_port: Some(cfg.backend_port),
            method: req.method().to_string(),
            authority,
            path: req.uri().path().to_owned(),
            query: req.uri().query().map(ToOwned::to_owned),
            request_headers_json: serialize_headers(req.headers(), &cfg.header_log_policy),
            request_cookie_header: header_value(req.headers(), header::COOKIE),
            request_cookies_json: parse_cookies(req.headers().get(header::COOKIE)),
            request_content_length: parse_content_length(req.headers()),
            request_transfer_encoding: header_value(req.headers(), header::TRANSFER_ENCODING),
            request_content_type: header_value(req.headers(), header::CONTENT_TYPE),
            response_headers_json: None,
            response_set_cookie_json: None,
            response_content_length: None,
            response_transfer_encoding: None,
            response_content_type: None,
            response_content_encoding: None,
            status_code: None,
            reason_phrase: None,
            backend_ip: None,
            backend_http_version: None,
            upstream_connection_reused: None,
            frontend_tls,
            backend_tls: BackendTlsMetadata::default(),
            upstream_first_byte_ms: None,
            response_started_at: None,
            proxy_result: ProxyResult::Inflight,
            error: None,
            request_body: BodyAccumulator::new(cfg.body_max_bytes),
            response_body: BodyAccumulator::new(cfg.body_max_bytes),
            finalized: false,
        })))
    }

    pub fn set_rejection(&self, status: StatusCode, error: ErrorInfo) {
        let mut guard = self.lock();
        guard.status_code = Some(status.as_u16());
        guard.reason_phrase = status.canonical_reason().map(ToOwned::to_owned);
        guard.proxy_result = ProxyResult::ClientRejected;
        guard.error = Some(error);
    }

    pub fn set_upstream_response(
        &self,
        cfg: &RuntimeConfig,
        resp: &Response,
        response_started_at: OffsetDateTime,
    ) {
        let mut guard = self.lock();
        guard.status_code = Some(resp.status().as_u16());
        guard.reason_phrase = resp.status().canonical_reason().map(ToOwned::to_owned);
        guard.response_headers_json =
            Some(serialize_headers(resp.headers(), &cfg.header_log_policy));
        guard.response_set_cookie_json = parse_set_cookies(resp.headers());
        guard.response_content_length = parse_content_length(resp.headers());
        guard.response_transfer_encoding = header_value(resp.headers(), header::TRANSFER_ENCODING);
        guard.response_content_type = header_value(resp.headers(), header::CONTENT_TYPE);
        guard.response_content_encoding = header_value(resp.headers(), header::CONTENT_ENCODING);
        guard.backend_http_version = Some(format_http_version(resp.version()));
        guard.response_started_at = Some(response_started_at);
        guard.upstream_first_byte_ms = Some(duration_ms_i64(
            response_started_at - guard.request_start_ts,
        ));
        guard.proxy_result = ProxyResult::Success;

        if let Some(input_extensions) = resp.extensions().get::<InputExtensions>() {
            let ext = &input_extensions.0;
            if let Some(socket) = ext.get::<ClientSocketInfo>() {
                guard.backend_ip = Some(socket.peer_addr().ip().to_string());
                guard.backend_port = Some(socket.peer_addr().port());
            }
            guard.backend_tls = backend_tls_metadata(ext, &cfg.upstream_sni);
        }
    }

    pub fn set_upstream_error(&self, error: ErrorInfo) {
        let mut guard = self.lock();
        guard.proxy_result = ProxyResult::UpstreamError;
        guard.error = Some(error);
    }

    pub fn observe_request_bytes(&self, chunk: &Bytes) {
        self.lock().request_body.observe(chunk);
    }

    pub fn observe_response_bytes(&self, chunk: &Bytes) {
        self.lock().response_body.observe(chunk);
    }

    pub fn finalize_and_send(&self, writer: &ParquetWriterHandle) {
        let event = {
            let mut guard = self.lock();
            if guard.finalized {
                return;
            }
            guard.finalized = true;
            guard.build_event()
        };
        writer.try_send(event);
    }

    pub fn finalize_with_body_error_and_send(
        &self,
        writer: &ParquetWriterHandle,
        error: ErrorInfo,
    ) {
        {
            let mut guard = self.lock();
            if guard.error.is_none() {
                guard.proxy_result = ProxyResult::StreamError;
                guard.error = Some(error);
            }
        }
        self.finalize_and_send(writer);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ExchangeCapture> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[derive(Debug)]
struct ExchangeCapture {
    event_id: Uuid,
    connection_id: Uuid,
    request_start_ts: OffsetDateTime,
    client_ip: Option<String>,
    client_port: Option<u16>,
    proxy_local_ip: Option<String>,
    proxy_local_port: Option<u16>,
    frontend_server_name: Option<String>,
    frontend_http_version: Option<String>,
    frontend_scheme: String,
    backend_url: String,
    backend_host: String,
    backend_ip: Option<String>,
    backend_port: Option<u16>,
    backend_http_version: Option<String>,
    upstream_connection_reused: Option<bool>,
    method: String,
    authority: Option<String>,
    path: String,
    query: Option<String>,
    request_headers_json: String,
    request_cookie_header: Option<String>,
    request_cookies_json: Option<String>,
    request_content_length: Option<i64>,
    request_transfer_encoding: Option<String>,
    request_content_type: Option<String>,
    response_headers_json: Option<String>,
    response_set_cookie_json: Option<String>,
    response_content_length: Option<i64>,
    response_transfer_encoding: Option<String>,
    response_content_type: Option<String>,
    response_content_encoding: Option<String>,
    status_code: Option<u16>,
    reason_phrase: Option<String>,
    frontend_tls: FrontendTlsMetadata,
    backend_tls: BackendTlsMetadata,
    upstream_first_byte_ms: Option<i64>,
    response_started_at: Option<OffsetDateTime>,
    proxy_result: ProxyResult,
    error: Option<ErrorInfo>,
    request_body: BodyAccumulator,
    response_body: BodyAccumulator,
    finalized: bool,
}

impl ExchangeCapture {
    fn build_event(&mut self) -> ProxyEvent {
        let ended_at = OffsetDateTime::now_utc();
        let response_stream_duration_ms = self
            .response_started_at
            .map(|value| duration_ms_i64(ended_at - value));

        let request_body = self.request_body.finish();
        let response_body = self.response_body.finish();

        ProxyEvent {
            event_id: self.event_id,
            connection_id: self.connection_id,
            request_start_ts: self.request_start_ts,
            request_end_ts: ended_at,
            duration_ms: duration_ms_i64(ended_at - self.request_start_ts),
            client_ip: self.client_ip.clone(),
            client_port: self.client_port,
            proxy_local_ip: self.proxy_local_ip.clone(),
            proxy_local_port: self.proxy_local_port,
            frontend_server_name: self.frontend_server_name.clone(),
            frontend_http_version: self.frontend_http_version.clone(),
            frontend_scheme: self.frontend_scheme.clone(),
            backend_url: self.backend_url.clone(),
            backend_host: self.backend_host.clone(),
            backend_ip: self.backend_ip.clone(),
            backend_port: self.backend_port,
            backend_http_version: self.backend_http_version.clone(),
            upstream_connection_reused: self.upstream_connection_reused,
            method: self.method.clone(),
            authority: self.authority.clone(),
            path: self.path.clone(),
            query: self.query.clone(),
            request_headers_json: self.request_headers_json.clone(),
            request_cookie_header: self.request_cookie_header.clone(),
            request_cookies_json: self.request_cookies_json.clone(),
            request_content_length: self.request_content_length,
            request_transfer_encoding: self.request_transfer_encoding.clone(),
            request_content_type: self.request_content_type.clone(),
            request_body_sha256: request_body.sha256,
            request_body_preview_base64: request_body.preview_base64,
            request_body_truncated: request_body.truncated,
            status_code: self.status_code,
            reason_phrase: self.reason_phrase.clone(),
            response_headers_json: self.response_headers_json.clone(),
            response_set_cookie_json: self.response_set_cookie_json.clone(),
            response_content_length: self.response_content_length,
            response_transfer_encoding: self.response_transfer_encoding.clone(),
            response_content_type: self.response_content_type.clone(),
            response_content_encoding: self.response_content_encoding.clone(),
            response_body_sha256: response_body.sha256,
            response_body_preview_base64: response_body.preview_base64,
            response_body_truncated: response_body.truncated,
            frontend_tls_version: self.frontend_tls.version.clone(),
            frontend_tls_cipher_suite: self.frontend_tls.cipher_suite.clone(),
            frontend_tls_alpn: self.frontend_tls.alpn.clone(),
            frontend_tls_sni: self.frontend_tls.sni.clone(),
            frontend_tls_ja3: self.frontend_tls.ja3.clone(),
            frontend_tls_ja4: self.frontend_tls.ja4.clone(),
            backend_tls_version: self.backend_tls.version.clone(),
            backend_tls_cipher_suite: self.backend_tls.cipher_suite.clone(),
            backend_tls_alpn: self.backend_tls.alpn.clone(),
            backend_tls_sni: self.backend_tls.sni.clone(),
            backend_cert_leaf_sha256: self.backend_tls.cert_leaf_sha256.clone(),
            backend_cert_subject: self.backend_tls.cert_subject.clone(),
            backend_cert_issuer: self.backend_tls.cert_issuer.clone(),
            backend_cert_not_before: self.backend_tls.cert_not_before,
            backend_cert_not_after: self.backend_tls.cert_not_after,
            dns_duration_ms: None,
            connect_duration_ms: None,
            tls_handshake_duration_ms: None,
            upstream_first_byte_ms: self.upstream_first_byte_ms,
            response_stream_duration_ms,
            error_kind: self.error.as_ref().map(|error| error.kind),
            error_text: self.error.as_ref().map(|error| error.text.clone()),
            proxy_result: self.proxy_result,
        }
    }
}

#[derive(Debug)]
struct BodyAccumulator {
    max_bytes: usize,
    hasher: Sha256,
    retained_bytes: Vec<u8>,
    truncated: bool,
    saw_bytes: bool,
}

#[derive(Debug)]
struct BodySummary {
    sha256: Option<String>,
    preview_base64: Option<String>,
    truncated: Option<bool>,
}

impl BodyAccumulator {
    fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            hasher: Sha256::new(),
            retained_bytes: Vec::new(),
            truncated: false,
            saw_bytes: false,
        }
    }

    fn observe(&mut self, chunk: &Bytes) {
        if chunk.is_empty() {
            return;
        }

        self.saw_bytes = true;
        self.hasher.update(chunk);

        if self.retained_bytes.len() < self.max_bytes {
            let remaining = self.max_bytes - self.retained_bytes.len();
            let take = remaining.min(chunk.len());
            self.retained_bytes.extend_from_slice(&chunk[..take]);
            if take < chunk.len() {
                self.truncated = true;
            }
        } else {
            self.truncated = true;
        }
    }

    fn finish(&mut self) -> BodySummary {
        let sha256 = self
            .saw_bytes
            .then(|| hex::encode(self.hasher.clone().finalize()));
        let preview_base64 =
            (!self.retained_bytes.is_empty()).then(|| BASE64.encode(&self.retained_bytes));
        let truncated = Some(self.truncated);

        BodySummary {
            sha256,
            preview_base64,
            truncated,
        }
    }
}

pub struct ObservedBody<B> {
    inner: B,
    capture: SharedExchangeCapture,
    writer: Option<ParquetWriterHandle>,
    direction: BodyDirection,
    finalized: bool,
    completed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BodyDirection {
    Request,
    Response,
}

impl<B> ObservedBody<B> {
    pub fn request(inner: B, capture: SharedExchangeCapture) -> Self {
        Self {
            inner,
            capture,
            writer: None,
            direction: BodyDirection::Request,
            finalized: false,
            completed: false,
        }
    }

    pub fn response(inner: B, capture: SharedExchangeCapture, writer: ParquetWriterHandle) -> Self {
        Self {
            inner,
            capture,
            writer: Some(writer),
            direction: BodyDirection::Response,
            finalized: false,
            completed: false,
        }
    }
}

impl<B> StreamingBody for ObservedBody<B>
where
    B: StreamingBody<Data = Bytes>,
    B::Error: std::fmt::Display,
{
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<rama::http::body::Frame<Self::Data>, Self::Error>>> {
        let this = unsafe { self.as_mut().get_unchecked_mut() };
        let poll = ready!(unsafe { Pin::new_unchecked(&mut this.inner) }.poll_frame(cx));

        match poll {
            Some(Ok(frame)) => {
                if let Some(chunk) = frame.data_ref() {
                    match this.direction {
                        BodyDirection::Request => this.capture.observe_request_bytes(chunk),
                        BodyDirection::Response => this.capture.observe_response_bytes(chunk),
                    }
                }
                this.completed = this.inner.is_end_stream();
                Poll::Ready(Some(Ok(frame)))
            }
            Some(Err(err)) => {
                if this.direction == BodyDirection::Response
                    && let Some(writer) = &this.writer
                {
                    this.capture.finalize_with_body_error_and_send(
                        writer,
                        ErrorInfo::from_display(ErrorKind::UpstreamProtocol, &err),
                    );
                    this.finalized = true;
                }
                Poll::Ready(Some(Err(err)))
            }
            None => {
                this.completed = true;
                if this.direction == BodyDirection::Response
                    && let Some(writer) = &this.writer
                    && !this.finalized
                {
                    this.capture.finalize_and_send(writer);
                    this.finalized = true;
                }
                Poll::Ready(None)
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> rama::http::body::SizeHint {
        self.inner.size_hint()
    }
}

impl<B> Drop for ObservedBody<B> {
    fn drop(&mut self) {
        if self.direction == BodyDirection::Response
            && !self.finalized
            && let Some(writer) = &self.writer
        {
            if self.completed {
                self.capture.finalize_and_send(writer);
            } else {
                self.capture.finalize_with_body_error_and_send(
                    writer,
                    ErrorInfo::new(
                        ErrorKind::ClientProtocol,
                        "response body dropped before completion",
                    ),
                );
            }
            self.finalized = true;
        }
    }
}

pub fn request_authority(req: &Request) -> Option<String> {
    req.uri()
        .authority()
        .map(|authority| authority.as_str().to_owned())
        .or_else(|| header_value(req.headers(), header::HOST))
}

pub fn host_matches_frontend(req: &Request, frontend_domain: &str) -> bool {
    request_authority(req)
        .and_then(|authority| authority_host(&authority))
        .map(|host| host.eq_ignore_ascii_case(frontend_domain))
        .unwrap_or(false)
}

pub fn authority_host(authority: &str) -> Option<String> {
    authority
        .rsplit_once('@')
        .map_or(authority, |(_, value)| value)
        .split_once(':')
        .map_or_else(
            || Some(authority.to_ascii_lowercase()),
            |(host, _)| Some(host.to_ascii_lowercase()),
        )
}

pub fn make_gateway_error(status: StatusCode, message: &str) -> Response {
    let mut response = Response::new(rama::http::Body::from(message.to_owned()));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

pub fn header_value(headers: &HeaderMap, name: header::HeaderName) -> Option<String> {
    headers.get(name).map(stringify_header_value)
}

pub fn strip_hop_by_hop_headers(headers: &mut HeaderMap) {
    let connection_tokens = headers
        .get(header::CONNECTION)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(',')
                .map(|token| token.trim().to_ascii_lowercase())
                .filter(|token| !token.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    for token in connection_tokens {
        if let Ok(name) = header::HeaderName::from_bytes(token.as_bytes()) {
            headers.remove(name);
        }
    }

    headers.remove(header::CONNECTION);
    headers.remove(header::PROXY_AUTHENTICATE);
    headers.remove(header::PROXY_AUTHORIZATION);
    headers.remove(header::TE);
    headers.remove(header::TRAILER);
    headers.remove(header::TRANSFER_ENCODING);
    headers.remove(header::UPGRADE);
    headers.remove(header::KEEP_ALIVE.clone());
}

fn serialize_headers(headers: &HeaderMap, policy: &crate::config::HeaderLogPolicy) -> String {
    let mut json_headers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, value) in headers {
        let normalized = name.as_str().to_ascii_lowercase();
        if !policy.allows(&normalized) {
            continue;
        }
        json_headers
            .entry(normalized)
            .or_default()
            .push(stringify_header_value(value));
    }
    serde_json::to_string(&json_headers).unwrap_or_else(|_| ProxyEvent::empty_json())
}

fn parse_cookies(value: Option<&HeaderValue>) -> Option<String> {
    let raw = value.and_then(|value| value.to_str().ok())?;
    let mut cookies = BTreeMap::new();
    for segment in raw.split(';') {
        let trimmed = segment.trim();
        if let Some((name, val)) = trimmed.split_once('=') {
            cookies.insert(name.trim().to_owned(), val.trim().to_owned());
        }
    }
    serde_json::to_string(&cookies).ok()
}

fn parse_set_cookies(headers: &HeaderMap) -> Option<String> {
    let items = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(parse_set_cookie_value)
        .collect::<Vec<_>>();
    if items.is_empty() {
        None
    } else {
        serde_json::to_string(&items).ok()
    }
}

fn parse_set_cookie_value(raw: &str) -> serde_json::Value {
    let mut parts = raw.split(';');
    let first = parts.next().unwrap_or_default();
    let (name, value) = first.split_once('=').unwrap_or((first, ""));
    let mut attrs = BTreeMap::new();
    for attr in parts {
        let trimmed = attr.trim();
        match trimmed.split_once('=') {
            Some((key, val)) => {
                attrs.insert(
                    key.to_ascii_lowercase(),
                    serde_json::Value::String(val.to_owned()),
                );
            }
            None if !trimmed.is_empty() => {
                attrs.insert(trimmed.to_ascii_lowercase(), serde_json::Value::Bool(true));
            }
            None => {}
        }
    }

    serde_json::json!({
        "name": name.trim(),
        "value": value.trim(),
        "attributes": attrs,
        "raw": raw,
    })
}

fn stringify_header_value(value: &HeaderValue) -> String {
    value
        .to_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|_| format!("0x{}", hex::encode(value.as_bytes())))
}

fn parse_content_length(headers: &HeaderMap) -> Option<i64> {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
}

fn forwarded_client_ip(headers: &HeaderMap) -> Option<String> {
    if let Some(forwarded) = headers
        .get("forwarded")
        .and_then(|value| value.to_str().ok())
    {
        for part in forwarded.split(';') {
            let trimmed = part.trim();
            if let Some(value) = trimmed.strip_prefix("for=") {
                return Some(
                    value
                        .trim_matches('"')
                        .trim_matches('[')
                        .trim_matches(']')
                        .to_owned(),
                );
            }
        }
    }

    headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(|value| value.trim().to_owned())
}

fn forwarded_proto(headers: &HeaderMap) -> Option<String> {
    if let Some(forwarded) = headers
        .get("forwarded")
        .and_then(|value| value.to_str().ok())
    {
        for part in forwarded.split(';') {
            let trimmed = part.trim();
            if let Some(value) = trimmed.strip_prefix("proto=") {
                return Some(value.trim_matches('"').to_owned());
            }
        }
    }

    headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

fn format_http_version(version: Version) -> String {
    match version {
        Version::HTTP_09 => "HTTP/0.9".to_owned(),
        Version::HTTP_10 => "HTTP/1.0".to_owned(),
        Version::HTTP_11 => "HTTP/1.1".to_owned(),
        Version::HTTP_2 => "HTTP/2".to_owned(),
        Version::HTTP_3 => "HTTP/3".to_owned(),
        _ => format!("{version:?}"),
    }
}

fn duration_ms_i64(value: time::Duration) -> i64 {
    value.whole_milliseconds() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use rama::http::header::{self, HeaderName};

    #[test]
    fn extracts_authority_host_without_port() {
        assert_eq!(
            authority_host("example.test:8443"),
            Some("example.test".to_owned())
        );
        assert_eq!(
            authority_host("user@example.test:8443"),
            Some("example.test".to_owned())
        );
        assert_eq!(
            authority_host("example.test"),
            Some("example.test".to_owned())
        );
    }

    #[test]
    fn strips_hop_by_hop_headers_and_connection_tokens() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONNECTION,
            HeaderValue::from_static("x-remove, keep-alive"),
        );
        headers.insert(
            HeaderName::from_static("x-remove"),
            HeaderValue::from_static("1"),
        );
        headers.insert(
            header::KEEP_ALIVE.clone(),
            HeaderValue::from_static("timeout=5"),
        );
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));

        strip_hop_by_hop_headers(&mut headers);

        assert!(!headers.contains_key(header::CONNECTION));
        assert!(!headers.contains_key("x-remove"));
        assert!(!headers.contains_key(header::KEEP_ALIVE.clone()));
        assert_eq!(
            headers
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/plain")
        );
    }
}
