# kidproxy

`kidproxy` is a single-backend HTTPS reverse proxy built with Rama and Rustls. It terminates frontend TLS, forwards traffic to one configured `https://` backend, preserves request and response flow as closely as practical, and writes one SQLite row per completed exchange.

## What v1 does

- Accepts HTTPS on one listen address with one frontend certificate.
- Proxies to one fixed HTTPS backend with Rustls certificate verification.
- Preserves method, path, query, status, headers, and streaming bodies without intentional payload rewriting.
- Captures request, response, connection, timing, and TLS metadata.
- Writes completed exchanges into one SQLite database with SeaORM-managed migrations.
- Drops log events under pressure instead of blocking proxy traffic.

## What v1 does not do

- Dynamic routing or multiple backends.
- Config files.
- Request or response body rewriting.
- Header rewriting beyond hop-by-hop filtering required for correct proxying.
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
  --sqlite-path ./data/kidproxy.sqlite
```

Environment example:

```bash
export PROXY_LISTEN_ADDR="0.0.0.0:8443"
export PROXY_FRONTEND_DOMAIN="example1.com"
export PROXY_BACKEND_URL="https://example2.com"
export PROXY_TLS_CERT_PATH="./certs/fullchain.pem"
export PROXY_TLS_KEY_PATH="./certs/privkey.pem"
export PROXY_SQLITE_PATH="./data/kidproxy.sqlite"
cargo run --
```

## SQLite output

The writer stores one row per completed exchange in the `proxy_events` table. Schema setup is handled by in-process SeaORM migrations only; the SeaORM CLI is not used.

## Development

Main modules:

- `src/main.rs`: startup, tracing, probe, shutdown orchestration
- `src/config.rs`: CLI to validated runtime config
- `src/tls.rs`: Rustls frontend and upstream configuration
- `src/proxy.rs`: Rama listener and reverse proxy path
- `src/capture.rs`: exchange capture and body observers
- `src/entity.rs`: SeaORM entity definitions
- `src/migration.rs`: programmatic database migrations
- `src/writer.rs`: bounded writer task and SQLite flush logic
- `src/probe.rs`: backend compatibility probe

Integration coverage lives in:

- `tests/basic_proxy.rs`
- `tests/streaming.rs`
- `tests/sqlite_output.rs`

## Notes

- Backend URLs must use `https://`.
- Header allowlists and denylists affect logged header JSON only, not forwarded traffic.
- Request and response bodies are always hashed and retained up to `body_max_bytes`, with truncation recorded when the body exceeds that cap.
- JA3 and JA4 are only recorded when Rama exposes enough metadata on the active Rustls path; otherwise those fields are null.
- The proxy aims to be protocol-boring, not magical. It will not spoof backend certificate identity for a different frontend hostname.
