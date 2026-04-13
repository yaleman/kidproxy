use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Parser)]
#[command(name = "kidproxy")]
#[command(about = "Single-backend HTTPS reverse proxy with SQLite logging")]
pub struct Cli {
    #[arg(long, env = "PROXY_CONFIG_PATH", default_value = "./kidproxy.json")]
    pub config_path: PathBuf,

    #[arg(
        long,
        env = "PROXY_ADMIN_LISTEN_ADDR",
        default_value = "127.0.0.1:3000"
    )]
    pub admin_listen_addr: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum HttpMode {
    Auto,
    Http1,
    Http2,
}
