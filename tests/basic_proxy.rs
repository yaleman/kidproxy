mod common;

use anyhow::Context;
use common::{proxy_url, start_proxy_harness, test_client};
use std::sync::atomic::Ordering;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn proxies_get_post_and_preserves_location() -> anyhow::Result<()> {
    let mut harness = start_proxy_harness(1024, 1).await?;
    let client = test_client(&harness.frontend_cert.cert_pem, harness.proxy_addr())?;

    let hello = client
        .get(proxy_url(harness.proxy_addr(), "/hello"))
        .header("host", "example.test")
        .send()
        .await
        .context("send hello request")?;
    let hello_status = hello.status();
    let hello_body = hello.text().await.context("read hello response")?;
    assert_eq!(hello_status, reqwest::StatusCode::OK);
    assert_eq!(hello_body, "hello from backend");

    let echo = client
        .post(proxy_url(harness.proxy_addr(), "/echo"))
        .header("host", "example.test")
        .body("proxy body")
        .send()
        .await
        .context("send echo request")?;
    let echo_status = echo.status();
    let echo_body = echo.text().await.context("read echo response")?;
    assert_eq!(echo_status, reqwest::StatusCode::OK);
    assert_eq!(echo_body, "proxy body");

    let redirect = client
        .get(proxy_url(harness.proxy_addr(), "/redirect"))
        .header("host", "example.test")
        .send()
        .await
        .context("send redirect request")?;
    assert_eq!(redirect.status(), reqwest::StatusCode::FOUND);
    assert_eq!(
        redirect
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("https://backend.local/elsewhere")
    );

    harness.shutdown().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rejects_host_mismatch_without_hitting_upstream() -> anyhow::Result<()> {
    let mut harness = start_proxy_harness(1024, 1).await?;
    let client = test_client(&harness.frontend_cert.cert_pem, harness.proxy_addr())?;
    let baseline_hits = harness.backend.hits.load(Ordering::Relaxed);

    let response = client
        .get(proxy_url(harness.proxy_addr(), "/hello"))
        .header("host", "wrong.test")
        .send()
        .await
        .context("send host mismatch request")?;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("read host mismatch response")?;

    assert_eq!(status, reqwest::StatusCode::MISDIRECTED_REQUEST);
    assert_eq!(body, "misdirected request");
    assert_eq!(harness.backend.hits.load(Ordering::Relaxed), baseline_hits);

    harness.shutdown().await
}
