mod common;

use anyhow::Context;
use common::{proxy_url, read_logged_events, start_proxy_harness, test_client};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writes_one_row_per_completed_exchange_with_sqlite_fields() -> anyhow::Result<()> {
    let mut harness = start_proxy_harness(8, 16).await?;
    let client = test_client(&harness.frontend_cert.cert_pem, harness.proxy_addr())?;

    let ok_response = client
        .post(proxy_url(harness.proxy_addr(), "/big-response"))
        .header("host", "example.test")
        .body("request-body-is-longer-than-preview")
        .send()
        .await
        .context("send retained-body request")?;
    assert_eq!(ok_response.status(), reqwest::StatusCode::OK);
    let ok_body = ok_response
        .text()
        .await
        .context("read retained-body response")?;
    assert_eq!(ok_body, "abcdefghijklmnopqrstuvwxyz");

    let bad_response = client
        .get(proxy_url(harness.proxy_addr(), "/missing"))
        .header("host", "example.test")
        .send()
        .await
        .context("send missing request")?;
    assert_eq!(bad_response.status(), reqwest::StatusCode::NOT_FOUND);

    harness.shutdown().await?;
    let rows = read_logged_events(&harness.writer).await?;
    assert_eq!(rows.len(), 2);

    let success_row = rows
        .iter()
        .find(|row| row.status_code == Some(200))
        .context("expected a logged 200 response row")?;
    let not_found_row = rows
        .iter()
        .find(|row| row.status_code == Some(404))
        .context("expected a logged 404 response row")?;

    assert!(!success_row.event_id.is_empty());
    assert!(!success_row.request_headers_json.is_empty());
    assert!(success_row.response_headers_json.is_some());
    assert!(success_row.request_body_preview_base64.is_some());
    assert!(success_row.response_body_preview_base64.is_some());
    assert_eq!(success_row.request_body_truncated, Some(true));
    assert_eq!(success_row.response_body_truncated, Some(true));
    assert_eq!(success_row.proxy_result, "success");

    assert_eq!(not_found_row.proxy_result, "success");
    assert_eq!(not_found_row.reason_phrase.as_deref(), Some("Not Found"));

    Ok(())
}
