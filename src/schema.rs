use crate::event::ProxyEvent;
use anyhow::Context;
use arrow::array::{
    ArrayRef, BooleanArray, Int64Array, StringArray, TimestampMillisecondArray, UInt16Array,
};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::record_batch::RecordBatch;
use std::sync::Arc;
use time::OffsetDateTime;

pub fn parquet_schema() -> Schema {
    let timestamp_tz = Some("UTC".into());
    Schema::new(vec![
        Field::new("event_id", DataType::Utf8, false),
        Field::new("connection_id", DataType::Utf8, false),
        Field::new(
            "request_start_ts",
            DataType::Timestamp(TimeUnit::Millisecond, timestamp_tz.clone()),
            false,
        ),
        Field::new(
            "request_end_ts",
            DataType::Timestamp(TimeUnit::Millisecond, timestamp_tz.clone()),
            false,
        ),
        Field::new("duration_ms", DataType::Int64, false),
        Field::new("client_ip", DataType::Utf8, true),
        Field::new("client_port", DataType::UInt16, true),
        Field::new("proxy_local_ip", DataType::Utf8, true),
        Field::new("proxy_local_port", DataType::UInt16, true),
        Field::new("frontend_server_name", DataType::Utf8, true),
        Field::new("frontend_http_version", DataType::Utf8, true),
        Field::new("frontend_scheme", DataType::Utf8, false),
        Field::new("backend_url", DataType::Utf8, false),
        Field::new("backend_host", DataType::Utf8, false),
        Field::new("backend_ip", DataType::Utf8, true),
        Field::new("backend_port", DataType::UInt16, true),
        Field::new("backend_http_version", DataType::Utf8, true),
        Field::new("upstream_connection_reused", DataType::Boolean, true),
        Field::new("method", DataType::Utf8, false),
        Field::new("authority", DataType::Utf8, true),
        Field::new("path", DataType::Utf8, false),
        Field::new("query", DataType::Utf8, true),
        Field::new("request_headers_json", DataType::Utf8, false),
        Field::new("request_cookie_header", DataType::Utf8, true),
        Field::new("request_cookies_json", DataType::Utf8, true),
        Field::new("request_content_length", DataType::Int64, true),
        Field::new("request_transfer_encoding", DataType::Utf8, true),
        Field::new("request_content_type", DataType::Utf8, true),
        Field::new("request_body_sha256", DataType::Utf8, true),
        Field::new("request_body_preview_base64", DataType::Utf8, true),
        Field::new("request_body_truncated", DataType::Boolean, true),
        Field::new("status_code", DataType::UInt16, true),
        Field::new("reason_phrase", DataType::Utf8, true),
        Field::new("response_headers_json", DataType::Utf8, true),
        Field::new("response_set_cookie_json", DataType::Utf8, true),
        Field::new("response_content_length", DataType::Int64, true),
        Field::new("response_transfer_encoding", DataType::Utf8, true),
        Field::new("response_content_type", DataType::Utf8, true),
        Field::new("response_content_encoding", DataType::Utf8, true),
        Field::new("response_body_sha256", DataType::Utf8, true),
        Field::new("response_body_preview_base64", DataType::Utf8, true),
        Field::new("response_body_truncated", DataType::Boolean, true),
        Field::new("frontend_tls_version", DataType::Utf8, true),
        Field::new("frontend_tls_cipher_suite", DataType::Utf8, true),
        Field::new("frontend_tls_alpn", DataType::Utf8, true),
        Field::new("frontend_tls_sni", DataType::Utf8, true),
        Field::new("frontend_tls_ja3", DataType::Utf8, true),
        Field::new("frontend_tls_ja4", DataType::Utf8, true),
        Field::new("backend_tls_version", DataType::Utf8, true),
        Field::new("backend_tls_cipher_suite", DataType::Utf8, true),
        Field::new("backend_tls_alpn", DataType::Utf8, true),
        Field::new("backend_tls_sni", DataType::Utf8, true),
        Field::new("backend_cert_leaf_sha256", DataType::Utf8, true),
        Field::new("backend_cert_subject", DataType::Utf8, true),
        Field::new("backend_cert_issuer", DataType::Utf8, true),
        Field::new(
            "backend_cert_not_before",
            DataType::Timestamp(TimeUnit::Millisecond, timestamp_tz.clone()),
            true,
        ),
        Field::new(
            "backend_cert_not_after",
            DataType::Timestamp(TimeUnit::Millisecond, timestamp_tz),
            true,
        ),
        Field::new("dns_duration_ms", DataType::Int64, true),
        Field::new("connect_duration_ms", DataType::Int64, true),
        Field::new("tls_handshake_duration_ms", DataType::Int64, true),
        Field::new("upstream_first_byte_ms", DataType::Int64, true),
        Field::new("response_stream_duration_ms", DataType::Int64, true),
        Field::new("error_kind", DataType::Utf8, true),
        Field::new("error_text", DataType::Utf8, true),
        Field::new("proxy_result", DataType::Utf8, false),
    ])
}

pub fn events_to_record_batch(events: &[ProxyEvent]) -> anyhow::Result<RecordBatch> {
    let schema = Arc::new(parquet_schema());
    let columns: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(strings(events, |event| {
            event.event_id.to_string()
        }))),
        Arc::new(StringArray::from(strings(events, |event| {
            event.connection_id.to_string()
        }))),
        Arc::new(
            TimestampMillisecondArray::from(
                events
                    .iter()
                    .map(|event| timestamp_ms(event.request_start_ts))
                    .collect::<Vec<_>>(),
            )
            .with_timezone("UTC"),
        ),
        Arc::new(
            TimestampMillisecondArray::from(
                events
                    .iter()
                    .map(|event| timestamp_ms(event.request_end_ts))
                    .collect::<Vec<_>>(),
            )
            .with_timezone("UTC"),
        ),
        Arc::new(Int64Array::from(ints(events, |event| event.duration_ms))),
        Arc::new(StringArray::from(options(events, |event| {
            event.client_ip.clone()
        }))),
        Arc::new(UInt16Array::from(options(events, |event| {
            event.client_port
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.proxy_local_ip.clone()
        }))),
        Arc::new(UInt16Array::from(options(events, |event| {
            event.proxy_local_port
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.frontend_server_name.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.frontend_http_version.clone()
        }))),
        Arc::new(StringArray::from(strings(events, |event| {
            event.frontend_scheme.clone()
        }))),
        Arc::new(StringArray::from(strings(events, |event| {
            event.backend_url.clone()
        }))),
        Arc::new(StringArray::from(strings(events, |event| {
            event.backend_host.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.backend_ip.clone()
        }))),
        Arc::new(UInt16Array::from(options(events, |event| {
            event.backend_port
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.backend_http_version.clone()
        }))),
        Arc::new(BooleanArray::from(options(events, |event| {
            event.upstream_connection_reused
        }))),
        Arc::new(StringArray::from(strings(events, |event| {
            event.method.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.authority.clone()
        }))),
        Arc::new(StringArray::from(strings(events, |event| {
            event.path.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.query.clone()
        }))),
        Arc::new(StringArray::from(strings(events, |event| {
            event.request_headers_json.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.request_cookie_header.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.request_cookies_json.clone()
        }))),
        Arc::new(Int64Array::from(options(events, |event| {
            event.request_content_length
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.request_transfer_encoding.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.request_content_type.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.request_body_sha256.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.request_body_preview_base64.clone()
        }))),
        Arc::new(BooleanArray::from(options(events, |event| {
            event.request_body_truncated
        }))),
        Arc::new(UInt16Array::from(options(events, |event| {
            event.status_code
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.reason_phrase.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.response_headers_json.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.response_set_cookie_json.clone()
        }))),
        Arc::new(Int64Array::from(options(events, |event| {
            event.response_content_length
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.response_transfer_encoding.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.response_content_type.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.response_content_encoding.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.response_body_sha256.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.response_body_preview_base64.clone()
        }))),
        Arc::new(BooleanArray::from(options(events, |event| {
            event.response_body_truncated
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.frontend_tls_version.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.frontend_tls_cipher_suite.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.frontend_tls_alpn.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.frontend_tls_sni.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.frontend_tls_ja3.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.frontend_tls_ja4.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.backend_tls_version.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.backend_tls_cipher_suite.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.backend_tls_alpn.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.backend_tls_sni.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.backend_cert_leaf_sha256.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.backend_cert_subject.clone()
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.backend_cert_issuer.clone()
        }))),
        Arc::new(
            TimestampMillisecondArray::from(options(events, |event| {
                event.backend_cert_not_before.map(timestamp_ms)
            }))
            .with_timezone("UTC"),
        ),
        Arc::new(
            TimestampMillisecondArray::from(options(events, |event| {
                event.backend_cert_not_after.map(timestamp_ms)
            }))
            .with_timezone("UTC"),
        ),
        Arc::new(Int64Array::from(options(events, |event| {
            event.dns_duration_ms
        }))),
        Arc::new(Int64Array::from(options(events, |event| {
            event.connect_duration_ms
        }))),
        Arc::new(Int64Array::from(options(events, |event| {
            event.tls_handshake_duration_ms
        }))),
        Arc::new(Int64Array::from(options(events, |event| {
            event.upstream_first_byte_ms
        }))),
        Arc::new(Int64Array::from(options(events, |event| {
            event.response_stream_duration_ms
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event
                .error_kind
                .map(|error_kind| error_kind.as_str().to_owned())
        }))),
        Arc::new(StringArray::from(options(events, |event| {
            event.error_text.clone()
        }))),
        Arc::new(StringArray::from(strings(events, |event| {
            event.proxy_result.as_str().to_owned()
        }))),
    ];

    RecordBatch::try_new(schema, columns).context("create arrow record batch")
}

fn strings<F>(events: &[ProxyEvent], map: F) -> Vec<String>
where
    F: Fn(&ProxyEvent) -> String,
{
    events.iter().map(map).collect()
}

fn ints<F>(events: &[ProxyEvent], map: F) -> Vec<i64>
where
    F: Fn(&ProxyEvent) -> i64,
{
    events.iter().map(map).collect()
}

fn options<T, F>(events: &[ProxyEvent], map: F) -> Vec<Option<T>>
where
    F: Fn(&ProxyEvent) -> Option<T>,
{
    events.iter().map(map).collect()
}

fn timestamp_ms(value: OffsetDateTime) -> i64 {
    value.unix_timestamp_nanos() as i64 / 1_000_000
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ProxyResult;
    use crate::event::ProxyEvent;
    use time::OffsetDateTime;
    use uuid::Uuid;

    #[test]
    fn builds_record_batch_with_expected_schema() -> anyhow::Result<()> {
        let now = OffsetDateTime::now_utc();
        let batch = events_to_record_batch(&[ProxyEvent {
            event_id: Uuid::new_v4(),
            connection_id: Uuid::new_v4(),
            request_start_ts: now,
            request_end_ts: now,
            duration_ms: 1,
            client_ip: Some("127.0.0.1".to_owned()),
            client_port: Some(1234),
            proxy_local_ip: Some("127.0.0.1".to_owned()),
            proxy_local_port: Some(8443),
            frontend_server_name: Some("example.test".to_owned()),
            frontend_http_version: Some("HTTP/1.1".to_owned()),
            frontend_scheme: "https".to_owned(),
            backend_url: "https://backend.test".to_owned(),
            backend_host: "backend.test".to_owned(),
            backend_ip: Some("127.0.0.1".to_owned()),
            backend_port: Some(443),
            backend_http_version: Some("HTTP/1.1".to_owned()),
            upstream_connection_reused: Some(false),
            method: "GET".to_owned(),
            authority: Some("example.test".to_owned()),
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
            request_body_truncated: None,
            status_code: Some(200),
            reason_phrase: Some("OK".to_owned()),
            response_headers_json: Some("{}".to_owned()),
            response_set_cookie_json: None,
            response_content_length: Some(0),
            response_transfer_encoding: None,
            response_content_type: Some("text/plain".to_owned()),
            response_content_encoding: None,
            response_body_sha256: None,
            response_body_preview_base64: None,
            response_body_truncated: None,
            frontend_tls_version: Some("TLSv1.3".to_owned()),
            frontend_tls_cipher_suite: None,
            frontend_tls_alpn: Some("http/1.1".to_owned()),
            frontend_tls_sni: Some("example.test".to_owned()),
            frontend_tls_ja3: None,
            frontend_tls_ja4: None,
            backend_tls_version: Some("TLSv1.3".to_owned()),
            backend_tls_cipher_suite: None,
            backend_tls_alpn: Some("http/1.1".to_owned()),
            backend_tls_sni: Some("backend.test".to_owned()),
            backend_cert_leaf_sha256: None,
            backend_cert_subject: None,
            backend_cert_issuer: None,
            backend_cert_not_before: Some(now),
            backend_cert_not_after: Some(now),
            dns_duration_ms: None,
            connect_duration_ms: None,
            tls_handshake_duration_ms: None,
            upstream_first_byte_ms: Some(1),
            response_stream_duration_ms: Some(1),
            error_kind: None,
            error_text: None,
            proxy_result: ProxyResult::Success,
        }])?;

        assert_eq!(batch.num_rows(), 1);
        assert_eq!(
            batch
                .schema()
                .field_with_name("request_start_ts")?
                .data_type(),
            &DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into()))
        );
        assert!(batch.column_by_name("proxy_result").is_some());

        Ok(())
    }
}
