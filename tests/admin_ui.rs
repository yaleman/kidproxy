mod common;

use anyhow::Context;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use kidproxy::admin::{self, AdminState};
use kidproxy::config::AppConfigFile;
use kidproxy::entity;
use kidproxy::runtime_manager::RuntimeManager;
use kidproxy::transform::{
    TransformActionFile, TransformMatcherFile, TransformRuleFile, TransformTargetFile,
};
use sea_orm::EntityTrait;
use tower::ServiceExt;

use common::{proxy_url, start_admin_runtime_harness, test_client};

#[tokio::test(flavor = "multi_thread")]
async fn root_redirects_to_results_and_pages_are_split() -> anyhow::Result<()> {
    let mut harness = start_admin_runtime_harness().await?;
    let runtime_manager = RuntimeManager::start(harness.config_path.clone()).await?;
    let app = admin::router(AdminState::new(runtime_manager.clone()));

    let root_response = app
        .clone()
        .oneshot(Request::builder().uri("/").body(Body::empty())?)
        .await
        .context("load root page")?;
    assert_eq!(root_response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        root_response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/results")
    );

    let results_response = app
        .clone()
        .oneshot(Request::builder().uri("/results").body(Body::empty())?)
        .await
        .context("load results page")?;
    assert_eq!(results_response.status(), StatusCode::OK);
    let results_body = response_text(results_response).await?;
    assert!(results_body.contains("Captured results"));
    assert!(results_body.contains("href=\"/transforms\""));
    assert!(results_body.contains("Last n hours"));
    assert!(results_body.contains("<option value=\"success\">Success</option>"));

    let config_response = app
        .clone()
        .oneshot(Request::builder().uri("/config").body(Body::empty())?)
        .await
        .context("load config page")?;
    assert_eq!(config_response.status(), StatusCode::OK);
    let config_body = response_text(config_response).await?;
    assert!(config_body.contains("Proxy config"));
    assert!(config_body.contains("Reload proxy"));
    assert!(!config_body.contains("Add transform"));
    assert!(!config_body.contains("Matcher pattern"));

    let transforms_response = app
        .clone()
        .oneshot(Request::builder().uri("/transforms").body(Body::empty())?)
        .await
        .context("load transforms page")?;
    assert_eq!(transforms_response.status(), StatusCode::OK);
    let transforms_body = response_text(transforms_response).await?;
    assert!(transforms_body.contains("Add transform"));
    assert!(transforms_body.contains("No transforms configured yet."));
    assert!(!transforms_body.contains("Listen address"));

    runtime_manager.clone().shutdown().await?;
    harness.shutdown().await
}

#[tokio::test(flavor = "multi_thread")]
async fn config_page_save_and_reload_work() -> anyhow::Result<()> {
    let mut harness = start_admin_runtime_harness().await?;
    let mut seeded = AppConfigFile::load_json(&harness.config_path)?;
    seeded.transforms.push(TransformRuleFile {
        matcher: TransformMatcherFile::Everything {},
        action: TransformActionFile::Replace {
            from: "backend".to_owned(),
            to: "proxy".to_owned(),
        },
        target: TransformTargetFile::Body {},
        stop: false,
    });
    seeded.write_json(&harness.config_path)?;

    let runtime_manager = RuntimeManager::start(harness.config_path.clone()).await?;
    let app = admin::router(AdminState::new(runtime_manager.clone()));

    let config_response = app
        .clone()
        .oneshot(Request::builder().uri("/config").body(Body::empty())?)
        .await
        .context("load config page")?;
    assert_eq!(config_response.status(), StatusCode::OK);
    let config_body = response_text(config_response).await?;
    assert!(config_body.contains("Proxy config"));
    assert!(config_body.contains("Reload proxy"));

    let save_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/config")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(form_body(&[
                    ("editor_action", "save"),
                    ("frontend_domain", "admin-updated.local"),
                ])))?,
        )
        .await
        .context("submit config save")?;
    assert_eq!(save_response.status(), StatusCode::OK);
    let save_body = response_text(save_response).await?;
    assert!(save_body.contains("Config saved. Reload is required to apply it."));

    let saved = AppConfigFile::load_json(&harness.config_path)?;
    assert_eq!(saved.runtime.frontend_domain, "admin-updated.local");
    assert_eq!(saved.transforms.len(), 1);
    assert!(runtime_manager.clone().state().await.pending_reload);

    let reload_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/config")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(form_body(&[("editor_action", "reload")])))?,
        )
        .await
        .context("submit config reload")?;
    assert_eq!(reload_response.status(), StatusCode::OK);
    let reload_body = response_text(reload_response).await?;
    assert!(reload_body.contains("Proxy config"));
    let runtime_state = runtime_manager.clone().state().await;
    assert!(
        !runtime_state.pending_reload,
        "reload stayed pending: {:?}\n{}",
        runtime_state.last_reload_error, reload_body
    );
    assert_eq!(
        runtime_state.active_runtime.frontend_domain,
        "admin-updated.local"
    );

    runtime_manager.clone().shutdown().await?;
    harness.shutdown().await
}

#[tokio::test(flavor = "multi_thread")]
async fn transforms_page_save_and_reload_work() -> anyhow::Result<()> {
    let mut harness = start_admin_runtime_harness().await?;
    let initial = AppConfigFile::load_json(&harness.config_path)?;
    let initial_frontend_domain = initial.runtime.frontend_domain.clone();
    let runtime_manager = RuntimeManager::start(harness.config_path.clone()).await?;
    let app = admin::router(AdminState::new(runtime_manager.clone()));

    let transforms_response = app
        .clone()
        .oneshot(Request::builder().uri("/transforms").body(Body::empty())?)
        .await
        .context("load transforms page")?;
    assert_eq!(transforms_response.status(), StatusCode::OK);
    let transforms_body = response_text(transforms_response).await?;
    assert!(transforms_body.contains("Add transform"));

    let save_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/transforms")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(form_body(&[
                    ("editor_action", "save"),
                    ("transform_count", "1"),
                    ("0", "unused"),
                    ("transform_0_present", "true"),
                    ("transform_0_matcher_type", "any"),
                    ("transform_0_matcher_pattern", ""),
                    ("transform_0_action_from", "backend"),
                    ("transform_0_action_to", "proxy"),
                    ("transform_0_target_type", "body"),
                    ("transform_0_target_name", ""),
                    ("transform_0_stop", "true"),
                ])))?,
        )
        .await
        .context("submit transforms save")?;
    assert_eq!(save_response.status(), StatusCode::OK);
    let save_body = response_text(save_response).await?;
    assert!(save_body.contains("Config saved. Reload is required to apply it."));

    let saved = AppConfigFile::load_json(&harness.config_path)?;
    assert_eq!(saved.runtime.frontend_domain, initial_frontend_domain);
    assert_eq!(saved.transforms.len(), 1);
    assert!(runtime_manager.clone().state().await.pending_reload);

    let reload_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/transforms")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(form_body(&[("editor_action", "reload")])))?,
        )
        .await
        .context("submit transforms reload")?;
    assert_eq!(reload_response.status(), StatusCode::OK);
    let reload_body = response_text(reload_response).await?;
    assert!(reload_body.contains("Transforms"));
    let runtime_state = runtime_manager.clone().state().await;
    assert!(
        !runtime_state.pending_reload,
        "reload stayed pending: {:?}\n{}",
        runtime_state.last_reload_error, reload_body
    );
    assert_eq!(runtime_state.active_runtime.transform_count, 1);
    assert_eq!(
        runtime_state.active_runtime.frontend_domain,
        initial_frontend_domain
    );

    runtime_manager.clone().shutdown().await?;
    harness.shutdown().await
}

#[tokio::test(flavor = "multi_thread")]
async fn results_pages_render_captured_events() -> anyhow::Result<()> {
    let mut harness = start_admin_runtime_harness().await?;
    let runtime_manager = RuntimeManager::start(harness.config_path.clone()).await?;
    let app = admin::router(AdminState::new(runtime_manager.clone()));
    let proxy_addr = harness.proxy_addr()?;
    let client = test_client(&harness.frontend_cert.cert_pem, proxy_addr)?;

    let response = client
        .get(proxy_url(proxy_addr, "/hello"))
        .send()
        .await
        .context("send proxied request")?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.text().await?, "hello from backend");

    let event = wait_for_logged_event(runtime_manager.clone()).await?;

    let results_response = app
        .clone()
        .oneshot(Request::builder().uri("/results").body(Body::empty())?)
        .await
        .context("load results page")?;
    assert_eq!(results_response.status(), StatusCode::OK);
    let results_body = response_text(results_response).await?;
    assert!(results_body.contains("Captured results"));
    assert!(results_body.contains("/hello"));
    assert!(results_body.contains(&event.event_id));
    assert!(results_body.contains("href=\"/transforms\""));

    let detail_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/results/{}", event.event_id))
                .body(Body::empty())?,
        )
        .await
        .context("load result detail page")?;
    assert_eq!(detail_response.status(), StatusCode::OK);
    let detail_body = response_text(detail_response).await?;
    assert!(detail_body.contains("GET /hello"));
    assert!(detail_body.contains("hello from backend"));
    assert!(detail_body.contains("Request headers"));

    runtime_manager.clone().shutdown().await?;
    harness.shutdown().await
}

#[tokio::test(flavor = "multi_thread")]
async fn results_page_accepts_blank_numeric_filters() -> anyhow::Result<()> {
    let mut harness = start_admin_runtime_harness().await?;
    let runtime_manager = RuntimeManager::start(harness.config_path.clone()).await?;
    let app = admin::router(AdminState::new(runtime_manager.clone()));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/results?method=GET&path_substring=&status_code=&proxy_result=&since_hours=&page=0")
                .body(Body::empty())?,
        )
        .await
        .context("load results page with blank numeric filters")?;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_text(response).await?;
    assert!(body.contains("Captured results"));
    assert!(body.contains("Last n hours"));

    runtime_manager.clone().shutdown().await?;
    harness.shutdown().await
}

async fn wait_for_logged_event(runtime_manager: RuntimeManager) -> anyhow::Result<entity::Model> {
    let started = std::time::Instant::now();
    loop {
        let database = runtime_manager.clone().active_database().await?;
        let rows = entity::Entity::find()
            .all(&database)
            .await
            .context("query logged events")?;
        if let Some(event) = rows.into_iter().next() {
            return Ok(event);
        }
        if started.elapsed() > std::time::Duration::from_secs(5) {
            anyhow::bail!("timed out waiting for logged event");
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

async fn response_text(response: axum::response::Response) -> anyhow::Result<String> {
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await?;
    String::from_utf8(bytes.to_vec()).context("response body was not utf-8")
}

fn form_body(fields: &[(&str, &str)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in fields {
        serializer.append_pair(name, value);
    }
    serializer.finish()
}
