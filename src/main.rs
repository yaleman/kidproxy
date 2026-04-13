#![deny(warnings)]
#![warn(unused_extern_crates)]
#![deny(clippy::todo)]
#![deny(clippy::unimplemented)]
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![deny(clippy::panic)]
#![deny(clippy::unreachable)]
#![deny(clippy::await_holding_lock)]
#![deny(clippy::needless_pass_by_value)]
#![deny(clippy::trivially_copy_pass_by_ref)]

use anyhow::Context;
use clap::Parser;
use std::net::SocketAddr;
use tokio::signal;
use tower_http::services::ServeDir;
use tracing::info;

use kidproxy::admin::{self, AdminState};
use kidproxy::cli::Cli;
use kidproxy::runtime_manager::RuntimeManager;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let cli = Cli::parse();
    let admin_listen_addr: SocketAddr = cli
        .admin_listen_addr
        .parse()
        .context("invalid admin listen address")?;
    let runtime_manager = RuntimeManager::start(cli.config_path.clone())
        .await
        .context("failed to start runtime manager")?;

    info!(
        config_path = %cli.config_path.display(),
        admin_listen_addr = %admin_listen_addr,
        "starting admin server"
    );

    let app = admin::router(AdminState::new(runtime_manager.clone()))
        .nest_service("/static", ServeDir::new("static"));
    let listener = tokio::net::TcpListener::bind(admin_listen_addr)
        .await
        .context("bind admin listener")?;

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = signal::ctrl_c().await;
        })
        .await
        .context("admin server failed")?;

    runtime_manager
        .shutdown()
        .await
        .context("runtime manager shutdown failed")?;

    Ok(())
}

fn init_tracing() {
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_owned());

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .compact()
        .init();
}
