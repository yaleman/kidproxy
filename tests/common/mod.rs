#![allow(dead_code)]

use anyhow::{Context, anyhow};
use bytes::Bytes;
use futures::{StreamExt, stream};
use kidproxy::cli::{Cli, HttpMode};
use kidproxy::config::RuntimeConfig;
use kidproxy::entity;
use kidproxy::probe::{self, BackendProbe};
use kidproxy::proxy::{ProxyApp, ProxyHandle};
use kidproxy::writer::{SqliteWriterHandle, spawn_writer};
use rama::Layer;
use rama::graceful::Shutdown;
use rama::http::{
    Body, Request, Response, StatusCode, body::util::BodyExt, header, server::HttpServer,
};
use rama::service::service_fn;
use rama::tcp::server::TcpListener;
use rama::tls::rustls::dep::pki_types::{CertificateDer, PrivateKeyDer};
use rama::tls::rustls::server::{TlsAcceptorDataBuilder, TlsAcceptorLayer};
use rcgen::generate_simple_self_signed;
use sea_orm::EntityTrait;
use std::convert::Infallible;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener as StdTcpListener};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Once;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;
use tokio::sync::oneshot;

#[derive(Clone)]
pub struct TestCert {
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
    pub cert_pem: Vec<u8>,
}

pub struct BackendHarness {
    pub addr: SocketAddr,
    pub cert: TestCert,
    pub hits: Arc<AtomicUsize>,
    stop_tx: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
}

impl BackendHarness {
    pub async fn shutdown(&mut self) -> anyhow::Result<()> {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
        if let Some(task) = &mut self.task {
            if let Ok(result) = tokio::time::timeout(std::time::Duration::from_secs(2), task).await
            {
                result.map_err(|err| anyhow!("backend task join failure: {err}"))??;
            } else if let Some(task) = self.task.take() {
                task.abort();
                match task.await {
                    Ok(result) => result?,
                    Err(err) if err.is_cancelled() => {}
                    Err(err) => return Err(anyhow!("backend task abort failure: {err}")),
                }
            }
        }

        Ok(())
    }
}

pub struct ProxyHarness {
    pub _tempdir: TempDir,
    pub frontend_cert: TestCert,
    pub backend: BackendHarness,
    pub config: RuntimeConfig,
    pub writer: SqliteWriterHandle,
    handle: Option<ProxyHandle>,
}

impl ProxyHarness {
    pub fn proxy_addr(&self) -> SocketAddr {
        self.config.listen_addr
    }

    pub async fn shutdown(&mut self) -> anyhow::Result<()> {
        if let Some(handle) = self.handle.take() {
            handle.shutdown().await?;
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), self.writer.shutdown())
            .await
            .context("writer shutdown timed out")??;
        self.backend.shutdown().await
    }
}

pub async fn start_proxy_harness(
    body_max_bytes: usize,
    flush_rows: usize,
) -> anyhow::Result<ProxyHarness> {
    start_proxy_harness_with_config(body_max_bytes, flush_rows, None).await
}

pub async fn start_proxy_harness_with_config(
    body_max_bytes: usize,
    flush_rows: usize,
    config_path: Option<PathBuf>,
) -> anyhow::Result<ProxyHarness> {
    init_test_crypto_provider();

    let tempdir = TempDir::new().context("create test tempdir")?;
    let frontend_cert = write_test_cert(
        tempdir.path(),
        "frontend",
        &["proxy.local".to_owned(), "127.0.0.1".to_owned()],
    )?;
    let backend = start_backend(tempdir.path()).await?;

    let config = RuntimeConfig::try_from(Cli {
        listen_addr: format!("127.0.0.1:{}", unused_port()?),
        frontend_domain: "example.test".to_owned(),
        backend_url: format!("https://127.0.0.1:{}", backend.addr.port()),
        tls_cert_path: frontend_cert.cert_path.clone(),
        tls_key_path: frontend_cert.key_path.clone(),
        sqlite_path: PathBuf::from(":memory:"),
        config_path,
        ca_bundle_path: Some(backend.cert.cert_path.clone()),
        upstream_sni_override: Some("127.0.0.1".to_owned()),
        http_mode: HttpMode::Http1,
        flush_rows,
        flush_interval_ms: 100,
        max_inflight_events: 1024,
        body_max_bytes,
        connect_timeout_ms: 2_000,
        request_timeout_ms: 10_000,
        idle_pool_timeout_ms: 2_000,
        graceful_shutdown_timeout_ms: 2_000,
        trust_proxy_headers: false,
        emit_keylog: false,
        header_allowlist: Vec::new(),
        header_denylist: Vec::new(),
    })
    .context("build test runtime config")?;

    let probe = probe::probe_backend(&config)
        .await
        .unwrap_or_else(|_| BackendProbe::fallback(&config));
    let writer = spawn_writer(&config).await.context("spawn test writer")?;
    let app = ProxyApp::new(config.clone(), writer.clone(), probe)
        .await
        .context("create proxy app")?;
    let handle = app.start().await.context("start proxy app")?;
    wait_for_listener(config.listen_addr).await?;

    Ok(ProxyHarness {
        _tempdir: tempdir,
        frontend_cert,
        backend,
        config,
        writer,
        handle: Some(handle),
    })
}

pub fn test_client(
    frontend_cert_pem: &[u8],
    proxy_addr: SocketAddr,
) -> anyhow::Result<reqwest::Client> {
    let cert = reqwest::Certificate::from_pem(frontend_cert_pem)
        .context("parse frontend test certificate")?;

    reqwest::Client::builder()
        .add_root_certificate(cert)
        .resolve("proxy.local", proxy_addr)
        .http1_only()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("build reqwest client")
}

pub fn proxy_url(proxy_addr: SocketAddr, path: &str) -> String {
    format!("https://proxy.local:{}{path}", proxy_addr.port())
}

pub async fn read_logged_events(writer: &SqliteWriterHandle) -> anyhow::Result<Vec<entity::Model>> {
    entity::Entity::find()
        .all(&writer.database())
        .await
        .context("read logged SQLite events")
}

fn write_test_cert(base: &Path, name: &str, sans: &[String]) -> anyhow::Result<TestCert> {
    let certified =
        generate_simple_self_signed(sans.to_vec()).context("generate test certificate")?;
    let cert_pem = certified.cert.pem().into_bytes();
    let key_pem = certified.signing_key.serialize_pem().into_bytes();
    let cert_path = base.join(format!("{name}.crt.pem"));
    let key_path = base.join(format!("{name}.key.pem"));

    fs::write(&cert_path, &cert_pem)
        .with_context(|| format!("write cert {}", cert_path.display()))?;
    fs::write(&key_path, &key_pem).with_context(|| format!("write key {}", key_path.display()))?;

    Ok(TestCert {
        cert_path,
        key_path,
        cert_pem,
    })
}

async fn start_backend(base: &Path) -> anyhow::Result<BackendHarness> {
    let cert = write_test_cert(
        base,
        "backend",
        &["localhost".to_owned(), "127.0.0.1".to_owned()],
    )?;
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), unused_port()?);
    let hits = Arc::new(AtomicUsize::new(0));
    let (stop_tx, stop_rx) = oneshot::channel();
    let listener = TcpListener::build()
        .bind(addr)
        .await
        .map_err(|err| anyhow!("bind backend listener: {err}"))?;
    let tls = build_test_acceptor(&cert)?;
    let hits_for_service = hits.clone();

    let task = tokio::spawn(async move {
        let shutdown = Shutdown::default();
        let serve_guard = shutdown.guard();
        let http_service = HttpServer::http1().service(service_fn(move |req: Request| {
            let hits = hits_for_service.clone();
            async move { backend_response(req, hits).await }
        }));
        let mut serve_task = tokio::spawn(async move {
            listener
                .serve_graceful(
                    serve_guard,
                    TlsAcceptorLayer::new(tls).into_layer(http_service),
                )
                .await;
            Ok::<(), anyhow::Error>(())
        });

        tokio::select! {
            result = &mut serve_task => {
                result.map_err(|err| anyhow!("backend serve task join failure: {err}"))??;
            }
            _ = stop_rx => {
                let _ = shutdown.shutdown_with_limit(std::time::Duration::from_secs(2)).await;
                serve_task
                    .await
                    .map_err(|err| anyhow!("backend serve task join failure: {err}"))??;
            }
        }

        Ok(())
    });

    Ok(BackendHarness {
        addr,
        cert,
        hits,
        stop_tx: Some(stop_tx),
        task: Some(task),
    })
}

async fn backend_response(req: Request, hits: Arc<AtomicUsize>) -> Result<Response, Infallible> {
    hits.fetch_add(1, Ordering::Relaxed);

    let path = req.uri().path().to_owned();
    let response = match path.as_str() {
        "/hello" => Response::new(Body::from("hello from backend")),
        "/headers" => {
            let mut response = Response::new(Body::from("backend header body"));
            response
                .headers_mut()
                .insert("x-test", header::HeaderValue::from_static("backend header"));
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("text/plain"),
            );
            response
        }
        "/cookies" => {
            let mut response = Response::new(Body::from("cookie backend body"));
            response.headers_mut().append(
                header::SET_COOKIE,
                header::HeaderValue::from_static("session=backend-token; Path=/"),
            );
            response.headers_mut().append(
                header::SET_COOKIE,
                header::HeaderValue::from_static("mode=backend; Path=/"),
            );
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("text/plain"),
            );
            response
        }
        "/html" => {
            let mut response = Response::new(Body::from("<html>backend page</html>"));
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("text/html; charset=utf-8"),
            );
            response
        }
        "/echo-accept-encoding" => {
            let body = req
                .headers()
                .get(header::ACCEPT_ENCODING)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("<none>")
                .to_owned();
            Response::new(Body::from(body))
        }
        "/redirect" => {
            let mut response = Response::new(Body::empty());
            *response.status_mut() = StatusCode::FOUND;
            response.headers_mut().insert(
                header::LOCATION,
                header::HeaderValue::from_static("https://backend.local/elsewhere"),
            );
            response
        }
        "/echo" => match req.into_body().collect().await {
            Ok(body) => Response::new(Body::from(body.to_bytes())),
            Err(err) => {
                let mut response = Response::new(Body::from(format!("collect body failed: {err}")));
                *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
                response
            }
        },
        "/stream" => {
            let chunks = vec![
                (Bytes::from_static(b"first-"), 150_u64),
                (Bytes::from_static(b"second-"), 150_u64),
                (Bytes::from_static(b"third"), 0_u64),
            ];
            let body_stream = stream::iter(chunks).then(|(chunk, delay_ms)| async move {
                if delay_ms > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                }
                Ok::<Bytes, std::io::Error>(chunk)
            });

            let mut response = Response::new(Body::from_stream(body_stream));
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("text/plain"),
            );
            response
        }
        "/big-response" => {
            let mut response = Response::new(Body::from("abcdefghijklmnopqrstuvwxyz"));
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("text/plain"),
            );
            response
        }
        _ => {
            let mut response = Response::new(Body::from("not found"));
            *response.status_mut() = StatusCode::NOT_FOUND;
            response
        }
    };

    Ok(response)
}

fn build_test_acceptor(
    cert: &TestCert,
) -> anyhow::Result<rama::tls::rustls::server::TlsAcceptorData> {
    let cert_chain = load_cert_chain(&cert.cert_path)?;
    let private_key = load_private_key(&cert.key_path)?;
    TlsAcceptorDataBuilder::new(cert_chain, private_key)
        .map(|builder| builder.build())
        .context("build test TLS acceptor")
}

fn load_cert_chain(path: &Path) -> anyhow::Result<Vec<CertificateDer<'static>>> {
    let file = fs::File::open(path).with_context(|| format!("open cert {}", path.display()))?;
    let mut reader = std::io::BufReader::new(file);
    rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("read cert chain {}", path.display()))
}

fn load_private_key(path: &Path) -> anyhow::Result<PrivateKeyDer<'static>> {
    let file = fs::File::open(path).with_context(|| format!("open key {}", path.display()))?;
    let mut reader = std::io::BufReader::new(file);
    let key = rustls_pemfile::private_key(&mut reader)
        .with_context(|| format!("read key {}", path.display()))?;
    key.ok_or_else(|| anyhow!("missing private key in {}", path.display()))
}

fn unused_port() -> anyhow::Result<u16> {
    let listener = StdTcpListener::bind((Ipv4Addr::LOCALHOST, 0)).context("bind port probe")?;
    let port = listener
        .local_addr()
        .context("read port probe local addr")?
        .port();
    Ok(port)
}

async fn wait_for_listener(addr: SocketAddr) -> anyhow::Result<()> {
    let started = std::time::Instant::now();
    loop {
        match tokio::net::TcpStream::connect(addr).await {
            Ok(stream) => {
                drop(stream);
                return Ok(());
            }
            Err(err) => {
                if started.elapsed() > std::time::Duration::from_secs(2) {
                    return Err(anyhow!("proxy listener did not become ready: {err}"));
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        }
    }
}

fn init_test_crypto_provider() {
    static INIT: Once = Once::new();

    INIT.call_once(|| {
        let _ =
            rama::tls::rustls::dep::rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}
