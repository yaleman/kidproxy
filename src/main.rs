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
use tokio::signal;
use tracing::{error, info, warn};

use kidproxy::cli::Cli;
use kidproxy::config::RuntimeConfig;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let cli = Cli::parse();
    let config = RuntimeConfig::try_from(cli).context("failed to build runtime config")?;

    info!(
        listen_addr = %config.listen_addr,
        frontend_domain = %config.frontend_domain,
        backend_url = %config.backend_url,
        parquet_dir = %config.parquet_dir.display(),
        "starting proxy"
    );

    let probe = match kidproxy::probe::probe_backend(&config).await {
        Ok(probe) => {
            info!(?probe, "backend probe complete");
            probe
        }
        Err(err) => {
            warn!(error = %err, "backend probe failed, continuing with defaults");
            kidproxy::probe::BackendProbe::fallback(&config)
        }
    };

    let writer = kidproxy::writer::spawn_writer(&config)
        .await
        .context("failed to start parquet writer")?;
    let proxy = kidproxy::proxy::ProxyApp::new(config.clone(), writer.clone(), probe)
        .await
        .context("failed to initialize proxy app")?;
    let mut handle = Some(proxy.start().await.context("failed to start proxy app")?);

    tokio::select! {
        result = async {
            let handle = handle
                .take()
                .context("proxy handle missing while waiting for completion")?;
            handle.wait().await
        } => {
            match result {
                Ok(()) => info!("proxy exited cleanly"),
                Err(err) => error!(error = %err, "proxy exited with error"),
            }
        }
        _ = signal::ctrl_c() => {
            info!("shutdown signal received");
            if let Some(handle) = handle.take() {
                if let Err(err) = handle.shutdown().await {
                    error!(error = %err, "proxy shutdown failed");
                }
            }
        }
    }

    writer.shutdown().await.context("writer shutdown failed")?;
    info!("shutdown complete");

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
