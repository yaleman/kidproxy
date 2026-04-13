use crate::config::{AppConfigFile, RuntimeConfig};
use crate::probe::{self, BackendProbe};
use crate::proxy::{ProxyApp, ProxyHandle};
use crate::writer::{SqliteWriterHandle, spawn_writer};
use anyhow::Context;
use sea_orm::DatabaseConnection;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tracing::{info, warn};

#[derive(Clone)]
pub struct RuntimeManager {
    controller: Arc<Mutex<RuntimeController>>,
    snapshot: Arc<RwLock<RuntimeManagerState>>,
}

struct RuntimeController {
    config_path: PathBuf,
    active: Option<ActiveRuntime>,
}

struct ActiveRuntime {
    app_config: AppConfigFile,
    runtime: RuntimeConfig,
    writer: SqliteWriterHandle,
    handle: ProxyHandle,
}

#[derive(Debug, Clone)]
pub struct RuntimeManagerState {
    pub config_path: PathBuf,
    pub saved_config: AppConfigFile,
    pub active_runtime: RuntimeSummary,
    pub pending_reload: bool,
    pub last_reload_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RuntimeSummary {
    pub listen_addr: String,
    pub frontend_domain: String,
    pub backend_url: String,
    pub sqlite_path: String,
    pub transform_count: usize,
    pub http_mode: String,
    pub dropped_events: u64,
    pub write_failures: u64,
}

impl RuntimeManager {
    pub async fn start(config_path: PathBuf) -> anyhow::Result<Self> {
        let app_config = AppConfigFile::load_json(&config_path)?;
        let runtime = app_config.to_runtime_config()?;
        let active = start_active_runtime(app_config.clone(), runtime).await?;
        let snapshot = RuntimeManagerState {
            config_path: config_path.clone(),
            saved_config: app_config,
            active_runtime: runtime_summary(&active),
            pending_reload: false,
            last_reload_error: None,
        };

        Ok(Self {
            controller: Arc::new(Mutex::new(RuntimeController {
                config_path,
                active: Some(active),
            })),
            snapshot: Arc::new(RwLock::new(snapshot)),
        })
    }

    pub async fn state(self) -> RuntimeManagerState {
        let RuntimeManager { snapshot, .. } = self;
        match snapshot.read() {
            Ok(state) => state.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    pub async fn active_database(self) -> anyhow::Result<DatabaseConnection> {
        let RuntimeManager { controller, .. } = self;
        let controller = controller
            .lock()
            .map_err(|_| anyhow::anyhow!("runtime manager controller lock poisoned"))?;
        controller
            .active
            .as_ref()
            .map(|active| active.writer.database())
            .context("runtime manager missing active runtime")
    }

    pub async fn save_config(
        self,
        app_config: AppConfigFile,
    ) -> anyhow::Result<RuntimeManagerState> {
        let RuntimeManager {
            controller: controller_handle,
            snapshot: snapshot_handle,
        } = self;
        app_config.to_runtime_config()?;

        let (config_path, active_matches) = {
            let controller = controller_handle
                .lock()
                .map_err(|_| anyhow::anyhow!("runtime manager controller lock poisoned"))?;
            (
                controller.config_path.clone(),
                controller
                    .active
                    .as_ref()
                    .map(|active| active.app_config == app_config)
                    .unwrap_or(false),
            )
        };

        app_config.write_json(&config_path)?;

        {
            let mut snapshot = snapshot_handle
                .write()
                .map_err(|_| anyhow::anyhow!("runtime manager snapshot lock poisoned"))?;
            snapshot.saved_config = app_config;
            snapshot.pending_reload = !active_matches;
            snapshot.last_reload_error = None;
        }

        snapshot_handle
            .read()
            .map(|state| state.clone())
            .map_err(|_| anyhow::anyhow!("runtime manager snapshot lock poisoned"))
    }

    pub async fn reload(self) -> anyhow::Result<RuntimeManagerState> {
        let handle = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || handle.block_on(self.reload_inner()))
            .await
            .map_err(|err| anyhow::anyhow!("runtime reload task join failure: {err}"))?
    }

    async fn reload_inner(self) -> anyhow::Result<RuntimeManagerState> {
        let RuntimeManager {
            controller: controller_handle,
            snapshot: snapshot_handle,
        } = self;
        let (config_path, previous) = {
            let mut controller = controller_handle
                .lock()
                .map_err(|_| anyhow::anyhow!("runtime manager controller lock poisoned"))?;
            let previous = controller
                .active
                .take()
                .context("runtime manager missing active runtime during reload")?;
            (controller.config_path.clone(), previous)
        };

        let saved_config = AppConfigFile::load_json(&config_path)?;
        let new_runtime = saved_config.to_runtime_config()?;
        let previous_config = previous.app_config.clone();
        let previous_runtime = previous.runtime.clone();

        if let Err(err) = shutdown_active_runtime(previous).await {
            warn!(error = %err, "failed to fully stop previous runtime before reload");
        }
        wait_for_listener_release(previous_runtime.listen_addr).await?;

        match start_active_runtime(saved_config.clone(), new_runtime).await {
            Ok(active) => {
                {
                    let mut controller = controller_handle
                        .lock()
                        .map_err(|_| anyhow::anyhow!("runtime manager controller lock poisoned"))?;
                    controller.active = Some(active);
                }

                let active_runtime = {
                    let controller = controller_handle
                        .lock()
                        .map_err(|_| anyhow::anyhow!("runtime manager controller lock poisoned"))?;
                    controller
                        .active
                        .as_ref()
                        .map(runtime_summary)
                        .context("runtime manager missing active runtime after reload")?
                };
                let mut snapshot = snapshot_handle
                    .write()
                    .map_err(|_| anyhow::anyhow!("runtime manager snapshot lock poisoned"))?;
                snapshot.saved_config = saved_config;
                snapshot.active_runtime = active_runtime;
                snapshot.pending_reload = false;
                snapshot.last_reload_error = None;
                Ok(snapshot.clone())
            }
            Err(err) => {
                let restart_previous =
                    start_active_runtime(previous_config, previous_runtime).await;
                match restart_previous {
                    Ok(active) => {
                        let mut controller = controller_handle.lock().map_err(|_| {
                            anyhow::anyhow!("runtime manager controller lock poisoned")
                        })?;
                        controller.active = Some(active);
                    }
                    Err(restart_err) => {
                        let message =
                            format!("reload failed: {err}; fallback restart failed: {restart_err}");
                        let mut snapshot = snapshot_handle.write().map_err(|_| {
                            anyhow::anyhow!("runtime manager snapshot lock poisoned")
                        })?;
                        snapshot.last_reload_error = Some(message.clone());
                        return Err(anyhow::anyhow!(message));
                    }
                }

                let mut snapshot = snapshot_handle
                    .write()
                    .map_err(|_| anyhow::anyhow!("runtime manager snapshot lock poisoned"))?;
                snapshot.last_reload_error = Some(err.to_string());
                snapshot.pending_reload = true;
                Err(err)
            }
        }
    }

    pub async fn shutdown(self) -> anyhow::Result<()> {
        let RuntimeManager {
            controller: controller_handle,
            ..
        } = self;
        let active = {
            let mut controller = controller_handle
                .lock()
                .map_err(|_| anyhow::anyhow!("runtime manager controller lock poisoned"))?;
            controller
                .active
                .take()
                .context("runtime manager missing active runtime during shutdown")?
        };
        shutdown_active_runtime(active).await
    }
}

async fn start_active_runtime(
    app_config: AppConfigFile,
    runtime: RuntimeConfig,
) -> anyhow::Result<ActiveRuntime> {
    let probe = match probe::probe_backend(&runtime).await {
        Ok(probe) => {
            info!(?probe, "backend probe complete");
            probe
        }
        Err(err) => {
            warn!(error = %err, "backend probe failed, continuing with defaults");
            BackendProbe::fallback(&runtime)
        }
    };

    let writer = spawn_writer(&runtime)
        .await
        .context("failed to start SQLite writer")?;
    let proxy = ProxyApp::new(runtime.clone(), writer.clone(), probe)
        .await
        .context("failed to initialize proxy app")?;
    let handle = proxy.start().await.context("failed to start proxy app")?;

    Ok(ActiveRuntime {
        app_config,
        runtime,
        writer,
        handle,
    })
}

async fn shutdown_active_runtime(active: ActiveRuntime) -> anyhow::Result<()> {
    active.handle.shutdown().await?;
    active.writer.shutdown().await
}

fn runtime_summary(active: &ActiveRuntime) -> RuntimeSummary {
    RuntimeSummary {
        listen_addr: active.runtime.listen_addr.to_string(),
        frontend_domain: active.runtime.frontend_domain.clone(),
        backend_url: active.runtime.backend_url.to_string(),
        sqlite_path: display_path(&active.runtime.sqlite_path),
        transform_count: active.app_config.transforms.len(),
        http_mode: format!("{:?}", active.runtime.http_mode).to_ascii_lowercase(),
        dropped_events: active.writer.dropped_events(),
        write_failures: active.writer.write_failures(),
    }
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

async fn wait_for_listener_release(addr: std::net::SocketAddr) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match std::net::TcpListener::bind(addr) {
            Ok(listener) => {
                drop(listener);
                return Ok(());
            }
            Err(err) => {
                if Instant::now() >= deadline {
                    return Err(anyhow::anyhow!(
                        "listener {} did not shut down cleanly: {}",
                        addr,
                        err
                    ));
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
    }
}
