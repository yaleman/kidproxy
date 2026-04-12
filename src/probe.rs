use crate::config::RuntimeConfig;
use crate::proxy::build_upstream_client;
use crate::tls::backend_tls_metadata;
use anyhow::Context;
use rama::extensions::{ExtensionsRef, InputExtensions};
use rama::http::service::client::HttpClientExt;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendProbe {
    pub backend_url: String,
    pub negotiated_alpn: Option<String>,
    pub tls_version: Option<String>,
    pub supports_h2: Option<bool>,
    pub cert_subject: Option<String>,
    pub cert_issuer: Option<String>,
    pub cert_not_before: Option<OffsetDateTime>,
    pub cert_not_after: Option<OffsetDateTime>,
    pub sample_server_header: Option<String>,
    pub sample_content_encoding: Option<String>,
}

impl BackendProbe {
    pub fn fallback(cfg: &RuntimeConfig) -> Self {
        Self {
            backend_url: cfg.backend_url.to_string(),
            negotiated_alpn: None,
            tls_version: None,
            supports_h2: None,
            cert_subject: None,
            cert_issuer: None,
            cert_not_before: None,
            cert_not_after: None,
            sample_server_header: None,
            sample_content_encoding: None,
        }
    }
}

pub async fn probe_backend(cfg: &RuntimeConfig) -> anyhow::Result<BackendProbe> {
    let client = build_upstream_client(cfg, cfg.resolved_http_mode(None))?;
    let response = client
        .get(cfg.backend_url.as_str())
        .header("accept-encoding", "gzip")
        .send()
        .await
        .context("probe request failed")?;

    let mut probe = BackendProbe::fallback(cfg);
    probe.supports_h2 = Some(response.version() == rama::http::Version::HTTP_2);
    probe.sample_server_header = response
        .headers()
        .get(rama::http::header::SERVER)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    probe.sample_content_encoding = response
        .headers()
        .get(rama::http::header::CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);

    if let Some(input_extensions) = response.extensions().get::<InputExtensions>() {
        let tls = backend_tls_metadata(&input_extensions.0, &cfg.upstream_sni);
        probe.negotiated_alpn = tls.alpn;
        probe.tls_version = tls.version;
        probe.cert_subject = tls.cert_subject;
        probe.cert_issuer = tls.cert_issuer;
        probe.cert_not_before = tls.cert_not_before;
        probe.cert_not_after = tls.cert_not_after;
    }

    Ok(probe)
}
