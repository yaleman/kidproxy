use crate::capture::{
    ObservedBody, SharedExchangeCapture, host_matches_frontend, make_gateway_error,
    strip_hop_by_hop_headers,
};
use crate::config::{ResolvedHttpMode, RuntimeConfig};
use crate::error::{ErrorInfo, ErrorKind};
use crate::probe::BackendProbe;
use crate::tls::{build_frontend_tls, build_upstream_tls};
use crate::writer::SqliteWriterHandle;
use anyhow::{Context, anyhow};
use rama::graceful::Shutdown;
use rama::http::{
    Body, Request, Response, StatusCode, client::EasyHttpWebClient,
    layer::required_header::AddRequiredRequestHeadersLayer, server::HttpServer,
};
use rama::net::client::pool::http::HttpPooledConnectorConfig;
use rama::rt::Executor;
use rama::service::{BoxService, service_fn};
use rama::tcp::{TcpStream, client::service::TcpConnector, server::TcpListener};
use rama::tls::rustls::server::TlsAcceptorLayer;
use rama::{Layer, Service};
use std::convert::Infallible;
use tokio::sync::oneshot;
use tracing::info;

pub struct ProxyApp {
    cfg: RuntimeConfig,
    writer: SqliteWriterHandle,
    probe: BackendProbe,
    http_mode: ResolvedHttpMode,
    upstream_client: BoxService<Request, Response, rama::error::BoxError>,
    frontend_tls: crate::tls::FrontendTlsConfig,
}

pub struct ProxyHandle {
    stop_tx: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl ProxyHandle {
    pub async fn wait(self) -> anyhow::Result<()> {
        self.task
            .await
            .map_err(|err| anyhow!("proxy task join failure: {err}"))?
    }

    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
        if let Ok(result) =
            tokio::time::timeout(std::time::Duration::from_secs(5), &mut self.task).await
        {
            result.map_err(|err| anyhow!("proxy task join failure: {err}"))?
        } else {
            self.task.abort();
            match self.task.await {
                Ok(result) => result?,
                Err(err) if err.is_cancelled() => {}
                Err(err) => return Err(anyhow!("proxy task abort failure: {err}")),
            }
            Ok(())
        }
    }
}

impl ProxyApp {
    pub async fn new(
        cfg: RuntimeConfig,
        writer: SqliteWriterHandle,
        probe: BackendProbe,
    ) -> anyhow::Result<Self> {
        let http_mode = cfg.resolved_http_mode(probe.supports_h2);
        let frontend_tls = build_frontend_tls(&cfg, http_mode)?;
        let upstream_client = build_upstream_client(&cfg, http_mode)?;

        Ok(Self {
            cfg,
            writer,
            probe,
            http_mode,
            upstream_client,
            frontend_tls,
        })
    }

    pub async fn start(self) -> anyhow::Result<ProxyHandle> {
        let (stop_tx, stop_rx) = oneshot::channel();
        let cfg = self.cfg.clone();
        let writer = self.writer.clone();
        let probe = self.probe.clone();
        let http_mode = self.http_mode;
        let upstream_client = self.upstream_client.clone();
        let frontend_tls = self.frontend_tls.clone();

        let task = tokio::spawn(async move {
            let shutdown = Shutdown::default();
            let listener = TcpListener::build()
                .bind(cfg.listen_addr)
                .await
                .map_err(|err| anyhow!("bind frontend TCP listener: {err}"))?;

            let service = ProxyService {
                cfg: cfg.clone(),
                writer,
                client: upstream_client,
            };
            let exec_guard = shutdown.guard();
            let serve_guard = shutdown.guard();
            let exec = Executor::graceful(exec_guard);

            info!(
                listen_addr = %cfg.listen_addr,
                frontend_domain = %cfg.frontend_domain,
                backend_url = %cfg.backend_url,
                backend_supports_h2 = ?probe.supports_h2,
                resolved_http_mode = ?http_mode,
                "proxy listener ready"
            );

            let mut serve_task = match http_mode {
                ResolvedHttpMode::Auto => {
                    let http_service = HttpServer::auto(exec).service(service_fn(move |req| {
                        let service = service.clone();
                        async move { service.handle(req).await }
                    }));
                    tokio::spawn(async move {
                        listener
                            .serve_graceful(
                                serve_guard,
                                TlsAcceptorLayer::new(frontend_tls.acceptor_data)
                                    .into_layer(http_service),
                            )
                            .await;
                        Ok::<(), anyhow::Error>(())
                    })
                }
                ResolvedHttpMode::Http1 => {
                    let http_service = HttpServer::http1().service(service_fn(move |req| {
                        let service = service.clone();
                        async move { service.handle(req).await }
                    }));
                    tokio::spawn(async move {
                        listener
                            .serve_graceful(
                                serve_guard,
                                TlsAcceptorLayer::new(frontend_tls.acceptor_data)
                                    .into_layer(http_service),
                            )
                            .await;
                        Ok::<(), anyhow::Error>(())
                    })
                }
                ResolvedHttpMode::Http2 => {
                    let http_service = HttpServer::h2(exec).service(service_fn(move |req| {
                        let service = service.clone();
                        async move { service.handle(req).await }
                    }));
                    tokio::spawn(async move {
                        listener
                            .serve_graceful(
                                serve_guard,
                                TlsAcceptorLayer::new(frontend_tls.acceptor_data)
                                    .into_layer(http_service),
                            )
                            .await;
                        Ok::<(), anyhow::Error>(())
                    })
                }
            };

            tokio::select! {
                result = &mut serve_task => {
                    result.map_err(|err| anyhow!("proxy serve task join failure: {err}"))??;
                }
                _ = stop_rx => {
                    shutdown
                        .shutdown_with_limit(cfg.graceful_shutdown_timeout)
                        .await
                        .context("graceful proxy shutdown failed")?;
                    serve_task
                        .await
                        .map_err(|err| anyhow!("proxy serve task join failure: {err}"))??;
                }
            }

            Ok(())
        });

        Ok(ProxyHandle {
            stop_tx: Some(stop_tx),
            task,
        })
    }
}

#[derive(Clone)]
struct ProxyService {
    cfg: RuntimeConfig,
    writer: SqliteWriterHandle,
    client: BoxService<Request, Response, rama::error::BoxError>,
}

impl ProxyService {
    async fn handle(&self, req: Request) -> Result<Response, Infallible> {
        let capture = SharedExchangeCapture::new(&self.cfg, &req);

        if !host_matches_frontend(&req, &self.cfg.frontend_domain) {
            capture.set_rejection(
                StatusCode::MISDIRECTED_REQUEST,
                ErrorInfo::new(
                    ErrorKind::ClientProtocol,
                    "request host does not match configured frontend domain",
                ),
            );
            capture.finalize_and_send(&self.writer);
            return Ok(make_gateway_error(
                StatusCode::MISDIRECTED_REQUEST,
                "misdirected request",
            ));
        }

        let (mut parts, body) = req.into_parts();
        let upstream_uri = match self
            .cfg
            .build_backend_uri(parts.uri.path(), parts.uri.query())
        {
            Ok(uri) => uri,
            Err(err) => {
                capture.set_rejection(
                    StatusCode::BAD_REQUEST,
                    ErrorInfo::from_display(
                        ErrorKind::Config,
                        format!("failed to construct upstream URI: {err}"),
                    ),
                );
                capture.finalize_and_send(&self.writer);
                return Ok(make_gateway_error(StatusCode::BAD_REQUEST, "bad request"));
            }
        };

        parts.uri = upstream_uri;
        parts.headers.remove(rama::http::header::HOST);
        strip_hop_by_hop_headers(&mut parts.headers);

        let observed_request_body = Body::new(ObservedBody::request(body, capture.clone()));
        let upstream_request = Request::from_parts(parts, observed_request_body);

        let upstream_result = tokio::time::timeout(
            self.cfg.request_timeout,
            self.client.serve(upstream_request),
        )
        .await;

        let response_started_at = time::OffsetDateTime::now_utc();
        let upstream_response = match upstream_result {
            Ok(Ok(resp)) => resp,
            Ok(Err(err)) => {
                capture
                    .set_upstream_error(ErrorInfo::from_display(ErrorKind::UpstreamProtocol, err));
                capture.finalize_and_send(&self.writer);
                return Ok(make_gateway_error(StatusCode::BAD_GATEWAY, "bad gateway"));
            }
            Err(_) => {
                capture.set_upstream_error(ErrorInfo::new(
                    ErrorKind::Timeout,
                    "upstream request timed out",
                ));
                capture.finalize_and_send(&self.writer);
                return Ok(make_gateway_error(
                    StatusCode::GATEWAY_TIMEOUT,
                    "gateway timeout",
                ));
            }
        };

        capture.set_upstream_response(&self.cfg, &upstream_response, response_started_at);

        let (mut response_parts, response_body) = upstream_response.into_parts();
        strip_hop_by_hop_headers(&mut response_parts.headers);

        let response = Response::from_parts(
            response_parts,
            Body::new(ObservedBody::response(
                response_body,
                capture,
                self.writer.clone(),
            )),
        );

        Ok(response)
    }
}

pub(crate) fn build_upstream_client(
    cfg: &RuntimeConfig,
    http_mode: ResolvedHttpMode,
) -> anyhow::Result<BoxService<Request, Response, rama::error::BoxError>> {
    let tls_config = build_upstream_tls(cfg, http_mode)?.connector_data;
    let transport_connector = TcpConnector::new().with_connector({
        let connect_timeout = cfg.connect_timeout;
        move |addr| async move {
            let stream =
                tokio::time::timeout(connect_timeout, tokio::net::TcpStream::connect(addr))
                    .await
                    .map_err(|_| std::io::Error::other("connect timeout"))??;
            Ok::<TcpStream, std::io::Error>(stream.into())
        }
    });
    let pool_config = HttpPooledConnectorConfig {
        idle_timeout: Some(cfg.idle_pool_timeout),
        wait_for_pool_timeout: Some(cfg.request_timeout),
        ..Default::default()
    };

    let builder = EasyHttpWebClient::connector_builder()
        .with_custom_transport_connector(transport_connector)
        .without_tls_proxy_support()
        .without_proxy_support()
        .with_tls_support_using_rustls_and_default_http_version(
            Some(tls_config),
            cfg.upstream_default_version(),
        )
        .with_default_http_connector::<Body>()
        .try_with_connection_pool(pool_config)
        .context("enable upstream connection pool")?;

    let client = AddRequiredRequestHeadersLayer::new()
        .into_layer(builder.build_client())
        .boxed();

    Ok(client)
}
