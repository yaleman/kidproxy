use crate::config::{AppConfigFile, RuntimeSettingsFile};
use crate::entity::{self, Column};
use crate::error::ProxyResult;
use crate::runtime_manager::{RuntimeManager, RuntimeManagerState};
use crate::transform::{
    TransformActionFile, TransformMatcherFile, TransformRuleFile, TransformTargetFile,
};
use anyhow::Context;
use axum::Router;
use axum::extract::{Form, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use base64::Engine;
use leptos::prelude::*;
use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Clone)]
pub struct AdminState {
    runtime_manager: RuntimeManager,
}

impl AdminState {
    pub fn new(runtime_manager: RuntimeManager) -> Self {
        Self { runtime_manager }
    }
}

pub fn router(state: AdminState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/config", get(config_page).post(config_submit))
        .route("/transforms", get(transforms_page).post(transforms_submit))
        .route("/results", get(results_page))
        .route("/results/{event_id}", get(result_detail_page))
        .with_state(state)
}

async fn index() -> Redirect {
    Redirect::to("/results")
}

async fn config_page(State(state): State<AdminState>) -> Response {
    let runtime_manager = state.runtime_manager.clone();
    let snapshot = runtime_manager.state().await;

    render_runtime_config(snapshot.saved_config.clone(), snapshot, None, Vec::new())
}

async fn transforms_page(State(state): State<AdminState>) -> Response {
    let runtime_manager = state.runtime_manager.clone();
    let snapshot = runtime_manager.state().await;

    render_transforms(snapshot.saved_config.clone(), snapshot, None, Vec::new())
}

fn config_submit(
    State(state): State<AdminState>,
    Form(values): Form<HashMap<String, String>>,
) -> impl std::future::Future<Output = Response> + Send {
    let runtime_manager = state.runtime_manager.clone();
    async move {
        let snapshot = runtime_manager.clone().state().await;
        let action = editor_action(&values);

        if matches!(action, EditorAction::Reload) {
            match runtime_manager.clone().reload().await {
                Ok(updated_snapshot) => {
                    return render_runtime_config(
                        updated_snapshot.saved_config.clone(),
                        updated_snapshot,
                        Some(Notice::info("Proxy reloaded from disk.".to_owned())),
                        Vec::new(),
                    );
                }
                Err(err) => {
                    let current_snapshot = runtime_manager.state().await;
                    return render_runtime_config(
                        current_snapshot.saved_config.clone(),
                        current_snapshot,
                        Some(Notice::error("Reload failed.".to_owned())),
                        vec![err.to_string()],
                    );
                }
            }
        }

        let mut draft = snapshot.saved_config.clone();
        draft.runtime = runtime_from_form(&values, &snapshot.saved_config.runtime);

        if matches!(action, EditorAction::Save) {
            match runtime_manager.save_config(draft.clone()).await {
                Ok(updated_snapshot) => {
                    let message = if updated_snapshot.pending_reload {
                        "Config saved. Reload is required to apply it.".to_owned()
                    } else {
                        "Config saved. The running proxy already matches this file.".to_owned()
                    };
                    return render_runtime_config(
                        draft,
                        updated_snapshot,
                        Some(Notice::info(message)),
                        Vec::new(),
                    );
                }
                Err(err) => {
                    return render_runtime_config(
                        draft,
                        snapshot,
                        Some(Notice::error("Config did not save.".to_owned())),
                        vec![err.to_string()],
                    );
                }
            }
        }

        render_runtime_config(draft, snapshot, None, Vec::new())
    }
}

fn transforms_submit(
    State(state): State<AdminState>,
    Form(values): Form<HashMap<String, String>>,
) -> impl std::future::Future<Output = Response> + Send {
    let runtime_manager = state.runtime_manager.clone();
    async move {
        let snapshot = runtime_manager.clone().state().await;
        let action = editor_action(&values);

        if matches!(action, EditorAction::Reload) {
            match runtime_manager.clone().reload().await {
                Ok(updated_snapshot) => {
                    return render_transforms(
                        updated_snapshot.saved_config.clone(),
                        updated_snapshot,
                        Some(Notice::info("Proxy reloaded from disk.".to_owned())),
                        Vec::new(),
                    );
                }
                Err(err) => {
                    let current_snapshot = runtime_manager.state().await;
                    return render_transforms(
                        current_snapshot.saved_config.clone(),
                        current_snapshot,
                        Some(Notice::error("Reload failed.".to_owned())),
                        vec![err.to_string()],
                    );
                }
            }
        }

        let mut draft = snapshot.saved_config.clone();
        draft.transforms = transforms_from_form(&values, &snapshot.saved_config.transforms);
        apply_editor_action(&mut draft.transforms, action);

        if matches!(action, EditorAction::Save) {
            match runtime_manager.save_config(draft.clone()).await {
                Ok(updated_snapshot) => {
                    let message = if updated_snapshot.pending_reload {
                        "Config saved. Reload is required to apply it.".to_owned()
                    } else {
                        "Config saved. The running proxy already matches this file.".to_owned()
                    };
                    return render_transforms(
                        draft,
                        updated_snapshot,
                        Some(Notice::info(message)),
                        Vec::new(),
                    );
                }
                Err(err) => {
                    return render_transforms(
                        draft,
                        snapshot,
                        Some(Notice::error("Config did not save.".to_owned())),
                        vec![err.to_string()],
                    );
                }
            }
        }

        render_transforms(draft, snapshot, None, Vec::new())
    }
}

async fn results_page(
    State(state): State<AdminState>,
    Query(query): Query<EventListFilterQuery>,
) -> Response {
    let runtime_manager = state.runtime_manager.clone();
    match runtime_manager.clone().active_database().await {
        Ok(database) => match list_events(&database, &query.to_filter()).await {
            Ok(page) => {
                let snapshot = runtime_manager.state().await;
                render_page(
                    "Results",
                    view! {
                        <ResultsPage snapshot=snapshot query=query items=page.items total=page.total />
                    },
                )
            }
            Err(err) => render_error_page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Results failed".to_owned(),
                err.to_string(),
            ),
        },
        Err(err) => render_error_page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Results failed".to_owned(),
            err.to_string(),
        ),
    }
}

async fn result_detail_page(
    State(state): State<AdminState>,
    Path(event_id): Path<String>,
) -> Response {
    let runtime_manager = state.runtime_manager.clone();
    match runtime_manager.clone().active_database().await {
        Ok(database) => match load_event_detail(&database, &event_id).await {
            Ok(Some(detail)) => {
                let snapshot = runtime_manager.state().await;
                render_page(
                    "Event Detail",
                    view! { <ResultDetailPage snapshot=snapshot detail=detail /> },
                )
            }
            Ok(None) => render_error_page(
                StatusCode::NOT_FOUND,
                "Event not found".to_owned(),
                "No matching event.".to_owned(),
            ),
            Err(err) => render_error_page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Detail failed".to_owned(),
                err.to_string(),
            ),
        },
        Err(err) => render_error_page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Detail failed".to_owned(),
            err.to_string(),
        ),
    }
}

fn render_runtime_config(
    draft: AppConfigFile,
    snapshot: RuntimeManagerState,
    notice: Option<Notice>,
    validation_errors: Vec<String>,
) -> Response {
    render_page(
        "Config",
        view! {
            <RuntimeConfigPage
                draft=draft
                snapshot=snapshot
                notice=notice
                validation_errors=validation_errors
            />
        },
    )
}

fn render_transforms(
    draft: AppConfigFile,
    snapshot: RuntimeManagerState,
    notice: Option<Notice>,
    validation_errors: Vec<String>,
) -> Response {
    render_page(
        "Transforms",
        view! {
            <TransformsPage
                draft=draft
                snapshot=snapshot
                notice=notice
                validation_errors=validation_errors
            />
        },
    )
}

fn render_page(title: &'static str, body: impl IntoView + 'static) -> Response {
    Html(
        view! {
            <Document title=title.to_owned()>
                {body.into_view()}
            </Document>
        }
        .to_html(),
    )
    .into_response()
}

fn render_error_page(status: StatusCode, title: String, detail: String) -> Response {
    (
        status,
        Html(
            view! {
                <Document title=title.to_owned()>
                    <main class="mx-auto max-w-3xl px-6 py-10">
                        <h1 class="text-3xl font-semibold text-slate-900">{title.to_owned()}</h1>
                        <p class="mt-4 rounded-2xl border border-rose-200 bg-rose-50 px-4 py-3 text-sm text-rose-900">
                            {detail.to_owned()}
                        </p>
                    </main>
                </Document>
            }
            .to_html(),
        ),
    )
        .into_response()
}

#[component]
fn Document(title: String, children: Children) -> impl IntoView {
    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
                <title>{format!("kid🐐proxy admin | {title}")}</title>
                <link rel="stylesheet" href="/static/admin.css"/>
            </head>
            <body class="min-h-screen bg-slate-100 text-slate-900">
                <div class="border-b border-slate-200 bg-white/90 backdrop-blur">
                    <div class="mx-auto flex max-w-7xl items-center justify-between px-6 py-4">
                        <div>
                            <a class="text-lg font-semibold tracking-tight text-slate-950" href="/results">
                                "kid🐐proxy admin"
                            </a>
                        </div>
                        <nav class="flex gap-3 text-sm font-medium text-slate-600">
                            <a class="rounded-full px-3 py-1.5 hover:bg-slate-100 hover:text-slate-950" href="/results">
                                "Results"
                            </a>
                            <a class="rounded-full px-3 py-1.5 hover:bg-slate-100 hover:text-slate-950" href="/config">
                                "Config"
                            </a>
                            <a class="rounded-full px-3 py-1.5 hover:bg-slate-100 hover:text-slate-950" href="/transforms">
                                "Transforms"
                            </a>
                        </nav>
                    </div>
                </div>
                {children()}
            </body>
        </html>
    }
}

#[component]
fn RuntimeConfigPage(
    draft: AppConfigFile,
    snapshot: RuntimeManagerState,
    notice: Option<Notice>,
    validation_errors: Vec<String>,
) -> impl IntoView {
    let runtime = draft.runtime.clone();
    view! {
        <main class="mx-auto grid max-w-7xl gap-6 px-6 py-8 lg:grid-cols-[20rem_minmax(0,1fr)]">
            <AdminSidebar snapshot=snapshot.clone() reload_path="/config"/>
            <section class="space-y-4">
                <EditorFeedback notice=notice validation_errors=validation_errors/>
                <form action="/config" method="post" class="space-y-6">
                    <section class="rounded-3xl border border-slate-200 bg-white p-6 shadow-sm">
                        <div class="flex items-center justify-between">
                            <h1 class="text-2xl font-semibold tracking-tight text-slate-950">"Proxy config"</h1>
                            <button
                                class="inline-flex rounded-full bg-emerald-600 px-4 py-2 text-sm font-medium text-white hover:bg-emerald-500"
                                name="editor_action"
                                type="submit"
                                value="save"
                            >
                                "Save config"
                            </button>
                        </div>
                        <div class="mt-6 grid gap-4 md:grid-cols-2">
                            <InputField label="Listen address" name="listen_addr".to_owned() value=runtime.listen_addr.clone() field_type="text"/>
                            <InputField label="Frontend domain" name="frontend_domain".to_owned() value=runtime.frontend_domain.clone() field_type="text"/>
                            <InputField label="Backend URL" name="backend_url".to_owned() value=runtime.backend_url.clone() field_type="text"/>
                            <InputField label="SQLite path" name="sqlite_path".to_owned() value=runtime.sqlite_path.to_string_lossy().to_string() field_type="text"/>
                            <InputField label="TLS cert path" name="tls_cert_path".to_owned() value=runtime.tls_cert_path.to_string_lossy().to_string() field_type="text"/>
                            <InputField label="TLS key path" name="tls_key_path".to_owned() value=runtime.tls_key_path.to_string_lossy().to_string() field_type="text"/>
                            <InputField label="CA bundle path" name="ca_bundle_path".to_owned() value=optional_path(&runtime.ca_bundle_path) field_type="text"/>
                            <InputField label="Upstream SNI override" name="upstream_sni_override".to_owned() value=runtime.upstream_sni_override.clone().unwrap_or_default() field_type="text"/>
                            <SelectField label="HTTP mode" name="http_mode".to_owned() value=http_mode_value(runtime.http_mode) options=http_mode_options()/>
                            <InputField label="Flush rows" name="flush_rows".to_owned() value=runtime.flush_rows.to_string() field_type="number"/>
                            <InputField label="Flush interval ms" name="flush_interval_ms".to_owned() value=runtime.flush_interval_ms.to_string() field_type="number"/>
                            <InputField label="Max inflight events" name="max_inflight_events".to_owned() value=runtime.max_inflight_events.to_string() field_type="number"/>
                            <InputField label="Body max bytes" name="body_max_bytes".to_owned() value=runtime.body_max_bytes.to_string() field_type="number"/>
                            <InputField label="Connect timeout ms" name="connect_timeout_ms".to_owned() value=runtime.connect_timeout_ms.to_string() field_type="number"/>
                            <InputField label="Request timeout ms" name="request_timeout_ms".to_owned() value=runtime.request_timeout_ms.to_string() field_type="number"/>
                            <InputField label="Idle pool timeout ms" name="idle_pool_timeout_ms".to_owned() value=runtime.idle_pool_timeout_ms.to_string() field_type="number"/>
                            <InputField label="Graceful shutdown timeout ms" name="graceful_shutdown_timeout_ms".to_owned() value=runtime.graceful_shutdown_timeout_ms.to_string() field_type="number"/>
                            <InputField label="Header allowlist" name="header_allowlist".to_owned() value=runtime.header_allowlist.join(",") field_type="text"/>
                            <InputField label="Header denylist" name="header_denylist".to_owned() value=runtime.header_denylist.join(",") field_type="text"/>
                        </div>
                        <div class="mt-4 grid gap-4 md:grid-cols-2">
                            <CheckboxField label="Trust proxy headers" name="trust_proxy_headers".to_owned() checked=runtime.trust_proxy_headers/>
                            <CheckboxField label="Emit TLS key log" name="emit_keylog".to_owned() checked=runtime.emit_keylog/>
                        </div>
                    </section>
                </form>
            </section>
        </main>
    }
}

#[component]
fn TransformsPage(
    draft: AppConfigFile,
    snapshot: RuntimeManagerState,
    notice: Option<Notice>,
    validation_errors: Vec<String>,
) -> impl IntoView {
    let transforms = draft.transforms.clone();
    view! {
        <main class="mx-auto grid max-w-7xl gap-6 px-6 py-8 lg:grid-cols-[20rem_minmax(0,1fr)]">
            <AdminSidebar snapshot=snapshot reload_path="/transforms"/>
            <section class="space-y-4">
                <EditorFeedback notice=notice validation_errors=validation_errors/>
                <form action="/transforms" method="post" class="space-y-6">
                    <section class="rounded-3xl border border-slate-200 bg-white p-6 shadow-sm">
                        <div class="flex items-center justify-between">
                            <h1 class="text-2xl font-semibold tracking-tight text-slate-950">"Transforms"</h1>
                            <div class="flex gap-3">
                                <button
                                    class="inline-flex rounded-full border border-slate-300 px-4 py-2 text-sm font-medium text-slate-700 hover:border-slate-950 hover:text-slate-950"
                                    name="editor_action"
                                    type="submit"
                                    value="add_transform"
                                >
                                    "Add transform"
                                </button>
                                <button
                                    class="inline-flex rounded-full bg-emerald-600 px-4 py-2 text-sm font-medium text-white hover:bg-emerald-500"
                                    name="editor_action"
                                    type="submit"
                                    value="save"
                                >
                                    "Save config"
                                </button>
                            </div>
                        </div>
                        <input name="transform_count" type="hidden" value=transforms.len().to_string()/>
                        <div class="mt-6 space-y-4">
                            {transforms
                                .iter()
                                .enumerate()
                                .map(|(index, rule)| {
                                    view! {
                                        <TransformEditor index=index rule=rule.clone() />
                                    }
                                })
                                .collect_view()}
                            {transforms.is_empty().then(|| view! {
                                <p class="rounded-2xl border border-dashed border-slate-300 px-4 py-6 text-sm text-slate-600">
                                    "No transforms configured yet."
                                </p>
                            })}
                        </div>
                    </section>
                </form>
            </section>
        </main>
    }
}

#[component]
fn AdminSidebar(snapshot: RuntimeManagerState, reload_path: &'static str) -> impl IntoView {
    view! {
        <aside class="space-y-4">
            <section class="rounded-3xl border border-slate-200 bg-white p-5 shadow-sm">
                <p class="text-xs font-semibold uppercase tracking-[0.2em] text-slate-500">"Active runtime"</p>
                <dl class="mt-4 space-y-3 text-sm">
                    <SummaryRow label="Listen" value=snapshot.active_runtime.listen_addr.clone()/>
                    <SummaryRow label="Frontend" value=snapshot.active_runtime.frontend_domain.clone()/>
                    <SummaryRow label="Backend" value=snapshot.active_runtime.backend_url.clone()/>
                    <SummaryRow label="SQLite" value=snapshot.active_runtime.sqlite_path.clone()/>
                    <SummaryRow label="Transforms" value=snapshot.active_runtime.transform_count.to_string()/>
                    <SummaryRow label="Dropped events" value=snapshot.active_runtime.dropped_events.to_string()/>
                    <SummaryRow label="Write failures" value=snapshot.active_runtime.write_failures.to_string()/>
                </dl>
            </section>
            <section class="rounded-3xl border border-slate-200 bg-white p-5 shadow-sm">
                <p class="text-xs font-semibold uppercase tracking-[0.2em] text-slate-500">"Reload status"</p>
                <p class="mt-3 text-sm text-slate-700">
                    {if snapshot.pending_reload {
                        "Saved config differs from the running proxy."
                    } else {
                        "Running proxy matches the saved config."
                    }}
                </p>
                <form action=reload_path method="post" class="mt-4">
                    <button
                        class="inline-flex rounded-full bg-slate-950 px-4 py-2 text-sm font-medium text-white hover:bg-slate-800"
                        name="editor_action"
                        type="submit"
                        value="reload"
                    >
                        "Reload proxy"
                    </button>
                </form>
                {snapshot.last_reload_error.as_ref().map(|error| view! {
                    <p class="mt-4 rounded-2xl border border-rose-200 bg-rose-50 px-4 py-3 text-sm text-rose-900">
                        {error.clone()}
                    </p>
                })}
            </section>
        </aside>
    }
}

#[component]
fn EditorFeedback(notice: Option<Notice>, validation_errors: Vec<String>) -> impl IntoView {
    let notice_view = notice.map(|item| {
        let tone = item.tone.to_owned();
        let message = item.message;
        view! {
            <div class=format!("rounded-3xl border p-5 text-sm {}", tone)>
                {message}
            </div>
        }
        .into_view()
    });
    view! {
        {notice_view}
        {(!validation_errors.is_empty()).then(|| view! {
            <div class="rounded-3xl border border-rose-200 bg-rose-50 p-5 text-sm text-rose-900">
                <p class="font-semibold">"Validation errors"</p>
                <ul class="mt-3 space-y-2">
                    {validation_errors
                        .iter()
                        .map(|error| view! { <li>{error.clone()}</li> })
                        .collect_view()}
                </ul>
            </div>
        })}
    }
}

#[component]
fn SummaryRow(label: &'static str, value: String) -> impl IntoView {
    view! {
        <div class="flex flex-col gap-1">
            <dt class="text-xs uppercase tracking-[0.14em] text-slate-500">{label}</dt>
            <dd class="font-medium text-slate-900 break-all">{value}</dd>
        </div>
    }
}

#[component]
fn InputField(
    label: &'static str,
    name: String,
    value: String,
    field_type: &'static str,
) -> impl IntoView {
    view! {
        <label class="block">
            <span class="mb-2 block text-sm font-medium text-slate-700">{label}</span>
            <input
                class="w-full rounded-2xl border border-slate-300 bg-slate-50 px-4 py-3 text-sm text-slate-900 outline-none ring-0 transition focus:border-slate-950 focus:bg-white"
                name=name
                type=field_type
                value=value
            />
        </label>
    }
}

#[component]
fn CheckboxField(label: &'static str, name: String, checked: bool) -> impl IntoView {
    view! {
        <label class="flex items-center gap-3 rounded-2xl border border-slate-200 bg-slate-50 px-4 py-3 text-sm font-medium text-slate-700">
            <input checked=checked class="size-4 rounded border-slate-300 text-slate-950" name=name type="checkbox" value="true"/>
            {label}
        </label>
    }
}

#[component]
fn SelectField(
    label: &'static str,
    name: String,
    value: String,
    options: Vec<(&'static str, &'static str)>,
) -> impl IntoView {
    view! {
        <label class="block">
            <span class="mb-2 block text-sm font-medium text-slate-700">{label}</span>
            <select
                class="w-full rounded-2xl border border-slate-300 bg-slate-50 px-4 py-3 text-sm text-slate-900 outline-none transition focus:border-slate-950 focus:bg-white"
                name=name
            >
                {options
                    .into_iter()
                    .map(|(option_value, option_label)| {
                        view! {
                            <option selected=option_value == value value=option_value>
                                {option_label}
                            </option>
                        }
                    })
                    .collect_view()}
            </select>
        </label>
    }
}

#[component]
fn TransformEditor(index: usize, rule: TransformRuleFile) -> impl IntoView {
    let matcher_type = matcher_type_value(&rule.matcher);
    let matcher_pattern = matcher_pattern_value(&rule.matcher);
    let target_type = target_type_value(&rule.target);
    let target_name = target_name_value(&rule.target);
    let (action_from, action_to) = action_values(&rule.action);
    view! {
        <article class="rounded-3xl border border-slate-200 bg-slate-50 p-5">
            <div class="flex items-center justify-between">
                <h3 class="text-base font-semibold text-slate-950">{format!("Rule {}", index + 1)}</h3>
                <div class="flex gap-2">
                    <button class="rounded-full border border-slate-300 px-3 py-1.5 text-xs font-medium text-slate-700 hover:border-slate-950 hover:text-slate-950" name="editor_action" type="submit" value="move_transform_up">
                        "Up"
                    </button>
                    <button class="rounded-full border border-slate-300 px-3 py-1.5 text-xs font-medium text-slate-700 hover:border-slate-950 hover:text-slate-950" name="editor_action" type="submit" value="move_transform_down">
                        "Down"
                    </button>
                    <button class="rounded-full border border-rose-300 px-3 py-1.5 text-xs font-medium text-rose-700 hover:border-rose-500 hover:text-rose-900" name="editor_action" type="submit" value="remove_transform">
                        "Remove"
                    </button>
                    <input name="editor_index" type="hidden" value=index.to_string()/>
                </div>
            </div>
            <input name=format!("transform_{index}_present") type="hidden" value="true"/>
            <div class="mt-4 grid gap-4 md:grid-cols-2">
                <SelectField label="Matcher" name=owned_name(index, "matcher_type") value=matcher_type options=matcher_options()/>
                <InputField label="Matcher pattern" name=owned_name(index, "matcher_pattern") value=matcher_pattern field_type="text"/>
                <InputField label="Replace from" name=owned_name(index, "action_from") value=action_from field_type="text"/>
                <InputField label="Replace to" name=owned_name(index, "action_to") value=action_to field_type="text"/>
                <SelectField label="Target" name=owned_name(index, "target_type") value=target_type options=target_options()/>
                <InputField label="Target header name" name=owned_name(index, "target_name") value=target_name field_type="text"/>
            </div>
            <div class="mt-4">
                <label class="inline-flex items-center gap-3 rounded-2xl border border-slate-200 bg-white px-4 py-3 text-sm font-medium text-slate-700">
                    <input checked=rule.stop class="size-4 rounded border-slate-300 text-slate-950" name=owned_name(index, "stop") type="checkbox" value="true"/>
                    "Stop after this rule triggers"
                </label>
            </div>
        </article>
    }
}

#[component]
fn ResultsPage(
    snapshot: RuntimeManagerState,
    query: EventListFilterQuery,
    items: Vec<EventListItem>,
    total: u64,
) -> impl IntoView {
    view! {
        <main class="mx-auto max-w-7xl px-6 py-8">
            <div class="mb-6 flex items-end justify-between gap-4">
                <div>
                    <h1 class="text-2xl font-semibold tracking-tight text-slate-950">"Captured results"</h1>
                    <p class="mt-2 text-sm text-slate-600">
                        {format!("Active SQLite database: {}", snapshot.active_runtime.sqlite_path)}
                    </p>
                </div>
            </div>
            <form class="grid gap-4 rounded-3xl border border-slate-200 bg-white p-6 shadow-sm md:grid-cols-3 xl:grid-cols-6" method="get">
                <InputField label="Method" name="method".to_owned() value=query.method.clone().unwrap_or_default() field_type="text"/>
                <InputField label="Path contains" name="path_substring".to_owned() value=query.path_substring.clone().unwrap_or_default() field_type="text"/>
                <InputField label="Status code" name="status_code".to_owned() value=query.status_code.map(|v| v.to_string()).unwrap_or_default() field_type="number"/>
                <SelectField label="Proxy result" name="proxy_result".to_owned() value=query.proxy_result.clone().unwrap_or_default() options=proxy_result_options()/>
                <InputField label="Last n hours" name="since_hours".to_owned() value=query.since_hours.map(|v| v.to_string()).unwrap_or_default() field_type="number"/>
                <input name="page" type="hidden" value="0"/>
                <div class="md:col-span-3 xl:col-span-6">
                    <button class="inline-flex rounded-full bg-slate-950 px-4 py-2 text-sm font-medium text-white hover:bg-slate-800" type="submit">
                        "Filter"
                    </button>
                </div>
            </form>

            <section class="mt-6 overflow-hidden rounded-3xl border border-slate-200 bg-white shadow-sm">
                <div class="flex items-center justify-between border-b border-slate-200 px-6 py-4">
                    <h2 class="text-sm font-semibold uppercase tracking-[0.2em] text-slate-500">"Recent events"</h2>
                    <p class="text-sm text-slate-600">{format!("{total} matching rows")}</p>
                </div>
                <div class="overflow-x-auto">
                    <table class="min-w-full divide-y divide-slate-200 text-sm">
                        <thead class="bg-slate-50 text-left text-xs uppercase tracking-[0.16em] text-slate-500">
                            <tr>
                                <th class="px-6 py-3">"Started"</th>
                                <th class="px-6 py-3">"Method"</th>
                                <th class="px-6 py-3">"Path"</th>
                                <th class="px-6 py-3">"Status"</th>
                                <th class="px-6 py-3">"Duration"</th>
                                <th class="px-6 py-3">"Backend"</th>
                                <th class="px-6 py-3">"Result"</th>
                            </tr>
                        </thead>
                        <tbody class="divide-y divide-slate-100">
                            {items
                                .into_iter()
                                .map(|item| {
                                    view! {
                                        <tr class="hover:bg-slate-50">
                                            <td class="px-6 py-4 text-slate-600">{item.request_start_ts}</td>
                                            <td class="px-6 py-4 font-medium text-slate-950">{item.method}</td>
                                            <td class="px-6 py-4">
                                                <a class="font-mono text-xs text-sky-700 hover:text-sky-900" href=format!("/results/{}", item.event_id)>
                                                    {item.path}
                                                </a>
                                            </td>
                                            <td class="px-6 py-4">{item.status_code}</td>
                                            <td class="px-6 py-4">{format!("{} ms", item.duration_ms)}</td>
                                            <td class="px-6 py-4 text-slate-600">{item.backend_host}</td>
                                            <td class="px-6 py-4">{item.proxy_result}</td>
                                        </tr>
                                    }
                                })
                                .collect_view()}
                        </tbody>
                    </table>
                </div>
            </section>
        </main>
    }
}

#[component]
fn ResultDetailPage(snapshot: RuntimeManagerState, detail: EventDetail) -> impl IntoView {
    view! {
        <main class="mx-auto max-w-7xl space-y-6 px-6 py-8">
            <div class="flex items-end justify-between gap-4">
                <div>
                    <p class="text-xs font-semibold uppercase tracking-[0.2em] text-slate-500">"Active SQLite"</p>
                    <p class="mt-2 text-sm text-slate-600">{snapshot.active_runtime.sqlite_path}</p>
                    <h1 class="mt-4 text-2xl font-semibold tracking-tight text-slate-950">
                        {format!("{} {}", detail.method, detail.path)}
                    </h1>
                </div>
                <a class="rounded-full border border-slate-300 px-4 py-2 text-sm font-medium text-slate-700 hover:border-slate-950 hover:text-slate-950" href="/results">
                    "Back to results"
                </a>
            </div>
            <section class="grid gap-4 md:grid-cols-3">
                <MetricCard label="Started" value=detail.request_start_ts.clone()/>
                <MetricCard label="Status" value=detail.status_code.clone()/>
                <MetricCard label="Proxy result" value=detail.proxy_result.clone()/>
                <MetricCard label="Duration" value=format!("{} ms", detail.duration_ms) />
                <MetricCard label="Backend" value=detail.backend_host.clone()/>
                <MetricCard label="Frontend SNI" value=detail.frontend_tls_sni.clone().unwrap_or_else(|| "n/a".to_owned())/>
            </section>
            <section class="grid gap-6 lg:grid-cols-2">
                <TextBlock title="Request headers" body=detail.request_headers_json/>
                <TextBlock title="Response headers" body=detail.response_headers_json.unwrap_or_else(|| "{}".to_owned())/>
                <TextBlock title="Request body preview" body=detail.request_body_preview/>
                <TextBlock title="Response body preview" body=detail.response_body_preview/>
                <TextBlock title="Request cookies" body=detail.request_cookies_json.unwrap_or_else(|| "{}".to_owned())/>
                <TextBlock title="Response set-cookie" body=detail.response_set_cookie_json.unwrap_or_else(|| "{}".to_owned())/>
            </section>
        </main>
    }
}

#[component]
fn MetricCard(label: &'static str, value: String) -> impl IntoView {
    view! {
        <div class="rounded-3xl border border-slate-200 bg-white p-5 shadow-sm">
            <p class="text-xs font-semibold uppercase tracking-[0.2em] text-slate-500">{label}</p>
            <p class="mt-3 text-sm font-medium text-slate-900 break-all">{value}</p>
        </div>
    }
}

#[component]
fn TextBlock(title: &'static str, body: String) -> impl IntoView {
    view! {
        <section class="rounded-3xl border border-slate-200 bg-white p-5 shadow-sm">
            <h2 class="text-sm font-semibold uppercase tracking-[0.2em] text-slate-500">{title}</h2>
            <pre class="mt-4 overflow-x-auto whitespace-pre-wrap rounded-2xl bg-slate-950/95 p-4 text-xs text-slate-100">
                {body}
            </pre>
        </section>
    }
}

#[derive(Debug, Clone)]
struct Notice {
    tone: &'static str,
    message: String,
}

impl Notice {
    fn info(message: String) -> Self {
        Self {
            tone: "border-emerald-200 bg-emerald-50 text-emerald-900",
            message,
        }
    }

    fn error(message: String) -> Self {
        Self {
            tone: "border-rose-200 bg-rose-50 text-rose-900",
            message,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum EditorAction {
    AddTransform,
    MoveTransformDown(usize),
    MoveTransformUp(usize),
    RemoveTransform(usize),
    Reload,
    Save,
}

fn editor_action(values: &HashMap<String, String>) -> EditorAction {
    let index = values
        .get("editor_index")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or_default();
    match values.get("editor_action").map(String::as_str) {
        Some("add_transform") => EditorAction::AddTransform,
        Some("move_transform_up") => EditorAction::MoveTransformUp(index),
        Some("move_transform_down") => EditorAction::MoveTransformDown(index),
        Some("remove_transform") => EditorAction::RemoveTransform(index),
        Some("reload") => EditorAction::Reload,
        _ => EditorAction::Save,
    }
}

fn apply_editor_action(draft: &mut Vec<TransformRuleFile>, action: EditorAction) {
    match action {
        EditorAction::AddTransform => draft.push(blank_transform()),
        EditorAction::MoveTransformUp(index) => {
            if index > 0 && index < draft.len() {
                draft.swap(index, index - 1);
            }
        }
        EditorAction::MoveTransformDown(index) => {
            if index + 1 < draft.len() {
                draft.swap(index, index + 1);
            }
        }
        EditorAction::RemoveTransform(index) => {
            if index < draft.len() {
                draft.remove(index);
            }
        }
        EditorAction::Reload => {}
        EditorAction::Save => {}
    }
}

fn runtime_from_form(
    values: &HashMap<String, String>,
    fallback: &RuntimeSettingsFile,
) -> RuntimeSettingsFile {
    RuntimeSettingsFile {
        listen_addr: text_value(values, "listen_addr", &fallback.listen_addr),
        frontend_domain: text_value(values, "frontend_domain", &fallback.frontend_domain),
        backend_url: text_value(values, "backend_url", &fallback.backend_url),
        tls_cert_path: path_value(values, "tls_cert_path", &fallback.tls_cert_path),
        tls_key_path: path_value(values, "tls_key_path", &fallback.tls_key_path),
        sqlite_path: path_value(values, "sqlite_path", &fallback.sqlite_path),
        ca_bundle_path: optional_path_value(values, "ca_bundle_path", &fallback.ca_bundle_path),
        upstream_sni_override: optional_text_value(
            values,
            "upstream_sni_override",
            &fallback.upstream_sni_override,
        ),
        http_mode: http_mode_from_form(values.get("http_mode"), fallback.http_mode),
        flush_rows: usize_value(values, "flush_rows", fallback.flush_rows),
        flush_interval_ms: u64_value(values, "flush_interval_ms", fallback.flush_interval_ms),
        max_inflight_events: usize_value(
            values,
            "max_inflight_events",
            fallback.max_inflight_events,
        ),
        body_max_bytes: usize_value(values, "body_max_bytes", fallback.body_max_bytes),
        connect_timeout_ms: u64_value(values, "connect_timeout_ms", fallback.connect_timeout_ms),
        request_timeout_ms: u64_value(values, "request_timeout_ms", fallback.request_timeout_ms),
        idle_pool_timeout_ms: u64_value(
            values,
            "idle_pool_timeout_ms",
            fallback.idle_pool_timeout_ms,
        ),
        graceful_shutdown_timeout_ms: u64_value(
            values,
            "graceful_shutdown_timeout_ms",
            fallback.graceful_shutdown_timeout_ms,
        ),
        trust_proxy_headers: values.contains_key("trust_proxy_headers"),
        emit_keylog: values.contains_key("emit_keylog"),
        header_allowlist: comma_list(values.get("header_allowlist"), &fallback.header_allowlist),
        header_denylist: comma_list(values.get("header_denylist"), &fallback.header_denylist),
    }
}

fn transforms_from_form(
    values: &HashMap<String, String>,
    fallback: &[TransformRuleFile],
) -> Vec<TransformRuleFile> {
    let transform_count = usize_value(values, "transform_count", fallback.len());
    let mut transforms = Vec::with_capacity(transform_count);
    for index in 0..transform_count {
        transforms.push(TransformRuleFile {
            matcher: matcher_from_form(values, index),
            action: TransformActionFile::Replace {
                from: text_value_owned(values, &owned_name(index, "action_from")),
                to: text_value_owned(values, &owned_name(index, "action_to")),
            },
            target: target_from_form(values, index),
            stop: values.contains_key(&owned_name(index, "stop")),
        });
    }
    transforms
}

#[derive(Debug, Clone, Deserialize)]
struct EventListFilterQuery {
    method: Option<String>,
    path_substring: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_number")]
    status_code: Option<i32>,
    proxy_result: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_number")]
    since_hours: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_number")]
    page: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_number")]
    per_page: Option<u64>,
}

impl EventListFilterQuery {
    fn to_filter(&self) -> EventListFilter {
        EventListFilter {
            method: self.method.clone().filter(|value| !value.is_empty()),
            path_substring: self
                .path_substring
                .clone()
                .filter(|value| !value.is_empty()),
            status_code: self.status_code,
            proxy_result: self.proxy_result.clone().filter(|value| !value.is_empty()),
            since_hours: self.since_hours,
            page: self.page.unwrap_or(0),
            per_page: self.per_page.unwrap_or(50).clamp(1, 200),
        }
    }
}

#[derive(Debug, Clone)]
struct EventListFilter {
    method: Option<String>,
    path_substring: Option<String>,
    status_code: Option<i32>,
    proxy_result: Option<String>,
    since_hours: Option<i64>,
    page: u64,
    per_page: u64,
}

#[derive(Debug, Clone)]
struct EventListPage {
    items: Vec<EventListItem>,
    total: u64,
}

#[derive(Debug, Clone)]
struct EventListItem {
    event_id: String,
    request_start_ts: String,
    method: String,
    path: String,
    status_code: String,
    duration_ms: i64,
    backend_host: String,
    proxy_result: String,
}

#[derive(Debug, Clone)]
struct EventDetail {
    request_start_ts: String,
    method: String,
    path: String,
    status_code: String,
    duration_ms: i64,
    backend_host: String,
    proxy_result: String,
    frontend_tls_sni: Option<String>,
    request_headers_json: String,
    response_headers_json: Option<String>,
    request_cookies_json: Option<String>,
    response_set_cookie_json: Option<String>,
    request_body_preview: String,
    response_body_preview: String,
}

async fn list_events(
    database: &DatabaseConnection,
    filter: &EventListFilter,
) -> anyhow::Result<EventListPage> {
    let mut query = entity::Entity::find().order_by_desc(Column::RequestStartTs);

    if let Some(method) = &filter.method {
        query = query.filter(Column::Method.eq(method.to_ascii_uppercase()));
    }
    if let Some(path_substring) = &filter.path_substring {
        query = query.filter(Column::Path.contains(path_substring));
    }
    if let Some(status_code) = filter.status_code {
        query = query.filter(Column::StatusCode.eq(status_code));
    }
    if let Some(proxy_result) = &filter.proxy_result {
        query = query.filter(Column::ProxyResult.eq(proxy_result.clone()));
    }
    if let Some(since_hours) = filter.since_hours {
        let since = time::OffsetDateTime::now_utc() - time::Duration::hours(since_hours);
        let since_text = since
            .format(&time::format_description::well_known::Rfc3339)
            .context("format results since timestamp")?;
        query = query.filter(Column::RequestStartTs.gte(since_text));
    }

    let paginator = query.paginate(database, filter.per_page);
    let total = paginator.num_items().await.context("count result rows")?;
    let rows = paginator
        .fetch_page(filter.page)
        .await
        .context("fetch result page")?;

    Ok(EventListPage {
        items: rows
            .into_iter()
            .map(|row| EventListItem {
                event_id: row.event_id,
                request_start_ts: row.request_start_ts,
                method: row.method,
                path: row.path,
                status_code: row
                    .status_code
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "n/a".to_owned()),
                duration_ms: row.duration_ms,
                backend_host: row.backend_host,
                proxy_result: row.proxy_result,
            })
            .collect(),
        total,
    })
}

async fn load_event_detail(
    database: &DatabaseConnection,
    event_id: &str,
) -> anyhow::Result<Option<EventDetail>> {
    let row = entity::Entity::find_by_id(event_id.to_owned())
        .one(database)
        .await
        .context("load event detail")?;

    Ok(row.map(|row| EventDetail {
        request_start_ts: row.request_start_ts,
        method: row.method,
        path: row.path,
        status_code: row
            .status_code
            .map(|value| value.to_string())
            .unwrap_or_else(|| "n/a".to_owned()),
        duration_ms: row.duration_ms,
        backend_host: row.backend_host,
        proxy_result: row.proxy_result,
        frontend_tls_sni: row.frontend_tls_sni,
        request_headers_json: prettify_json(&row.request_headers_json),
        response_headers_json: row.response_headers_json.map(|value| prettify_json(&value)),
        request_cookies_json: row.request_cookies_json.map(|value| prettify_json(&value)),
        response_set_cookie_json: row
            .response_set_cookie_json
            .map(|value| prettify_json(&value)),
        request_body_preview: decode_preview(row.request_body_preview_base64),
        response_body_preview: decode_preview(row.response_body_preview_base64),
    }))
}

fn decode_preview(value: Option<String>) -> String {
    let Some(encoded) = value else {
        return "n/a".to_owned();
    };

    match base64::engine::general_purpose::STANDARD.decode(encoded.as_bytes()) {
        Ok(bytes) => match String::from_utf8(bytes.clone()) {
            Ok(text) => text,
            Err(_) => String::from_utf8_lossy(&bytes).to_string(),
        },
        Err(_) => encoded,
    }
}

fn deserialize_optional_number<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    match raw.as_deref().map(str::trim) {
        None | Some("") => Ok(None),
        Some(value) => value
            .parse::<T>()
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

fn prettify_json(value: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(value) {
        Ok(parsed) => serde_json::to_string_pretty(&parsed).unwrap_or_else(|_| value.to_owned()),
        Err(_) => value.to_owned(),
    }
}

fn blank_transform() -> TransformRuleFile {
    TransformRuleFile {
        matcher: TransformMatcherFile::Everything {},
        action: TransformActionFile::Replace {
            from: String::new(),
            to: String::new(),
        },
        target: TransformTargetFile::Body {},
        stop: false,
    }
}

fn matcher_from_form(values: &HashMap<String, String>, index: usize) -> TransformMatcherFile {
    match values
        .get(&owned_name(index, "matcher_type"))
        .map(String::as_str)
        .unwrap_or("any")
    {
        "url_glob" => TransformMatcherFile::UrlGlob {
            pattern: text_value_owned(values, &owned_name(index, "matcher_pattern")),
        },
        "content_type_glob" => TransformMatcherFile::ContentTypeGlob {
            pattern: text_value_owned(values, &owned_name(index, "matcher_pattern")),
        },
        _ => TransformMatcherFile::Everything {},
    }
}

fn target_from_form(values: &HashMap<String, String>, index: usize) -> TransformTargetFile {
    match values
        .get(&owned_name(index, "target_type"))
        .map(String::as_str)
        .unwrap_or("body")
    {
        "any" => TransformTargetFile::Everything {},
        "all_headers" => TransformTargetFile::AllHeaders {},
        "header" => TransformTargetFile::Header {
            name: text_value_owned(values, &owned_name(index, "target_name")),
        },
        "cookies" => TransformTargetFile::Cookies {},
        _ => TransformTargetFile::Body {},
    }
}

fn matcher_type_value(value: &TransformMatcherFile) -> String {
    match value {
        TransformMatcherFile::UrlGlob { .. } => "url_glob",
        TransformMatcherFile::ContentTypeGlob { .. } => "content_type_glob",
        TransformMatcherFile::Everything {} => "any",
    }
    .to_owned()
}

fn matcher_pattern_value(value: &TransformMatcherFile) -> String {
    match value {
        TransformMatcherFile::UrlGlob { pattern }
        | TransformMatcherFile::ContentTypeGlob { pattern } => pattern.clone(),
        TransformMatcherFile::Everything {} => String::new(),
    }
}

fn action_values(value: &TransformActionFile) -> (String, String) {
    match value {
        TransformActionFile::Replace { from, to } => (from.clone(), to.clone()),
    }
}

fn target_type_value(value: &TransformTargetFile) -> String {
    match value {
        TransformTargetFile::Everything {} => "any",
        TransformTargetFile::Body {} => "body",
        TransformTargetFile::AllHeaders {} => "all_headers",
        TransformTargetFile::Header { .. } => "header",
        TransformTargetFile::Cookies {} => "cookies",
    }
    .to_owned()
}

fn target_name_value(value: &TransformTargetFile) -> String {
    match value {
        TransformTargetFile::Header { name } => name.clone(),
        _ => String::new(),
    }
}

fn http_mode_options() -> Vec<(&'static str, &'static str)> {
    vec![("auto", "Auto"), ("http1", "HTTP/1"), ("http2", "HTTP/2")]
}

fn matcher_options() -> Vec<(&'static str, &'static str)> {
    vec![
        ("any", "Any response"),
        ("url_glob", "URL glob"),
        ("content_type_glob", "Content-Type glob"),
    ]
}

fn target_options() -> Vec<(&'static str, &'static str)> {
    vec![
        ("body", "Body"),
        ("any", "Body + headers"),
        ("all_headers", "All headers"),
        ("header", "One header"),
        ("cookies", "Set-Cookie"),
    ]
}

fn proxy_result_options() -> Vec<(&'static str, &'static str)> {
    vec![
        ("", "Any result"),
        (ProxyResult::Inflight.as_str(), "Inflight"),
        (ProxyResult::Success.as_str(), "Success"),
        (ProxyResult::ClientRejected.as_str(), "Client rejected"),
        (ProxyResult::UpstreamError.as_str(), "Upstream error"),
        (ProxyResult::StreamError.as_str(), "Stream error"),
    ]
}

fn http_mode_value(mode: crate::cli::HttpMode) -> String {
    match mode {
        crate::cli::HttpMode::Auto => "auto",
        crate::cli::HttpMode::Http1 => "http1",
        crate::cli::HttpMode::Http2 => "http2",
    }
    .to_owned()
}

fn http_mode_from_form(
    value: Option<&String>,
    fallback: crate::cli::HttpMode,
) -> crate::cli::HttpMode {
    match value.map(String::as_str) {
        Some("http1") => crate::cli::HttpMode::Http1,
        Some("http2") => crate::cli::HttpMode::Http2,
        Some("auto") => crate::cli::HttpMode::Auto,
        _ => fallback,
    }
}

fn text_value(values: &HashMap<String, String>, key: &str, fallback: &str) -> String {
    values
        .get(key)
        .cloned()
        .unwrap_or_else(|| fallback.to_owned())
}

fn text_value_owned(values: &HashMap<String, String>, key: &str) -> String {
    values.get(key).cloned().unwrap_or_default()
}

fn optional_text_value(
    values: &HashMap<String, String>,
    key: &str,
    fallback: &Option<String>,
) -> Option<String> {
    match values.get(key) {
        Some(value) if value.trim().is_empty() => None,
        Some(value) => Some(value.clone()),
        None => fallback.clone(),
    }
}

fn path_value(
    values: &HashMap<String, String>,
    key: &str,
    fallback: &std::path::Path,
) -> std::path::PathBuf {
    match values.get(key) {
        Some(value) if !value.trim().is_empty() => value.into(),
        _ => fallback.to_path_buf(),
    }
}

fn optional_path_value(
    values: &HashMap<String, String>,
    key: &str,
    fallback: &Option<std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    match values.get(key) {
        Some(value) if value.trim().is_empty() => None,
        Some(value) => Some(value.into()),
        None => fallback.clone(),
    }
}

fn optional_path(value: &Option<std::path::PathBuf>) -> String {
    value
        .as_ref()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn usize_value(values: &HashMap<String, String>, key: &str, fallback: usize) -> usize {
    values
        .get(key)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(fallback)
}

fn u64_value(values: &HashMap<String, String>, key: &str, fallback: u64) -> u64 {
    values
        .get(key)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(fallback)
}

fn comma_list(value: Option<&String>, fallback: &[String]) -> Vec<String> {
    match value {
        Some(raw) => raw
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
        None => fallback.to_vec(),
    }
}

fn owned_name(index: usize, suffix: &str) -> String {
    format!("transform_{index}_{suffix}")
}
