use crate::config::RuntimeConfig;
use crate::entity;
use crate::error::ErrorKind;
use crate::event::ProxyEvent;
use crate::migration::Migrator;
use anyhow::Context;
use sea_orm::ActiveValue::Set;
use sea_orm::{ConnectOptions, Database, DatabaseConnection, EntityTrait, TransactionTrait};
use sea_orm_migration::MigratorTrait;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use time::format_description::well_known::Rfc3339;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{MissedTickBehavior, interval};
use tracing::{error, info, warn};

#[derive(Clone)]
pub struct SqliteWriterHandle {
    tx: mpsc::Sender<WriterMessage>,
    database: DatabaseConnection,
    dropped_events: Arc<AtomicU64>,
    write_failures: Arc<AtomicU64>,
}

enum WriterMessage {
    Event(ProxyEvent),
    Shutdown(oneshot::Sender<()>),
}

pub async fn spawn_writer(cfg: &RuntimeConfig) -> anyhow::Result<SqliteWriterHandle> {
    ensure_sqlite_parent_dir(&cfg.sqlite_path)?;

    let database = connect_database(&cfg.sqlite_path).await?;
    Migrator::up(&database, None)
        .await
        .context("run SQLite migrations")?;

    let (tx, rx) = mpsc::channel(cfg.max_inflight_events);
    let dropped_events = Arc::new(AtomicU64::new(0));
    let write_failures = Arc::new(AtomicU64::new(0));
    let state = WriterState::new(
        cfg.clone(),
        database.clone(),
        dropped_events.clone(),
        write_failures.clone(),
    );

    tokio::spawn(async move {
        if let Err(err) = state.run(rx).await {
            error!(error = %err, "writer task failed");
        }
    });

    Ok(SqliteWriterHandle {
        tx,
        database,
        dropped_events,
        write_failures,
    })
}

impl SqliteWriterHandle {
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

    pub fn database(&self) -> DatabaseConnection {
        self.database.clone()
    }
}

struct WriterState {
    cfg: RuntimeConfig,
    database: DatabaseConnection,
    buffer: Vec<ProxyEvent>,
    dropped_events: Arc<AtomicU64>,
    write_failures: Arc<AtomicU64>,
}

impl WriterState {
    fn new(
        cfg: RuntimeConfig,
        database: DatabaseConnection,
        dropped_events: Arc<AtomicU64>,
        write_failures: Arc<AtomicU64>,
    ) -> Self {
        Self {
            cfg,
            database,
            buffer: Vec::new(),
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
                                self.try_flush().await;
                            }
                        }
                        Some(WriterMessage::Shutdown(done)) => {
                            self.drain_pending_events(&mut rx).await;
                            self.try_flush().await;
                            let _ = done.send(());
                            break;
                        }
                        None => {
                            self.try_flush().await;
                            break;
                        }
                    }
                }
                _ = ticker.tick() => {
                    self.try_flush().await;
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

    async fn try_flush(&mut self) {
        if self.buffer.is_empty() {
            return;
        }

        if let Err(err) = self.flush().await {
            let count = self.write_failures.fetch_add(1, Ordering::Relaxed) + 1;
            error!(
                error = %err,
                error_kind = %ErrorKind::SqliteWrite,
                write_failures = count,
                "failed to write SQLite batch"
            );
        }
    }

    async fn flush(&mut self) -> anyhow::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        let models = self
            .buffer
            .iter()
            .map(event_to_active_model)
            .collect::<anyhow::Result<Vec<_>>>()?;
        let rows = models.len();
        let transaction = self
            .database
            .begin()
            .await
            .context("begin SQLite transaction")?;

        entity::Entity::insert_many(models)
            .exec(&transaction)
            .await
            .context("insert SQLite batch")?;

        transaction
            .commit()
            .await
            .context("commit SQLite transaction")?;

        self.buffer.clear();

        info!(
            sqlite_path = %self.cfg.sqlite_path.display(),
            rows,
            dropped_events = self.dropped_events.load(Ordering::Relaxed),
            "wrote SQLite batch"
        );

        Ok(())
    }
}

fn ensure_sqlite_parent_dir(path: &Path) -> anyhow::Result<()> {
    if path == Path::new(":memory:") {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create SQLite parent dir {}", parent.display()))?;
    }
    Ok(())
}

async fn connect_database(path: &Path) -> anyhow::Result<DatabaseConnection> {
    let database_url = sqlite_connection_url(path);
    let mut options = ConnectOptions::new(database_url);
    options.max_connections(1).sqlx_logging(false);

    Database::connect(options)
        .await
        .with_context(|| format!("connect SQLite database {}", path.display()))
}

fn sqlite_connection_url(path: &Path) -> String {
    if path == Path::new(":memory:") {
        return "sqlite::memory:".to_owned();
    }
    let encoded_path = path.to_string_lossy().replace(' ', "%20");
    format!("sqlite://{encoded_path}?mode=rwc")
}

fn event_to_active_model(event: &ProxyEvent) -> anyhow::Result<entity::ActiveModel> {
    Ok(entity::ActiveModel {
        event_id: Set(event.event_id.to_string()),
        connection_id: Set(event.connection_id.to_string()),
        request_start_ts: Set(format_timestamp(event.request_start_ts)?),
        request_end_ts: Set(format_timestamp(event.request_end_ts)?),
        duration_ms: Set(event.duration_ms),
        client_ip: Set(event.client_ip.clone()),
        client_port: Set(event.client_port.map(i32::from)),
        proxy_local_ip: Set(event.proxy_local_ip.clone()),
        proxy_local_port: Set(event.proxy_local_port.map(i32::from)),
        frontend_server_name: Set(event.frontend_server_name.clone()),
        frontend_http_version: Set(event.frontend_http_version.clone()),
        frontend_scheme: Set(event.frontend_scheme.clone()),
        backend_url: Set(event.backend_url.clone()),
        backend_host: Set(event.backend_host.clone()),
        backend_ip: Set(event.backend_ip.clone()),
        backend_port: Set(event.backend_port.map(i32::from)),
        backend_http_version: Set(event.backend_http_version.clone()),
        upstream_connection_reused: Set(event.upstream_connection_reused),
        method: Set(event.method.clone()),
        authority: Set(event.authority.clone()),
        path: Set(event.path.clone()),
        query: Set(event.query.clone()),
        request_headers_json: Set(event.request_headers_json.clone()),
        request_cookie_header: Set(event.request_cookie_header.clone()),
        request_cookies_json: Set(event.request_cookies_json.clone()),
        request_content_length: Set(event.request_content_length),
        request_transfer_encoding: Set(event.request_transfer_encoding.clone()),
        request_content_type: Set(event.request_content_type.clone()),
        request_body_sha256: Set(event.request_body_sha256.clone()),
        request_body_preview_base64: Set(event.request_body_preview_base64.clone()),
        request_body_truncated: Set(event.request_body_truncated),
        status_code: Set(event.status_code.map(i32::from)),
        reason_phrase: Set(event.reason_phrase.clone()),
        response_headers_json: Set(event.response_headers_json.clone()),
        response_set_cookie_json: Set(event.response_set_cookie_json.clone()),
        response_content_length: Set(event.response_content_length),
        response_transfer_encoding: Set(event.response_transfer_encoding.clone()),
        response_content_type: Set(event.response_content_type.clone()),
        response_content_encoding: Set(event.response_content_encoding.clone()),
        response_body_sha256: Set(event.response_body_sha256.clone()),
        response_body_preview_base64: Set(event.response_body_preview_base64.clone()),
        response_body_truncated: Set(event.response_body_truncated),
        frontend_tls_version: Set(event.frontend_tls_version.clone()),
        frontend_tls_cipher_suite: Set(event.frontend_tls_cipher_suite.clone()),
        frontend_tls_alpn: Set(event.frontend_tls_alpn.clone()),
        frontend_tls_sni: Set(event.frontend_tls_sni.clone()),
        frontend_tls_ja3: Set(event.frontend_tls_ja3.clone()),
        frontend_tls_ja4: Set(event.frontend_tls_ja4.clone()),
        backend_tls_version: Set(event.backend_tls_version.clone()),
        backend_tls_cipher_suite: Set(event.backend_tls_cipher_suite.clone()),
        backend_tls_alpn: Set(event.backend_tls_alpn.clone()),
        backend_tls_sni: Set(event.backend_tls_sni.clone()),
        backend_cert_leaf_sha256: Set(event.backend_cert_leaf_sha256.clone()),
        backend_cert_subject: Set(event.backend_cert_subject.clone()),
        backend_cert_issuer: Set(event.backend_cert_issuer.clone()),
        backend_cert_not_before: Set(format_optional_timestamp(event.backend_cert_not_before)?),
        backend_cert_not_after: Set(format_optional_timestamp(event.backend_cert_not_after)?),
        dns_duration_ms: Set(event.dns_duration_ms),
        connect_duration_ms: Set(event.connect_duration_ms),
        tls_handshake_duration_ms: Set(event.tls_handshake_duration_ms),
        upstream_first_byte_ms: Set(event.upstream_first_byte_ms),
        response_stream_duration_ms: Set(event.response_stream_duration_ms),
        error_kind: Set(event.error_kind.map(|kind| kind.as_str().to_owned())),
        error_text: Set(event.error_text.clone()),
        proxy_result: Set(event.proxy_result.as_str().to_owned()),
    })
}

fn format_timestamp(timestamp: time::OffsetDateTime) -> anyhow::Result<String> {
    timestamp
        .format(&Rfc3339)
        .context("format timestamp for SQLite storage")
}

fn format_optional_timestamp(
    timestamp: Option<time::OffsetDateTime>,
) -> anyhow::Result<Option<String>> {
    timestamp.map(format_timestamp).transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::HttpMode;
    use crate::config::{HeaderLogPolicy, RuntimeConfig};
    use crate::error::ProxyResult;
    use crate::event::ProxyEvent;
    use sea_orm::EntityTrait;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::path::PathBuf;
    use time::OffsetDateTime;
    use url::Url;
    use uuid::Uuid;

    #[tokio::test]
    async fn creates_sqlite_database_and_applies_migration() -> anyhow::Result<()> {
        let cfg = RuntimeConfig {
            listen_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8443),
            frontend_domain: "example.test".to_owned(),
            backend_url: Url::parse("https://backend.test").context("parse backend url")?,
            backend_host: "backend.test".to_owned(),
            backend_port: 443,
            backend_authority: "backend.test".to_owned(),
            backend_path_prefix: "/".to_owned(),
            tls_cert_path: PathBuf::from("unused-cert.pem"),
            tls_key_path: PathBuf::from("unused-key.pem"),
            sqlite_path: PathBuf::from(":memory:"),
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
        };

        let writer = spawn_writer(&cfg).await?;
        writer.try_send(ProxyEvent {
            event_id: Uuid::new_v4(),
            connection_id: Uuid::new_v4(),
            request_start_ts: OffsetDateTime::now_utc(),
            request_end_ts: OffsetDateTime::now_utc(),
            duration_ms: 1,
            client_ip: None,
            client_port: None,
            proxy_local_ip: None,
            proxy_local_port: None,
            frontend_server_name: None,
            frontend_http_version: None,
            frontend_scheme: "https".to_owned(),
            backend_url: "https://backend.test".to_owned(),
            backend_host: "backend.test".to_owned(),
            backend_ip: None,
            backend_port: Some(443),
            backend_http_version: None,
            upstream_connection_reused: None,
            method: "GET".to_owned(),
            authority: None,
            path: "/".to_owned(),
            query: None,
            request_headers_json: "{}".to_owned(),
            request_cookie_header: None,
            request_cookies_json: None,
            request_content_length: None,
            request_transfer_encoding: None,
            request_content_type: None,
            request_body_sha256: None,
            request_body_preview_base64: None,
            request_body_truncated: Some(false),
            status_code: Some(200),
            reason_phrase: Some("OK".to_owned()),
            response_headers_json: Some("{}".to_owned()),
            response_set_cookie_json: None,
            response_content_length: None,
            response_transfer_encoding: None,
            response_content_type: None,
            response_content_encoding: None,
            response_body_sha256: None,
            response_body_preview_base64: None,
            response_body_truncated: Some(false),
            frontend_tls_version: None,
            frontend_tls_cipher_suite: None,
            frontend_tls_alpn: None,
            frontend_tls_sni: None,
            frontend_tls_ja3: None,
            frontend_tls_ja4: None,
            backend_tls_version: None,
            backend_tls_cipher_suite: None,
            backend_tls_alpn: None,
            backend_tls_sni: None,
            backend_cert_leaf_sha256: None,
            backend_cert_subject: None,
            backend_cert_issuer: None,
            backend_cert_not_before: None,
            backend_cert_not_after: None,
            dns_duration_ms: None,
            connect_duration_ms: None,
            tls_handshake_duration_ms: None,
            upstream_first_byte_ms: None,
            response_stream_duration_ms: None,
            error_kind: None,
            error_text: None,
            proxy_result: ProxyResult::Success,
        });
        writer.shutdown().await?;

        let db = writer.database();
        let rows = entity::Entity::find().all(&db).await?;
        assert_eq!(rows.len(), 1);

        Ok(())
    }
}
