use clap::{Parser, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Clone, Parser)]
#[command(name = "kidproxy")]
#[command(about = "Single-backend HTTPS reverse proxy with SQLite logging")]
pub struct Cli {
    #[arg(long, env = "PROXY_LISTEN_ADDR")]
    pub listen_addr: String,

    #[arg(long, env = "PROXY_FRONTEND_DOMAIN")]
    pub frontend_domain: String,

    #[arg(long, env = "PROXY_BACKEND_URL")]
    pub backend_url: String,

    #[arg(long, env = "PROXY_TLS_CERT_PATH")]
    pub tls_cert_path: PathBuf,

    #[arg(long, env = "PROXY_TLS_KEY_PATH")]
    pub tls_key_path: PathBuf,

    #[arg(
        long,
        env = "PROXY_SQLITE_PATH",
        default_value = "./data/kidproxy.sqlite"
    )]
    pub sqlite_path: PathBuf,

    #[arg(long, env = "PROXY_CONFIG_PATH")]
    pub config_path: Option<PathBuf>,

    #[arg(long, env = "PROXY_CA_BUNDLE_PATH")]
    pub ca_bundle_path: Option<PathBuf>,

    #[arg(long, env = "PROXY_UPSTREAM_SNI_OVERRIDE")]
    pub upstream_sni_override: Option<String>,

    #[arg(long, env = "PROXY_HTTP_MODE", default_value = "auto")]
    pub http_mode: HttpMode,

    #[arg(long, env = "PROXY_FLUSH_ROWS", default_value_t = 5_000)]
    pub flush_rows: usize,

    #[arg(long, env = "PROXY_FLUSH_INTERVAL_MS", default_value_t = 2_000)]
    pub flush_interval_ms: u64,

    #[arg(long, env = "PROXY_MAX_INFLIGHT_EVENTS", default_value_t = 10_000)]
    pub max_inflight_events: usize,

    #[arg(long, env = "PROXY_BODY_MAX_BYTES", default_value_t = 65_536)]
    pub body_max_bytes: usize,

    #[arg(long, env = "PROXY_CONNECT_TIMEOUT_MS", default_value_t = 5_000)]
    pub connect_timeout_ms: u64,

    #[arg(long, env = "PROXY_REQUEST_TIMEOUT_MS", default_value_t = 180_000)]
    pub request_timeout_ms: u64,

    #[arg(long, env = "PROXY_IDLE_POOL_TIMEOUT_MS", default_value_t = 90_000)]
    pub idle_pool_timeout_ms: u64,

    #[arg(
        long,
        env = "PROXY_GRACEFUL_SHUTDOWN_TIMEOUT_MS",
        default_value_t = 10_000
    )]
    pub graceful_shutdown_timeout_ms: u64,

    #[arg(long, env = "PROXY_TRUST_PROXY_HEADERS", default_value_t = false)]
    pub trust_proxy_headers: bool,

    #[arg(long, env = "PROXY_EMIT_KEYLOG", default_value_t = false)]
    pub emit_keylog: bool,

    #[arg(long, env = "PROXY_HEADER_ALLOWLIST", value_delimiter = ',')]
    pub header_allowlist: Vec<String>,

    #[arg(long, env = "PROXY_HEADER_DENYLIST", value_delimiter = ',')]
    pub header_denylist: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum HttpMode {
    Auto,
    Http1,
    Http2,
}
