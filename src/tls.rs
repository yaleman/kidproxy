use crate::config::{ResolvedHttpMode, RuntimeConfig};
use anyhow::{Context, bail};
use rama::extensions::Extensions;
use rama::net::address::Host;
use rama::net::tls::{
    ApplicationProtocol, DataEncoding, KeyLogIntent, SecureTransport,
    client::NegotiatedTlsParameters,
};
use rama::tls::rustls::client::{TlsConnectorData, TlsConnectorDataBuilder};
use rama::tls::rustls::dep::pki_types::{CertificateDer, PrivateKeyDer};
use rama::tls::rustls::dep::rustls::{ALL_VERSIONS, ClientConfig, RootCertStore};
use rama::tls::rustls::key_log::KeyLogFile;
use rama::tls::rustls::server::{TlsAcceptorData, TlsAcceptorDataBuilder};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;
use time::OffsetDateTime;
use x509_parser::prelude::FromDer;

#[derive(Debug, Clone)]
pub struct FrontendTlsConfig {
    pub acceptor_data: TlsAcceptorData,
}

#[derive(Debug, Clone)]
pub struct UpstreamTlsConfig {
    pub connector_data: TlsConnectorData,
}

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
) -> anyhow::Result<FrontendTlsConfig> {
    let cert_chain = load_cert_chain(&cfg.tls_cert_path)?;
    let private_key = load_private_key(&cfg.tls_key_path)?;

    let mut builder = TlsAcceptorDataBuilder::new(cert_chain, private_key)
        .context("create frontend rustls acceptor")?;

    builder = match http_mode {
        ResolvedHttpMode::Auto => builder.with_alpn_protocols_http_auto(),
        ResolvedHttpMode::Http1 => builder.with_alpn_protocols(&[ApplicationProtocol::HTTP_11]),
        ResolvedHttpMode::Http2 => builder.with_alpn_protocols(&[ApplicationProtocol::HTTP_2]),
    };

    if cfg.emit_keylog {
        builder = builder
            .try_with_env_key_logger()
            .context("enable frontend key logger")?;
    }

    Ok(FrontendTlsConfig {
        acceptor_data: builder.build(),
    })
}

pub fn build_upstream_tls(
    cfg: &RuntimeConfig,
    http_mode: ResolvedHttpMode,
) -> anyhow::Result<UpstreamTlsConfig> {
    let mut client_config = ClientConfig::builder_with_protocol_versions(ALL_VERSIONS)
        .with_root_certificates(load_root_store(cfg)?)
        .with_no_client_auth();

    client_config.alpn_protocols = match http_mode {
        ResolvedHttpMode::Auto => vec![
            ApplicationProtocol::HTTP_2.as_bytes().to_vec(),
            ApplicationProtocol::HTTP_11.as_bytes().to_vec(),
        ],
        ResolvedHttpMode::Http1 => vec![ApplicationProtocol::HTTP_11.as_bytes().to_vec()],
        ResolvedHttpMode::Http2 => vec![ApplicationProtocol::HTTP_2.as_bytes().to_vec()],
    };

    if cfg.emit_keylog {
        if let Some(path) = KeyLogIntent::Environment.file_path() {
            client_config.key_log =
                Arc::new(KeyLogFile::try_new(path.as_ref()).context("enable upstream key logger")?);
        }
    }

    let connector_data = TlsConnectorDataBuilder::from(client_config)
        .with_server_name(
            Host::try_from(cfg.upstream_sni.as_str())
                .map_err(|err| anyhow::anyhow!("invalid upstream SNI host: {err}"))?,
        )
        .with_store_server_certificate_chain(true)
        .build();

    Ok(UpstreamTlsConfig { connector_data })
}

pub fn frontend_tls_metadata(extensions: &Extensions) -> FrontendTlsMetadata {
    let mut metadata = FrontendTlsMetadata::default();

    if let Some(params) = extensions.get::<NegotiatedTlsParameters>() {
        metadata.version = Some(params.protocol_version.to_string());
        metadata.alpn = params
            .application_layer_protocol
            .as_ref()
            .map(ToString::to_string);
    }

    if let Some(secure) = extensions.get::<SecureTransport>()
        && let Some(client_hello) = secure.client_hello()
    {
        metadata.sni = client_hello.ext_server_name().map(ToString::to_string);
    }

    metadata.ja3 = rama::net::fingerprint::Ja3::compute(extensions)
        .ok()
        .map(|value| format!("{value:x}"));
    metadata.ja4 = rama::net::fingerprint::Ja4::compute(extensions)
        .ok()
        .map(|value| value.to_string());

    metadata
}

pub fn backend_tls_metadata(extensions: &Extensions, configured_sni: &str) -> BackendTlsMetadata {
    let mut metadata = BackendTlsMetadata {
        sni: Some(configured_sni.to_owned()),
        ..BackendTlsMetadata::default()
    };

    if let Some(params) = extensions.get::<NegotiatedTlsParameters>() {
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

fn populate_cert_metadata(metadata: &mut BackendTlsMetadata, chain: &DataEncoding) {
    let first_cert = match chain {
        DataEncoding::DerStack(chain) => chain.first(),
        DataEncoding::Der(der) => Some(der),
        DataEncoding::Pem(_) => None,
    };

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

fn load_root_store(cfg: &RuntimeConfig) -> anyhow::Result<RootCertStore> {
    let mut store = RootCertStore::empty();

    let native = rustls_native_certs::load_native_certs();
    for error in native.errors {
        tracing::warn!(error = %error, "failed to load a native CA certificate");
    }
    for cert in native.certs {
        let _ = store.add(cert);
    }

    if let Some(path) = &cfg.ca_bundle_path {
        for cert in load_cert_chain(path)? {
            store
                .add(cert)
                .map_err(|err| anyhow::anyhow!("failed to add CA certificate: {err}"))?;
        }
    }

    if store.is_empty() {
        bail!("no trust roots were loaded for upstream TLS verification");
    }

    Ok(store)
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
            parquet_dir: tempdir.path().join("parquet"),
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
            rollover_minutes: 1,
        };

        let frontend = build_frontend_tls(&cfg, ResolvedHttpMode::Http1)?;
        let upstream = build_upstream_tls(&cfg, ResolvedHttpMode::Http1)?;

        let _ = frontend.acceptor_data;
        let _ = upstream.connector_data;

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
