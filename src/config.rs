use crate::cli::HttpMode;
use crate::transform::{TransformConfig, TransformRuleFile};
use anyhow::{Context, bail};
use rama::http::{Uri, Version};
use rama::net::address::Domain;
use serde::{Deserialize, Serialize};
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppConfigFile {
    pub runtime: RuntimeSettingsFile,
    #[serde(default)]
    pub transforms: Vec<TransformRuleFile>,
}

impl AppConfigFile {
    pub fn load_json(path: &Path) -> anyhow::Result<Self> {
        let contents =
            fs::read_to_string(path).with_context(|| format!("read config {}", path.display()))?;
        serde_json::from_str(&contents).with_context(|| format!("parse config {}", path.display()))
    }

    pub fn write_json(&self, path: &Path) -> anyhow::Result<()> {
        let contents = serde_json::to_string_pretty(self).context("serialize config")?;
        fs::write(path, contents).with_context(|| format!("write config {}", path.display()))
    }

    pub fn to_runtime_config(&self) -> anyhow::Result<RuntimeConfig> {
        self.runtime.to_runtime_config(&self.transforms)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSettingsFile {
    pub listen_addr: String,
    pub frontend_domain: String,
    pub backend_url: String,
    pub tls_cert_path: PathBuf,
    pub tls_key_path: PathBuf,
    #[serde(default = "default_sqlite_path")]
    pub sqlite_path: PathBuf,
    #[serde(default)]
    pub ca_bundle_path: Option<PathBuf>,
    #[serde(default)]
    pub upstream_sni_override: Option<String>,
    #[serde(default = "default_http_mode")]
    pub http_mode: HttpMode,
    #[serde(default = "default_flush_rows")]
    pub flush_rows: usize,
    #[serde(default = "default_flush_interval_ms")]
    pub flush_interval_ms: u64,
    #[serde(default = "default_max_inflight_events")]
    pub max_inflight_events: usize,
    #[serde(default = "default_body_max_bytes")]
    pub body_max_bytes: usize,
    #[serde(default = "default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,
    #[serde(default = "default_request_timeout_ms")]
    pub request_timeout_ms: u64,
    #[serde(default = "default_idle_pool_timeout_ms")]
    pub idle_pool_timeout_ms: u64,
    #[serde(default = "default_graceful_shutdown_timeout_ms")]
    pub graceful_shutdown_timeout_ms: u64,
    #[serde(default)]
    pub trust_proxy_headers: bool,
    #[serde(default)]
    pub emit_keylog: bool,
    #[serde(default)]
    pub header_allowlist: Vec<String>,
    #[serde(default)]
    pub header_denylist: Vec<String>,
}

impl Default for RuntimeSettingsFile {
    fn default() -> Self {
        Self {
            listen_addr: "127.0.0.1:8443".to_owned(),
            frontend_domain: "example.test".to_owned(),
            backend_url: "https://backend.example".to_owned(),
            tls_cert_path: PathBuf::from("./certs/fullchain.pem"),
            tls_key_path: PathBuf::from("./certs/privkey.pem"),
            sqlite_path: default_sqlite_path(),
            ca_bundle_path: None,
            upstream_sni_override: None,
            http_mode: default_http_mode(),
            flush_rows: default_flush_rows(),
            flush_interval_ms: default_flush_interval_ms(),
            max_inflight_events: default_max_inflight_events(),
            body_max_bytes: default_body_max_bytes(),
            connect_timeout_ms: default_connect_timeout_ms(),
            request_timeout_ms: default_request_timeout_ms(),
            idle_pool_timeout_ms: default_idle_pool_timeout_ms(),
            graceful_shutdown_timeout_ms: default_graceful_shutdown_timeout_ms(),
            trust_proxy_headers: false,
            emit_keylog: false,
            header_allowlist: Vec::new(),
            header_denylist: Vec::new(),
        }
    }
}

impl RuntimeSettingsFile {
    pub fn to_runtime_config(
        &self,
        transform_rules: &[TransformRuleFile],
    ) -> anyhow::Result<RuntimeConfig> {
        let listen_addr: SocketAddr = self.listen_addr.parse().context("invalid listen address")?;
        let frontend_domain = normalize_domain(&self.frontend_domain)?;
        let backend_url = Url::parse(&self.backend_url).context("invalid backend URL")?;

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
        let upstream_sni = match &self.upstream_sni_override {
            Some(value) => normalize_domain(value)?,
            None => backend_host.clone(),
        };

        ensure_readable_file(&self.tls_cert_path, "TLS cert")?;
        ensure_readable_file(&self.tls_key_path, "TLS key")?;
        if let Some(path) = &self.ca_bundle_path {
            ensure_readable_file(path, "CA bundle")?;
        }

        if let Some(parent) = self.sqlite_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        validate_positive("flush rows", self.flush_rows)?;
        validate_positive("flush interval", self.flush_interval_ms)?;
        validate_positive("max inflight events", self.max_inflight_events)?;
        validate_positive("body max bytes", self.body_max_bytes)?;
        validate_positive("connect timeout", self.connect_timeout_ms)?;
        validate_positive("request timeout", self.request_timeout_ms)?;
        validate_positive("idle pool timeout", self.idle_pool_timeout_ms)?;
        validate_positive(
            "graceful shutdown timeout",
            self.graceful_shutdown_timeout_ms,
        )?;

        Ok(RuntimeConfig {
            listen_addr,
            frontend_domain,
            backend_url,
            backend_host,
            backend_port,
            backend_authority,
            backend_path_prefix,
            tls_cert_path: self.tls_cert_path.clone(),
            tls_key_path: self.tls_key_path.clone(),
            sqlite_path: self.sqlite_path.clone(),
            ca_bundle_path: self.ca_bundle_path.clone(),
            upstream_sni,
            http_mode: self.http_mode,
            flush_rows: self.flush_rows,
            flush_interval: Duration::from_millis(self.flush_interval_ms),
            max_inflight_events: self.max_inflight_events,
            body_max_bytes: self.body_max_bytes,
            connect_timeout: Duration::from_millis(self.connect_timeout_ms),
            request_timeout: Duration::from_millis(self.request_timeout_ms),
            idle_pool_timeout: Duration::from_millis(self.idle_pool_timeout_ms),
            graceful_shutdown_timeout: Duration::from_millis(self.graceful_shutdown_timeout_ms),
            trust_proxy_headers: self.trust_proxy_headers,
            emit_keylog: self.emit_keylog,
            header_log_policy: HeaderLogPolicy {
                allowlist: normalize_header_names(self.header_allowlist.clone()),
                denylist: normalize_header_names(self.header_denylist.clone()),
            },
            transforms: TransformConfig::compile(transform_rules)?,
        })
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
    pub sqlite_path: PathBuf,
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
    pub transforms: TransformConfig,
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

fn default_sqlite_path() -> PathBuf {
    PathBuf::from("./data/kidproxy.sqlite")
}

const fn default_http_mode() -> HttpMode {
    HttpMode::Auto
}

const fn default_flush_rows() -> usize {
    5_000
}

const fn default_flush_interval_ms() -> u64 {
    2_000
}

const fn default_max_inflight_events() -> usize {
    10_000
}

const fn default_body_max_bytes() -> usize {
    65_536
}

const fn default_connect_timeout_ms() -> u64 {
    5_000
}

const fn default_request_timeout_ms() -> u64 {
    180_000
}

const fn default_idle_pool_timeout_ms() -> u64 {
    90_000
}

const fn default_graceful_shutdown_timeout_ms() -> u64 {
    10_000
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;
    use rcgen::generate_simple_self_signed;
    use tempfile::TempDir;

    fn test_runtime(tempdir: &TempDir) -> anyhow::Result<RuntimeSettingsFile> {
        let cert = generate_simple_self_signed(vec!["localhost".to_owned()])
            .context("generate test cert")?;
        let cert_path = tempdir.path().join("cert.pem");
        let key_path = tempdir.path().join("key.pem");
        std::fs::write(&cert_path, cert.cert.pem()).context("write cert pem")?;
        std::fs::write(&key_path, cert.signing_key.serialize_pem()).context("write key pem")?;

        Ok(RuntimeSettingsFile {
            listen_addr: "127.0.0.1:8443".to_owned(),
            frontend_domain: "Example.TEST".to_owned(),
            backend_url: "https://backend.example/base".to_owned(),
            tls_cert_path: cert_path,
            tls_key_path: key_path,
            sqlite_path: tempdir.path().join("events.sqlite"),
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
            header_allowlist: vec!["X-Test".to_owned()],
            header_denylist: vec!["X-Drop".to_owned()],
        })
    }

    #[test]
    fn validates_https_backend_and_builds_backend_uri() -> anyhow::Result<()> {
        let tempdir = TempDir::new().context("create tempdir")?;
        let config = AppConfigFile {
            runtime: test_runtime(&tempdir)?,
            transforms: Vec::new(),
        }
        .to_runtime_config()?;

        assert_eq!(config.frontend_domain, "example.test");
        assert_eq!(config.backend_host, "backend.example");
        assert!(config.header_log_policy.allows("x-test"));
        assert!(!config.header_log_policy.allows("x-drop"));

        let uri = config.build_backend_uri("/hello", Some("a=1"))?;
        assert_eq!(uri.to_string(), "https://backend.example/base/hello?a=1");

        Ok(())
    }

    #[test]
    fn rejects_non_https_backend_urls() -> anyhow::Result<()> {
        let tempdir = TempDir::new().context("create tempdir")?;
        let mut runtime = test_runtime(&tempdir)?;
        runtime.backend_url = "http://backend.example".to_owned();

        let result = AppConfigFile {
            runtime,
            transforms: Vec::new(),
        }
        .to_runtime_config();

        assert!(result.is_err());
        if let Err(err) = result {
            assert!(err.to_string().contains("backend URL must use https"));
        }
        Ok(())
    }

    #[test]
    fn round_trips_config_json() -> anyhow::Result<()> {
        let tempdir = TempDir::new().context("create tempdir")?;
        let config = AppConfigFile {
            runtime: test_runtime(&tempdir)?,
            transforms: Vec::new(),
        };
        let path = tempdir.path().join("kidproxy.json");

        config.write_json(&path)?;
        let reloaded = AppConfigFile::load_json(&path)?;

        assert_eq!(config, reloaded);
        Ok(())
    }
}
