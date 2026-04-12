mod common;

use anyhow::Context;
use arrow::array::{Array, StringArray};
use common::{proxy_url, read_parquet_batches, start_proxy_harness, test_client};
use futures::{StreamExt, stream};
use std::time::{Duration, Instant};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streams_response_without_full_buffering() -> anyhow::Result<()> {
    let mut harness = start_proxy_harness(1024, 1).await?;
    let client = test_client(&harness.frontend_cert.cert_pem, harness.proxy_addr())?;

    let started = Instant::now();
    let response = client
        .get(proxy_url(harness.proxy_addr(), "/stream"))
        .header("host", "example.test")
        .send()
        .await
        .context("send stream request")?;
    let mut body_stream = response.bytes_stream();

    let first = body_stream
        .next()
        .await
        .context("missing first response chunk")?
        .context("read first response chunk")?;
    let after_first = started.elapsed();

    let second = body_stream
        .next()
        .await
        .context("missing second response chunk")?
        .context("read second response chunk")?;
    let third = body_stream
        .next()
        .await
        .context("missing third response chunk")?
        .context("read third response chunk")?;
    let finished = started.elapsed();

    assert_eq!(first.as_ref(), b"first-");
    assert_eq!(second.as_ref(), b"second-");
    assert_eq!(third.as_ref(), b"third");
    assert!(after_first < Duration::from_millis(280));
    assert!(finished >= Duration::from_millis(280));

    harness.shutdown().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hashes_streamed_request_body() -> anyhow::Result<()> {
    let mut harness = start_proxy_harness(1024, 1).await?;
    let client = test_client(&harness.frontend_cert.cert_pem, harness.proxy_addr())?;
    let body_stream = stream::iter(vec![
        Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"stream-")),
        Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"body")),
    ]);

    let response = client
        .post(proxy_url(harness.proxy_addr(), "/echo"))
        .header("host", "example.test")
        .body(reqwest::Body::wrap_stream(body_stream))
        .send()
        .await
        .context("send streaming echo request")?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.text().await.context("read streaming echo")?,
        "stream-body"
    );

    harness.shutdown().await?;
    let batches = read_parquet_batches(harness.parquet_dir())?;
    let request_hashes = batches
        .iter()
        .flat_map(|batch| {
            let column = batch
                .column_by_name("request_body_sha256")
                .and_then(|array| array.as_any().downcast_ref::<StringArray>());
            let Some(column) = column else {
                return Vec::<String>::new().into_iter();
            };
            (0..column.len())
                .filter_map(|index| {
                    (!column.is_null(index)).then(|| column.value(index).to_owned())
                })
                .collect::<Vec<_>>()
                .into_iter()
        })
        .collect::<Vec<_>>();

    assert!(
        request_hashes.iter().any(|value| !value.is_empty()),
        "expected at least one request body hash in parquet output"
    );

    Ok(())
}
