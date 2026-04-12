mod common;

use anyhow::Context;
use arrow::array::{Array, BooleanArray, StringArray, UInt16Array};
use common::{proxy_url, read_parquet_batches, start_proxy_harness, test_client};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writes_one_row_per_completed_exchange_with_preview_fields() -> anyhow::Result<()> {
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
    let batches = read_parquet_batches(harness.parquet_dir())?;
    let total_rows: usize = batches.iter().map(|batch| batch.num_rows()).sum();
    assert_eq!(total_rows, 2);

    let first_batch = batches
        .first()
        .context("expected at least one parquet batch")?;
    assert!(first_batch.column_by_name("event_id").is_some());
    assert!(first_batch.column_by_name("request_headers_json").is_some());
    assert!(
        first_batch
            .column_by_name("response_headers_json")
            .is_some()
    );

    let status_codes = first_batch
        .column_by_name("status_code")
        .and_then(|array| array.as_any().downcast_ref::<UInt16Array>())
        .context("status_code column missing or wrong type")?;
    let request_previews = first_batch
        .column_by_name("request_body_preview_base64")
        .and_then(|array| array.as_any().downcast_ref::<StringArray>())
        .context("request_body_preview_base64 column missing or wrong type")?;
    let response_previews = first_batch
        .column_by_name("response_body_preview_base64")
        .and_then(|array| array.as_any().downcast_ref::<StringArray>())
        .context("response_body_preview_base64 column missing or wrong type")?;
    let request_truncated = first_batch
        .column_by_name("request_body_truncated")
        .and_then(|array| array.as_any().downcast_ref::<BooleanArray>())
        .context("request_body_truncated column missing or wrong type")?;
    let response_truncated = first_batch
        .column_by_name("response_body_truncated")
        .and_then(|array| array.as_any().downcast_ref::<BooleanArray>())
        .context("response_body_truncated column missing or wrong type")?;
    let proxy_results = first_batch
        .column_by_name("proxy_result")
        .and_then(|array| array.as_any().downcast_ref::<StringArray>())
        .context("proxy_result column missing or wrong type")?;

    let statuses = (0..status_codes.len())
        .filter_map(|index| (!status_codes.is_null(index)).then(|| status_codes.value(index)))
        .collect::<Vec<_>>();
    assert!(statuses.contains(&200));
    assert!(statuses.contains(&404));

    let has_request_preview = (0..request_previews.len())
        .any(|index| !request_previews.is_null(index) && !request_previews.value(index).is_empty());
    let has_response_preview = (0..response_previews.len()).any(|index| {
        !response_previews.is_null(index) && !response_previews.value(index).is_empty()
    });
    let has_truncated_request = (0..request_truncated.len())
        .any(|index| !request_truncated.is_null(index) && request_truncated.value(index));
    let has_truncated_response = (0..response_truncated.len())
        .any(|index| !response_truncated.is_null(index) && response_truncated.value(index));
    let proxy_results = (0..proxy_results.len())
        .map(|index| proxy_results.value(index).to_owned())
        .collect::<Vec<_>>();

    assert!(has_request_preview);
    assert!(has_response_preview);
    assert!(has_truncated_request);
    assert!(has_truncated_response);
    assert!(proxy_results.iter().any(|value| value == "success"));

    Ok(())
}
