use crate::config::{ResolvedHttpMode, RuntimeConfig};
use anyhow::{Context, bail};
use rama::crypto::pki_types::{CertificateDer, PrivateKeyDer};
use rama::extensions::Extensions;
use rama::net::address::Host;

use rama::tls::client::{NegotiatedTlsParameters, TlsClientConfig};
use rama::tls::fingerprint::{Ja3, Ja4};
use rama::tls::{KeyLogIntent, SecureTransport};

use rama::tls::server::TlsServerConfig;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::BufReader;
use std::sync::Once;
use time::OffsetDateTime;
use x509_parser::prelude::FromDer;

#[derive(Debug, Clone, Default)]
pub struct FrontendTlsMetadata {
    pub version: Option<String>,
    pub cipher_suite: Option<String>,
    pub alpn: Option<String>,
    pub sni: Option<String>,
    pub ja3: Option<String>,
    pub ja4: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct BackendTlsMetadata {
    pub version: Option<String>,
    pub cipher_suite: Option<String>,
    pub alpn: Option<String>,
    pub sni: Option<String>,
    pub cert_leaf_sha256: Option<String>,
    pub cert_subject: Option<String>,
    pub cert_issuer: Option<String>,
    pub cert_not_before: Option<OffsetDateTime>,
    pub cert_not_after: Option<OffsetDateTime>,
}

pub fn build_frontend_tls(
    cfg: &RuntimeConfig,
    http_mode: ResolvedHttpMode,
) -> anyhow::Result<TlsServerConfig> {
    ensure_rustls_crypto_provider();
    let cert_chain = load_cert_chain(&cfg.tls_cert_path)?;
    let private_key = load_private_key(&cfg.tls_key_path)?;

    let mut builder = TlsServerConfig::new().with_single_cert(rama::tls::server::ServerAuthData {
        private_key,
        cert_chain,
        ocsp: None,
    });

    builder = match http_mode {
        ResolvedHttpMode::Auto => builder.with_alpn_http_auto(),
        ResolvedHttpMode::Http1 => builder.with_alpn_http_1(),
        ResolvedHttpMode::Http2 => builder.with_alpn_http_2(),
    };

    if cfg.emit_keylog {
        builder = builder.with_keylog(rama::tls::KeyLogIntent::Environment)
    }

    Ok(builder.to_owned())
}

pub fn build_upstream_tls(
    cfg: &RuntimeConfig,
    http_mode: ResolvedHttpMode,
) -> anyhow::Result<TlsClientConfig> {
    ensure_rustls_crypto_provider();
    let mut client_config = TlsClientConfig::new() // .withbuilder_with_protocol_versions(ALL_VERSIONS)        .
        .try_with_extra_server_trust_anchors(load_root_store(cfg)?)
        .map_err(|err| anyhow::anyhow!("failed to load root cert store: {err}"))?;

    client_config = match http_mode {
        ResolvedHttpMode::Auto => client_config.with_alpn_http_auto(),
        ResolvedHttpMode::Http1 => client_config.with_alpn_http_1(),
        ResolvedHttpMode::Http2 => client_config.with_alpn_http_2(),
    };

    if cfg.emit_keylog {
        client_config.set_keylog(KeyLogIntent::Environment);
    }

    let server_name = Host::try_from(cfg.upstream_sni.as_str())
        .map_err(|err| anyhow::anyhow!("invalid upstream SNI host: {err}"))?;

    Ok(client_config
        .with_server_name(server_name)
        .with_store_server_cert_chain(true))
}

fn ensure_rustls_crypto_provider() {
    static INIT: Once = Once::new();

    INIT.call_once(|| {
        let _ =
            rama::tls::rustls::dep::rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

pub fn frontend_tls_metadata(extensions: &Extensions) -> FrontendTlsMetadata {
    let mut metadata = FrontendTlsMetadata::default();

    if let Some(params) = extensions.get_arc::<NegotiatedTlsParameters>() {
        metadata.version = Some(params.protocol_version.to_string());
        metadata.alpn = params
            .application_layer_protocol
            .as_ref()
            .map(ToString::to_string);
    }

    if let Some(secure) = extensions.get_arc::<SecureTransport>()
        && let Some(client_hello) = secure.client_hello()
    {
        metadata.sni = client_hello.ext_server_name().map(ToString::to_string);
    }

    metadata.ja3 = Ja3::compute(extensions)
        .ok()
        .map(|value| format!("{value:x}"));
    metadata.ja4 = Ja4::compute(extensions).ok().map(|value| value.to_string());

    metadata
}

pub fn backend_tls_metadata(extensions: &Extensions, configured_sni: &str) -> BackendTlsMetadata {
    let mut metadata = BackendTlsMetadata {
        sni: Some(configured_sni.to_owned()),
        ..BackendTlsMetadata::default()
    };

    if let Some(params) = extensions.get_arc::<NegotiatedTlsParameters>() {
        metadata.version = Some(params.protocol_version.to_string());
        metadata.alpn = params
            .application_layer_protocol
            .as_ref()
            .map(ToString::to_string);

        if let Some(chain) = &params.peer_certificate_chain {
            populate_cert_metadata(&mut metadata, chain);
        }
    }

    metadata
}

fn populate_cert_metadata(metadata: &mut BackendTlsMetadata, chain: &Vec<CertificateDer>) {
    let first_cert = chain.into_iter().next();

    let Some(leaf_der) = first_cert else {
        return;
    };

    metadata.cert_leaf_sha256 = Some(hex::encode(Sha256::digest(leaf_der)));

    if let Ok((_, cert)) = x509_parser::certificate::X509Certificate::from_der(leaf_der) {
        metadata.cert_subject = Some(cert.subject().to_string());
        metadata.cert_issuer = Some(cert.issuer().to_string());
        metadata.cert_not_before = Some(cert.validity().not_before.to_datetime());
        metadata.cert_not_after = Some(cert.validity().not_after.to_datetime());
    }
}

fn load_root_store(cfg: &RuntimeConfig) -> anyhow::Result<Vec<CertificateDer<'static>>> {
    let mut certs = Vec::new();

    let native = rustls_native_certs::load_native_certs();
    for error in native.errors {
        tracing::warn!(error = %error, "failed to load a native CA certificate");
    }
    for cert in native.certs {
        certs.push(cert);
    }

    if let Some(path) = &cfg.ca_bundle_path {
        for cert in load_cert_chain(path)? {
            certs.push(cert);
        }
    }

    if certs.is_empty() {
        bail!("no trust roots were loaded for upstream TLS verification");
    }

    Ok(certs)
}

fn load_cert_chain(path: &std::path::Path) -> anyhow::Result<Vec<CertificateDer<'static>>> {
    let file =
        File::open(path).with_context(|| format!("open PEM cert file {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let certs = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("read PEM cert chain from {}", path.display()))?;

    if certs.is_empty() {
        bail!("no certificates found in {}", path.display());
    }

    Ok(certs)
}

fn load_private_key(path: &std::path::Path) -> anyhow::Result<PrivateKeyDer<'static>> {
    let file = File::open(path).with_context(|| format!("open PEM key file {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let key = rustls_pemfile::private_key(&mut reader)
        .with_context(|| format!("read PEM private key from {}", path.display()))?;

    key.ok_or_else(|| anyhow::anyhow!("no private key found in {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::HttpMode;
    use crate::config::{HeaderLogPolicy, RuntimeConfig};
    use crate::transform::TransformConfig;
    use anyhow::Context;
    use rcgen::generate_simple_self_signed;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Once;
    use tempfile::TempDir;
    use url::Url;

    #[test]
    fn builds_frontend_and_upstream_rustls_configs() -> anyhow::Result<()> {
        init_test_crypto_provider();

        let tempdir = TempDir::new().context("create tempdir")?;
        let certified =
            generate_simple_self_signed(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
                .context("generate rustls test cert")?;
        let cert_path = tempdir.path().join("cert.pem");
        let key_path = tempdir.path().join("key.pem");
        std::fs::write(&cert_path, certified.cert.pem()).context("write cert pem")?;
        std::fs::write(&key_path, certified.signing_key.serialize_pem())
            .context("write key pem")?;

        let cfg = RuntimeConfig {
            listen_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8443),
            frontend_domain: "example.test".to_owned(),
            backend_url: Url::parse("https://127.0.0.1:9443").context("parse backend url")?,
            backend_host: "127.0.0.1".to_owned(),
            backend_port: 9443,
            backend_authority: "127.0.0.1:9443".to_owned(),
            backend_path_prefix: "/".to_owned(),
            tls_cert_path: cert_path.clone(),
            tls_key_path: key_path.clone(),
            sqlite_path: tempdir.path().join("events.sqlite"),
            ca_bundle_path: Some(cert_path),
            upstream_sni: "127.0.0.1".to_owned(),
            http_mode: HttpMode::Http1,
            flush_rows: 1,
            flush_interval: std::time::Duration::from_millis(1),
            max_inflight_events: 1,
            body_max_bytes: 32,
            connect_timeout: std::time::Duration::from_millis(1),
            request_timeout: std::time::Duration::from_millis(1),
            idle_pool_timeout: std::time::Duration::from_millis(1),
            graceful_shutdown_timeout: std::time::Duration::from_millis(1),
            trust_proxy_headers: false,
            emit_keylog: false,
            header_log_policy: HeaderLogPolicy::default(),
            transforms: TransformConfig::default(),
        };

        build_frontend_tls(&cfg, ResolvedHttpMode::Http1)
            .expect("Failed to build http1 frontend TLS");
        build_upstream_tls(&cfg, ResolvedHttpMode::Http1)
            .expect("Failed to build http1 upstream TLS");

        Ok(())
    }

    fn init_test_crypto_provider() {
        static INIT: Once = Once::new();

        INIT.call_once(|| {
            let _ = rama::tls::rustls::dep::rustls::crypto::aws_lc_rs::default_provider()
                .install_default();
        });
    }
}
