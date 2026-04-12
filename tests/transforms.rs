mod common;

use anyhow::Context;
use common::{proxy_url, start_proxy_harness_with_config, test_client};
use futures::StreamExt;
use serde_json::json;
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn write_transform_config(
    tempdir: &TempDir,
    value: serde_json::Value,
) -> anyhow::Result<std::path::PathBuf> {
    let path = tempdir.path().join("transforms.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&value)?)
        .with_context(|| format!("write transform config {}", path.display()))?;
    Ok(path)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rewrites_headers_without_buffering_streams() -> anyhow::Result<()> {
    let tempdir = TempDir::new().context("create tempdir")?;
    let config_path = write_transform_config(
        &tempdir,
        json!({
            "transforms": [
                {
                    "matcher": { "type": "url_glob", "pattern": "/stream" },
                    "action": { "type": "replace", "from": "text/plain", "to": "text/stream" },
                    "target": { "type": "header", "name": "content-type" }
                }
            ]
        }),
    )?;
    let mut harness = start_proxy_harness_with_config(1024, 1, Some(config_path)).await?;
    let client = test_client(&harness.frontend_cert.cert_pem, harness.proxy_addr())?;

    let started = Instant::now();
    let response = client
        .get(proxy_url(harness.proxy_addr(), "/stream"))
        .header("host", "example.test")
        .send()
        .await
        .context("send transformed stream request")?;
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/stream")
    );
    let mut body_stream = response.bytes_stream();

    let first = body_stream
        .next()
        .await
        .context("missing first chunk")?
        .context("read first chunk")?;
    let after_first = started.elapsed();

    assert_eq!(first.as_ref(), b"first-");
    assert!(after_first < Duration::from_millis(280));

    harness.shutdown().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rewrites_response_body_and_updates_content_length() -> anyhow::Result<()> {
    let tempdir = TempDir::new().context("create tempdir")?;
    let config_path = write_transform_config(
        &tempdir,
        json!({
            "transforms": [
                {
                    "matcher": { "type": "url_glob", "pattern": "/hello" },
                    "action": { "type": "replace", "from": "backend", "to": "proxy" },
                    "target": { "type": "body" }
                }
            ]
        }),
    )?;
    let mut harness = start_proxy_harness_with_config(1024, 1, Some(config_path)).await?;
    let client = test_client(&harness.frontend_cert.cert_pem, harness.proxy_addr())?;

    let response = client
        .get(proxy_url(harness.proxy_addr(), "/hello"))
        .header("host", "example.test")
        .send()
        .await
        .context("send body rewrite request")?;
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok()),
        Some("16")
    );
    assert_eq!(
        response.text().await.context("read rewritten body")?,
        "hello from proxy"
    );

    harness.shutdown().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rewrites_everything_target() -> anyhow::Result<()> {
    let tempdir = TempDir::new().context("create tempdir")?;
    let config_path = write_transform_config(
        &tempdir,
        json!({
            "transforms": [
                {
                    "matcher": { "type": "url_glob", "pattern": "/headers" },
                    "action": { "type": "replace", "from": "backend", "to": "proxy" },
                    "target": { "type": "everything" }
                }
            ]
        }),
    )?;
    let mut harness = start_proxy_harness_with_config(1024, 1, Some(config_path)).await?;
    let client = test_client(&harness.frontend_cert.cert_pem, harness.proxy_addr())?;

    let response = client
        .get(proxy_url(harness.proxy_addr(), "/headers"))
        .header("host", "example.test")
        .send()
        .await
        .context("send everything rewrite request")?;
    assert_eq!(
        response
            .headers()
            .get("x-test")
            .and_then(|value| value.to_str().ok()),
        Some("proxy header")
    );
    assert_eq!(
        response.text().await.context("read everything body")?,
        "proxy header body"
    );

    harness.shutdown().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leaves_non_matching_rule_unchanged() -> anyhow::Result<()> {
    let tempdir = TempDir::new().context("create tempdir")?;
    let config_path = write_transform_config(
        &tempdir,
        json!({
            "transforms": [
                {
                    "matcher": { "type": "url_glob", "pattern": "/nope*" },
                    "action": { "type": "replace", "from": "backend", "to": "proxy" },
                    "target": { "type": "everything" }
                }
            ]
        }),
    )?;
    let mut harness = start_proxy_harness_with_config(1024, 1, Some(config_path)).await?;
    let client = test_client(&harness.frontend_cert.cert_pem, harness.proxy_addr())?;

    let response = client
        .get(proxy_url(harness.proxy_addr(), "/hello"))
        .header("host", "example.test")
        .send()
        .await
        .context("send non-matching request")?;
    assert_eq!(
        response.text().await.context("read untouched body")?,
        "hello from backend"
    );

    harness.shutdown().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn matched_body_rule_buffers_streaming_response() -> anyhow::Result<()> {
    let tempdir = TempDir::new().context("create tempdir")?;
    let config_path = write_transform_config(
        &tempdir,
        json!({
            "transforms": [
                {
                    "matcher": { "type": "url_glob", "pattern": "/stream" },
                    "action": { "type": "replace", "from": "second", "to": "middle" },
                    "target": { "type": "body" }
                }
            ]
        }),
    )?;
    let mut harness = start_proxy_harness_with_config(1024, 1, Some(config_path)).await?;
    let client = test_client(&harness.frontend_cert.cert_pem, harness.proxy_addr())?;

    let started = Instant::now();
    let response = client
        .get(proxy_url(harness.proxy_addr(), "/stream"))
        .header("host", "example.test")
        .send()
        .await
        .context("send buffered stream request")?;
    let first_chunk = response
        .bytes_stream()
        .next()
        .await
        .context("missing buffered first chunk")?
        .context("read buffered first chunk")?;
    let after_first = started.elapsed();

    assert!(after_first >= Duration::from_millis(280));
    assert_eq!(first_chunk.as_ref(), b"first-middle-third");

    harness.shutdown().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_rule_prevents_later_rules() -> anyhow::Result<()> {
    let tempdir = TempDir::new().context("create tempdir")?;
    let config_path = write_transform_config(
        &tempdir,
        json!({
            "transforms": [
                {
                    "matcher": { "type": "url_glob", "pattern": "/headers" },
                    "action": { "type": "replace", "from": "backend", "to": "proxy" },
                    "target": { "type": "header", "name": "x-test" },
                    "stop": true
                },
                {
                    "matcher": { "type": "url_glob", "pattern": "/headers" },
                    "action": { "type": "replace", "from": "proxy", "to": "final" },
                    "target": { "type": "header", "name": "x-test" }
                }
            ]
        }),
    )?;
    let mut harness = start_proxy_harness_with_config(1024, 1, Some(config_path)).await?;
    let client = test_client(&harness.frontend_cert.cert_pem, harness.proxy_addr())?;

    let response = client
        .get(proxy_url(harness.proxy_addr(), "/headers"))
        .header("host", "example.test")
        .send()
        .await
        .context("send stop-rule request")?;
    assert_eq!(
        response
            .headers()
            .get("x-test")
            .and_then(|value| value.to_str().ok()),
        Some("proxy header")
    );

    harness.shutdown().await
}
