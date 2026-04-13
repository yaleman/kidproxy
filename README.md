# kidproxy

`kidproxy` is a single-backend HTTPS reverse proxy built with Rama and Rustls. It terminates frontend TLS, forwards traffic to one configured `https://` backend, preserves request and response flow as closely as practical by default, can apply ordered response transforms from a JSON config file, and writes one SQLite row per completed exchange.

## What v1 does

- Accepts HTTPS on one listen address with one frontend certificate.
- Proxies to one fixed HTTPS backend with Rustls certificate verification.
- Preserves method, path, query, status, headers, and streaming bodies by default.
- Captures request, response, connection, timing, and TLS metadata.
- Writes completed exchanges into one SQLite database with SeaORM-managed migrations.
- Drops log events under pressure instead of blocking proxy traffic.
- Supports ordered response transforms from a JSON config file for header, cookie, and body replacement.

## What v1 does not do

- Dynamic routing or multiple backends.
- Impossible certificate mirroring between different frontend and backend hostnames.
- Database sharding, partitioning, or segmented output files.

## CLI

All operational flags have `PROXY_*` environment variable equivalents via `clap` derive and `env`.

Required flags:

- `--listen-addr`
- `--frontend-domain`
- `--backend-url`
- `--tls-cert-path`
- `--tls-key-path`
- `--sqlite-path`

Useful optional flags:

- `--config-path`
- `--ca-bundle-path`
- `--upstream-sni-override`
- `--http-mode auto|http1|http2`
- `--flush-rows`
- `--flush-interval-ms`
- `--body-max-bytes`
- `--trust-proxy-headers`
- `--emit-keylog`

## Run

```bash
cargo run -- \
  --listen-addr 0.0.0.0:8443 \
  --frontend-domain example1.com \
  --backend-url https://example2.com \
  --tls-cert-path ./certs/fullchain.pem \
  --tls-key-path ./certs/privkey.pem \
  --sqlite-path ./data/kidproxy.sqlite \
  --config-path ./config/transforms.json
```

Environment example:

```bash
export PROXY_LISTEN_ADDR="0.0.0.0:8443"
export PROXY_FRONTEND_DOMAIN="example1.com"
export PROXY_BACKEND_URL="https://example2.com"
export PROXY_TLS_CERT_PATH="./certs/fullchain.pem"
export PROXY_TLS_KEY_PATH="./certs/privkey.pem"
export PROXY_SQLITE_PATH="./data/kidproxy.sqlite"
export PROXY_CONFIG_PATH="./config/transforms.json"
cargo run --
```

## SQLite output

The writer stores one row per completed exchange in the `proxy_events` table. Schema setup is handled by in-process SeaORM migrations only; the SeaORM CLI is not used.

## Transform config

When `--config-path` is set, `kidproxy` loads a JSON file containing an ordered `transforms` array. Each transform has a typed matcher, action, target, and optional `stop` flag.

```json
{
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

Only response-side transforms are supported in v1. Header and cookie transforms preserve streaming. Body and `any` transforms buffer the matched response before returning it to the client.

## Development

Main modules:

- `src/main.rs`: startup, tracing, probe, shutdown orchestration
- `src/config.rs`: CLI to validated runtime config
- `src/tls.rs`: Rustls frontend and upstream configuration
- `src/proxy.rs`: Rama listener and reverse proxy path
- `src/capture.rs`: exchange capture and body observers
- `src/entity.rs`: SeaORM entity definitions
- `src/migration.rs`: programmatic database migrations
- `src/transform.rs`: transform config parsing, matching, and rewrite helpers
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
- Request and response bodies are always hashed and retained up to `body_max_bytes`, with truncation recorded when the body exceeds that cap.
- Response transforms are loaded only from the JSON config file; core runtime settings remain CLI/env-driven.
- JA3 and JA4 are only recorded when Rama exposes enough metadata on the active Rustls path; otherwise those fields are null.
- The proxy aims to be protocol-boring, not magical. It will not spoof backend certificate identity for a different frontend hostname.
