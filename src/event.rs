use crate::error::{ErrorKind, ProxyResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyEvent {
    pub event_id: Uuid,
    pub connection_id: Uuid,

    pub request_start_ts: OffsetDateTime,
    pub request_end_ts: OffsetDateTime,
    pub duration_ms: i64,

    pub client_ip: Option<String>,
    pub client_port: Option<u16>,
    pub proxy_local_ip: Option<String>,
    pub proxy_local_port: Option<u16>,
    pub frontend_server_name: Option<String>,
    pub frontend_http_version: Option<String>,
    pub frontend_scheme: String,

    pub backend_url: String,
    pub backend_host: String,
    pub backend_ip: Option<String>,
    pub backend_port: Option<u16>,
    pub backend_http_version: Option<String>,
    pub upstream_connection_reused: Option<bool>,

    pub method: String,
    pub authority: Option<String>,
    pub path: String,
    pub query: Option<String>,

    pub request_headers_json: String,
    pub request_cookie_header: Option<String>,
    pub request_cookies_json: Option<String>,
    pub request_content_length: Option<i64>,
    pub request_transfer_encoding: Option<String>,
    pub request_content_type: Option<String>,
    pub request_body_sha256: Option<String>,
    pub request_body_preview_base64: Option<String>,
    pub request_body_truncated: Option<bool>,

    pub status_code: Option<u16>,
    pub reason_phrase: Option<String>,
    pub response_headers_json: Option<String>,
    pub response_set_cookie_json: Option<String>,
    pub response_content_length: Option<i64>,
    pub response_transfer_encoding: Option<String>,
    pub response_content_type: Option<String>,
    pub response_content_encoding: Option<String>,
    pub response_body_sha256: Option<String>,
    pub response_body_preview_base64: Option<String>,
    pub response_body_truncated: Option<bool>,

    pub frontend_tls_version: Option<String>,
    pub frontend_tls_cipher_suite: Option<String>,
    pub frontend_tls_alpn: Option<String>,
    pub frontend_tls_sni: Option<String>,
    pub frontend_tls_ja3: Option<String>,
    pub frontend_tls_ja4: Option<String>,

    pub backend_tls_version: Option<String>,
    pub backend_tls_cipher_suite: Option<String>,
    pub backend_tls_alpn: Option<String>,
    pub backend_tls_sni: Option<String>,
    pub backend_cert_leaf_sha256: Option<String>,
    pub backend_cert_subject: Option<String>,
    pub backend_cert_issuer: Option<String>,
    pub backend_cert_not_before: Option<OffsetDateTime>,
    pub backend_cert_not_after: Option<OffsetDateTime>,

    pub dns_duration_ms: Option<i64>,
    pub connect_duration_ms: Option<i64>,
    pub tls_handshake_duration_ms: Option<i64>,
    pub upstream_first_byte_ms: Option<i64>,
    pub response_stream_duration_ms: Option<i64>,

    pub error_kind: Option<ErrorKind>,
    pub error_text: Option<String>,
    pub proxy_result: ProxyResult,
}

impl ProxyEvent {
    pub fn empty_json() -> String {
        Value::Object(Default::default()).to_string()
    }
}
