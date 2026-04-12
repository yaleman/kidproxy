use crate::cli::{Cli, HttpMode};
use anyhow::{Context, bail};
use rama::http::{Uri, Version};
use rama::net::address::Domain;
use std::collections::HashSet;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedHttpMode {
    Auto,
    Http1,
    Http2,
}

#[derive(Debug, Clone, Default)]
pub struct HeaderLogPolicy {
    allowlist: HashSet<String>,
    denylist: HashSet<String>,
}

impl HeaderLogPolicy {
    pub fn allows(&self, header_name: &str) -> bool {
        let normalized = header_name.to_ascii_lowercase();
        if !self.allowlist.is_empty() {
            return self.allowlist.contains(&normalized);
        }
        !self.denylist.contains(&normalized)
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub listen_addr: SocketAddr,
    pub frontend_domain: String,
    pub backend_url: Url,
    pub backend_host: String,
    pub backend_port: u16,
    pub backend_authority: String,
    pub backend_path_prefix: String,
    pub tls_cert_path: PathBuf,
    pub tls_key_path: PathBuf,
    pub parquet_dir: PathBuf,
    pub ca_bundle_path: Option<PathBuf>,
    pub upstream_sni: String,
    pub http_mode: HttpMode,
    pub flush_rows: usize,
    pub flush_interval: Duration,
    pub max_inflight_events: usize,
    pub body_max_bytes: usize,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub idle_pool_timeout: Duration,
    pub graceful_shutdown_timeout: Duration,
    pub trust_proxy_headers: bool,
    pub emit_keylog: bool,
    pub header_log_policy: HeaderLogPolicy,
    pub rollover_minutes: u64,
}

impl RuntimeConfig {
    pub fn resolved_http_mode(&self, backend_supports_h2: Option<bool>) -> ResolvedHttpMode {
        match self.http_mode {
            HttpMode::Http1 => ResolvedHttpMode::Http1,
            HttpMode::Http2 => ResolvedHttpMode::Http2,
            HttpMode::Auto => match backend_supports_h2 {
                Some(false) => ResolvedHttpMode::Http1,
                _ => ResolvedHttpMode::Auto,
            },
        }
    }

    pub fn upstream_default_version(&self) -> Version {
        Version::HTTP_11
    }

    pub fn build_backend_uri(&self, path: &str, query: Option<&str>) -> anyhow::Result<Uri> {
        let mut url = self.backend_url.clone();
        let combined_path = if self.backend_path_prefix == "/" {
            normalize_request_path(path)
        } else {
            format!(
                "{}{}",
                self.backend_path_prefix.trim_end_matches('/'),
                normalize_request_path(path)
            )
        };

        url.set_path(&combined_path);
        url.set_query(query);
        url.set_fragment(None);

        url.as_str()
            .parse()
            .with_context(|| format!("failed to build upstream URI from {}", url))
    }
}

impl TryFrom<Cli> for RuntimeConfig {
    type Error = anyhow::Error;

    fn try_from(cli: Cli) -> Result<Self, Self::Error> {
        let listen_addr: SocketAddr = cli.listen_addr.parse().context("invalid listen address")?;
        let frontend_domain = normalize_domain(&cli.frontend_domain)?;
        let backend_url = Url::parse(&cli.backend_url).context("invalid backend URL")?;

        if backend_url.scheme() != "https" {
            bail!("backend URL must use https");
        }

        let backend_host = backend_url
            .host_str()
            .context("backend URL must include a host")?
            .to_ascii_lowercase();
        let backend_port = backend_url
            .port_or_known_default()
            .context("backend URL must include a known HTTPS port")?;
        let backend_authority = match backend_url.port() {
            Some(port) => format!("{backend_host}:{port}"),
            None => backend_host.clone(),
        };
        let backend_path_prefix = normalize_backend_prefix(backend_url.path());
        let upstream_sni = match cli.upstream_sni_override {
            Some(value) => normalize_domain(&value)?,
            None => backend_host.clone(),
        };

        ensure_readable_file(&cli.tls_cert_path, "TLS cert")?;
        ensure_readable_file(&cli.tls_key_path, "TLS key")?;
        if let Some(path) = &cli.ca_bundle_path {
            ensure_readable_file(path, "CA bundle")?;
        }

        fs::create_dir_all(&cli.parquet_dir)
            .with_context(|| format!("failed to create {}", cli.parquet_dir.display()))?;
        if !cli.parquet_dir.is_dir() {
            bail!(
                "parquet dir is not a directory: {}",
                cli.parquet_dir.display()
            );
        }

        validate_positive("flush rows", cli.flush_rows)?;
        validate_positive("flush interval", cli.flush_interval_ms)?;
        validate_positive("max inflight events", cli.max_inflight_events)?;
        validate_positive("body max bytes", cli.body_max_bytes)?;
        validate_positive("connect timeout", cli.connect_timeout_ms)?;
        validate_positive("request timeout", cli.request_timeout_ms)?;
        validate_positive("idle pool timeout", cli.idle_pool_timeout_ms)?;
        validate_positive(
            "graceful shutdown timeout",
            cli.graceful_shutdown_timeout_ms,
        )?;
        validate_positive("rollover minutes", cli.rollover_minutes)?;

        Ok(Self {
            listen_addr,
            frontend_domain,
            backend_url,
            backend_host,
            backend_port,
            backend_authority,
            backend_path_prefix,
            tls_cert_path: cli.tls_cert_path,
            tls_key_path: cli.tls_key_path,
            parquet_dir: cli.parquet_dir,
            ca_bundle_path: cli.ca_bundle_path,
            upstream_sni,
            http_mode: cli.http_mode,
            flush_rows: cli.flush_rows,
            flush_interval: Duration::from_millis(cli.flush_interval_ms),
            max_inflight_events: cli.max_inflight_events,
            body_max_bytes: cli.body_max_bytes,
            connect_timeout: Duration::from_millis(cli.connect_timeout_ms),
            request_timeout: Duration::from_millis(cli.request_timeout_ms),
            idle_pool_timeout: Duration::from_millis(cli.idle_pool_timeout_ms),
            graceful_shutdown_timeout: Duration::from_millis(cli.graceful_shutdown_timeout_ms),
            trust_proxy_headers: cli.trust_proxy_headers,
            emit_keylog: cli.emit_keylog,
            header_log_policy: HeaderLogPolicy {
                allowlist: normalize_header_names(cli.header_allowlist),
                denylist: normalize_header_names(cli.header_denylist),
            },
            rollover_minutes: cli.rollover_minutes,
        })
    }
}

fn normalize_domain(raw: &str) -> anyhow::Result<String> {
    let trimmed = raw.trim().trim_end_matches('.');
    if trimmed.is_empty() {
        bail!("domain must not be empty");
    }
    let domain: Domain = trimmed.parse().context("domain must be a valid DNS name")?;
    Ok(domain.to_string().to_ascii_lowercase())
}

fn normalize_header_names(values: Vec<String>) -> HashSet<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

fn ensure_readable_file(path: &Path, label: &str) -> anyhow::Result<()> {
    fs::File::open(path)
        .with_context(|| format!("{label} path is not readable: {}", path.display()))?;
    Ok(())
}

fn validate_positive<T>(label: &str, value: T) -> anyhow::Result<()>
where
    T: PartialEq + Default + Copy,
{
    if value == T::default() {
        bail!("{label} must be greater than zero");
    }
    Ok(())
}

fn normalize_request_path(path: &str) -> String {
    if path.is_empty() {
        "/".to_owned()
    } else if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    }
}

fn normalize_backend_prefix(path: &str) -> String {
    if path.is_empty() || path == "/" {
        "/".to_owned()
    } else {
        normalize_request_path(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, HttpMode};
    use anyhow::Context;
    use rcgen::generate_simple_self_signed;
    use tempfile::TempDir;

    #[test]
    fn validates_https_backend_and_builds_backend_uri() -> anyhow::Result<()> {
        let tempdir = TempDir::new().context("create tempdir")?;
        let cert = generate_simple_self_signed(vec!["localhost".to_owned()])
            .context("generate test cert")?;
        let cert_path = tempdir.path().join("cert.pem");
        let key_path = tempdir.path().join("key.pem");
        std::fs::write(&cert_path, cert.cert.pem()).context("write cert pem")?;
        std::fs::write(&key_path, cert.signing_key.serialize_pem()).context("write key pem")?;

        let config = RuntimeConfig::try_from(Cli {
            listen_addr: "127.0.0.1:8443".to_owned(),
            frontend_domain: "Example.TEST".to_owned(),
            backend_url: "https://backend.example/base".to_owned(),
            tls_cert_path: cert_path.clone(),
            tls_key_path: key_path.clone(),
            parquet_dir: tempdir.path().join("parquet"),
            ca_bundle_path: Some(cert_path),
            upstream_sni_override: None,
            http_mode: HttpMode::Auto,
            flush_rows: 1,
            flush_interval_ms: 1,
            max_inflight_events: 1,
            body_max_bytes: 32,
            connect_timeout_ms: 1,
            request_timeout_ms: 1,
            idle_pool_timeout_ms: 1,
            graceful_shutdown_timeout_ms: 1,
            trust_proxy_headers: false,
            emit_keylog: false,
            header_allowlist: vec!["X-Test".to_owned()],
            header_denylist: vec!["X-Drop".to_owned()],
            rollover_minutes: 1,
        })?;

        assert_eq!(config.frontend_domain, "example.test");
        assert_eq!(config.backend_host, "backend.example");
        assert!(config.header_log_policy.allows("x-test"));
        assert!(!config.header_log_policy.allows("x-drop"));

        let uri = config.build_backend_uri("/hello", Some("a=1"))?;
        assert_eq!(uri.to_string(), "https://backend.example/base/hello?a=1");

        Ok(())
    }

    #[test]
    fn rejects_non_https_backend_urls() {
        let cli = Cli {
            listen_addr: "127.0.0.1:8443".to_owned(),
            frontend_domain: "example.test".to_owned(),
            backend_url: "http://backend.example".to_owned(),
            tls_cert_path: PathBuf::from("missing-cert.pem"),
            tls_key_path: PathBuf::from("missing-key.pem"),
            parquet_dir: PathBuf::from("target/tmp"),
            ca_bundle_path: None,
            upstream_sni_override: None,
            http_mode: HttpMode::Auto,
            flush_rows: 1,
            flush_interval_ms: 1,
            max_inflight_events: 1,
            body_max_bytes: 32,
            connect_timeout_ms: 1,
            request_timeout_ms: 1,
            idle_pool_timeout_ms: 1,
            graceful_shutdown_timeout_ms: 1,
            trust_proxy_headers: false,
            emit_keylog: false,
            header_allowlist: Vec::new(),
            header_denylist: Vec::new(),
            rollover_minutes: 1,
        };

        let result = RuntimeConfig::try_from(cli);
        assert!(result.is_err());
        if let Err(err) = result {
            assert!(err.to_string().contains("backend URL must use https"));
        }
    }
}
