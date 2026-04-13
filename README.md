# kid🐐proxy

`kidproxy` is a single-backend HTTPS reverse proxy built with Rama and Rustls. It terminates frontend TLS, forwards traffic to one configured `https://` backend, records exchanges into SQLite, applies ordered response transforms, and now exposes a local Leptos admin UI for editing config and browsing captured results.

## What It Does

- Accepts HTTPS on one listen address with one frontend certificate.
- Proxies to one fixed HTTPS backend with Rustls certificate verification.
- Preserves method, path, query, status, headers, and streaming bodies by default.
- Captures request, response, timing, and TLS metadata.
- Writes completed exchanges into `proxy_events` in SQLite using in-process SeaORM migrations.
- Supports ordered response transforms for headers, cookies, and body replacement.
- Serves a localhost-only admin UI for:
  - editing runtime settings and transforms
  - saving config changes
  - explicitly reloading the running proxy
  - browsing recent SQLite rows and per-event details

## Bootstrap CLI

The CLI is now only for startup/bootstrap:

- `--config-path`
- `--admin-listen-addr`

Environment equivalents:

- `PROXY_CONFIG_PATH`
- `PROXY_ADMIN_LISTEN_ADDR`

Example:

```bash
cargo run -- \
  --config-path ./kidproxy.json \
  --admin-listen-addr 127.0.0.1:3000
```

The proxy runtime itself is loaded from `kidproxy.json`.

## Config File

`kidproxy.json` contains both runtime settings and transforms:

```json
{
  "runtime": {
    "listen_addr": "0.0.0.0:8443",
    "frontend_domain": "example.test",
    "backend_url": "https://backend.example",
    "tls_cert_path": "./certs/fullchain.pem",
    "tls_key_path": "./certs/privkey.pem",
    "sqlite_path": "./data/kidproxy.sqlite",
    "http_mode": "auto",
    "flush_rows": 5000,
    "flush_interval_ms": 2000,
    "max_inflight_events": 10000,
    "body_max_bytes": 65536,
    "connect_timeout_ms": 5000,
    "request_timeout_ms": 180000,
    "idle_pool_timeout_ms": 90000,
    "graceful_shutdown_timeout_ms": 10000,
    "trust_proxy_headers": false,
    "emit_keylog": false,
    "header_allowlist": [],
    "header_denylist": []
  },
  "transforms": [
    {
      "matcher": { "type": "url_glob", "pattern": "/hello*" },
      "action": { "type": "replace", "from": "backend", "to": "proxy" },
      "target": { "type": "body" },
      "stop": true
    }
  ]
}
```

Supported matcher types:

- `url_glob`
- `content_type_glob`
- `any`

Supported target types:

- `any`
- `body`
- `all_headers`
- `header`
- `cookies`

Only response-side transforms are supported. Header and cookie transforms preserve streaming. Body and `any` transforms buffer the matched response before returning it to the client.

## Admin UI

Start the service, then open the admin UI at `http://127.0.0.1:3000` by default.

Pages:

- `/config`: edit runtime settings and ordered transforms, save, and reload
- `/results`: filter recent rows from SQLite
- `/results/:event_id`: inspect one captured exchange

The admin server is separate from proxy reloads, so reloading the proxy does not stop the UI.

## Tailwind CSS

The admin UI stylesheet is generated from `admin/tailwind.css`.

Build it with:

```bash
pnpm run build:css
```

## Development

Main modules:

- `src/main.rs`: bootstrap CLI, runtime manager startup, admin server
- `src/config.rs`: file-backed app config and runtime validation
- `src/runtime_manager.rs`: save/reload lifecycle for proxy and writer
- `src/admin.rs`: Leptos admin pages and SQLite browsing
- `src/proxy.rs`: Rama listener and reverse proxy path
- `src/capture.rs`: exchange capture and body observers
- `src/entity.rs`: SeaORM entity definitions
- `src/migration.rs`: programmatic database migrations
- `src/transform.rs`: transform parsing, matching, and rewrite helpers
- `src/writer.rs`: bounded writer task and SQLite flush logic
- `src/probe.rs`: backend compatibility probe

Integration coverage lives in:

- `tests/basic_proxy.rs`
- `tests/streaming.rs`
- `tests/sqlite_output.rs`
- `tests/transforms.rs`

## Notes

- Backend URLs must use `https://`.
- Header allowlists and denylists affect logged header JSON only, not forwarded traffic.
- Request and response bodies are hashed and retained up to `body_max_bytes`, with truncation recorded when the body exceeds that cap.
- JA3 and JA4 are only recorded when Rama exposes enough metadata on the active Rustls path; otherwise those fields are null.
