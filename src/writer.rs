use crate::config::RuntimeConfig;
use crate::event::ProxyEvent;
use crate::schema::events_to_record_batch;
use anyhow::Context;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use std::fs::{self, File};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use time::OffsetDateTime;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{MissedTickBehavior, interval};
use tracing::{error, info, warn};

#[derive(Clone)]
pub struct ParquetWriterHandle {
    tx: mpsc::Sender<WriterMessage>,
    dropped_events: Arc<AtomicU64>,
    write_failures: Arc<AtomicU64>,
}

enum WriterMessage {
    Event(ProxyEvent),
    Shutdown(oneshot::Sender<()>),
}

pub async fn spawn_writer(cfg: &RuntimeConfig) -> anyhow::Result<ParquetWriterHandle> {
    let (tx, rx) = mpsc::channel(cfg.max_inflight_events);
    let dropped_events = Arc::new(AtomicU64::new(0));
    let write_failures = Arc::new(AtomicU64::new(0));
    let state = WriterState::new(cfg.clone(), dropped_events.clone(), write_failures.clone());

    tokio::spawn(async move {
        if let Err(err) = state.run(rx).await {
            error!(error = %err, "writer task failed");
        }
    });

    Ok(ParquetWriterHandle {
        tx,
        dropped_events,
        write_failures,
    })
}

impl ParquetWriterHandle {
    pub fn try_send(&self, event: ProxyEvent) {
        if self.tx.try_send(WriterMessage::Event(event)).is_err() {
            let dropped = self.dropped_events.fetch_add(1, Ordering::Relaxed) + 1;
            warn!(
                dropped_events = dropped,
                "writer queue full, dropping event"
            );
        }
    }

    pub async fn shutdown(&self) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        let _ = self.tx.send(WriterMessage::Shutdown(tx)).await;
        let _ = rx.await;
        info!(
            dropped_events = self.dropped_events(),
            write_failures = self.write_failures(),
            "writer shutdown complete"
        );
        Ok(())
    }

    pub fn dropped_events(&self) -> u64 {
        self.dropped_events.load(Ordering::Relaxed)
    }

    pub fn write_failures(&self) -> u64 {
        self.write_failures.load(Ordering::Relaxed)
    }
}

struct WriterState {
    cfg: RuntimeConfig,
    buffer: Vec<ProxyEvent>,
    sequence: u64,
    dropped_events: Arc<AtomicU64>,
    write_failures: Arc<AtomicU64>,
}

impl WriterState {
    fn new(
        cfg: RuntimeConfig,
        dropped_events: Arc<AtomicU64>,
        write_failures: Arc<AtomicU64>,
    ) -> Self {
        Self {
            cfg,
            buffer: Vec::new(),
            sequence: 0,
            dropped_events,
            write_failures,
        }
    }

    async fn run(mut self, mut rx: mpsc::Receiver<WriterMessage>) -> anyhow::Result<()> {
        let mut ticker = interval(self.cfg.flush_interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                maybe_msg = rx.recv() => {
                    match maybe_msg {
                        Some(WriterMessage::Event(event)) => {
                            self.buffer.push(event);
                            if self.buffer.len() >= self.cfg.flush_rows {
                                self.try_flush();
                            }
                        }
                        Some(WriterMessage::Shutdown(done)) => {
                            self.drain_pending_events(&mut rx).await;
                            self.try_flush();
                            let _ = done.send(());
                            break;
                        }
                        None => {
                            self.try_flush();
                            break;
                        }
                    }
                }
                _ = ticker.tick() => {
                    self.try_flush();
                }
            }
        }

        Ok(())
    }

    async fn drain_pending_events(&mut self, rx: &mut mpsc::Receiver<WriterMessage>) {
        while let Ok(message) = rx.try_recv() {
            match message {
                WriterMessage::Event(event) => self.buffer.push(event),
                WriterMessage::Shutdown(done) => {
                    let _ = done.send(());
                }
            }
        }

        while let Ok(Some(message)) =
            tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await
        {
            match message {
                WriterMessage::Event(event) => self.buffer.push(event),
                WriterMessage::Shutdown(done) => {
                    let _ = done.send(());
                }
            }
        }
    }

    fn try_flush(&mut self) {
        if self.buffer.is_empty() {
            return;
        }

        if let Err(err) = self.flush() {
            let count = self.write_failures.fetch_add(1, Ordering::Relaxed) + 1;
            error!(error = %err, write_failures = count, "failed to write parquet batch");
        }
    }

    fn flush(&mut self) -> anyhow::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        let batch = events_to_record_batch(&self.buffer)?;
        let path = self.next_output_path()?;
        let file = File::create(&path)
            .with_context(|| format!("create parquet output file {}", path.display()))?;
        let props = WriterProperties::builder()
            .set_compression(Compression::ZSTD(Default::default()))
            .build();

        let mut writer =
            ArrowWriter::try_new(file, Arc::new(batch.schema().as_ref().clone()), Some(props))
                .context("create parquet writer")?;
        writer.write(&batch).context("write parquet batch")?;
        writer.close().context("close parquet writer")?;

        info!(
            path = %path.display(),
            rows = self.buffer.len(),
            dropped_events = self.dropped_events.load(Ordering::Relaxed),
            "wrote parquet batch"
        );

        self.buffer.clear();
        self.sequence += 1;
        Ok(())
    }

    fn next_output_path(&self) -> anyhow::Result<PathBuf> {
        let now = OffsetDateTime::now_utc();
        let dir = self
            .cfg
            .parquet_dir
            .join(format!("{:04}", now.year()))
            .join(format!("{:02}", u8::from(now.month())))
            .join(format!("{:02}", now.day()))
            .join(format!("{:02}", now.hour()));

        fs::create_dir_all(&dir)
            .with_context(|| format!("create parquet partition dir {}", dir.display()))?;

        let filename = format!(
            "{}-{}-{}.parquet",
            now.unix_timestamp(),
            std::process::id(),
            self.sequence
        );

        Ok(dir.join(filename))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::HttpMode;
    use crate::config::{HeaderLogPolicy, RuntimeConfig};
    use anyhow::Context;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use tempfile::TempDir;
    use url::Url;

    #[test]
    fn partitions_output_paths_by_hour() -> anyhow::Result<()> {
        let tempdir = TempDir::new().context("create tempdir")?;
        let cfg = RuntimeConfig {
            listen_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8443),
            frontend_domain: "example.test".to_owned(),
            backend_url: Url::parse("https://backend.test").context("parse backend url")?,
            backend_host: "backend.test".to_owned(),
            backend_port: 443,
            backend_authority: "backend.test".to_owned(),
            backend_path_prefix: "/".to_owned(),
            tls_cert_path: tempdir.path().join("unused-cert.pem"),
            tls_key_path: tempdir.path().join("unused-key.pem"),
            parquet_dir: tempdir.path().join("parquet"),
            ca_bundle_path: None,
            upstream_sni: "backend.test".to_owned(),
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
            rollover_minutes: 60,
        };

        let state = WriterState::new(
            cfg,
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        );
        let path = state.next_output_path()?;
        let parent = path.parent().context("missing parquet parent dir")?;

        assert_eq!(
            path.extension().and_then(|value| value.to_str()),
            Some("parquet")
        );
        assert!(parent.starts_with(tempdir.path().join("parquet")));

        Ok(())
    }
}
