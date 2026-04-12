# kidproxy

`kidproxy` is a single-backend HTTPS reverse proxy built with Rama and Rustls. It terminates frontend TLS, forwards traffic to one configured `https://` backend, preserves request and response flow as closely as practical, and writes one Parquet row per completed exchange.

## What v1 does

- Accepts HTTPS on one listen address with one frontend certificate.
- Proxies to one fixed HTTPS backend with Rustls certificate verification.
- Preserves method, path, query, status, headers, and streaming bodies without intentional payload rewriting.
- Captures request, response, connection, timing, and TLS metadata.
- Writes batched Parquet files under a time-partitioned directory layout.
- Drops log events under pressure instead of blocking proxy traffic.

## What v1 does not do

- Dynamic routing or multiple backends.
- Config files.
- Request or response body rewriting.
- Header rewriting beyond hop-by-hop filtering required for correct proxying.
- Impossible certificate mirroring between different frontend and backend hostnames.

## CLI

All operational flags have `PROXY_*` environment variable equivalents via `clap` derive and `env`.

Required flags:

- `--listen-addr`
- `--frontend-domain`
- `--backend-url`
- `--tls-cert-path`
- `--tls-key-path`
- `--parquet-dir`

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
  --parquet-dir ./data/parquet
```

Environment example:

```bash
export PROXY_LISTEN_ADDR="0.0.0.0:8443"
export PROXY_FRONTEND_DOMAIN="example1.com"
export PROXY_BACKEND_URL="https://example2.com"
export PROXY_TLS_CERT_PATH="./certs/fullchain.pem"
export PROXY_TLS_KEY_PATH="./certs/privkey.pem"
export PROXY_PARQUET_DIR="./data/parquet"
cargo run --
```

## Parquet output

Files are written under:

```text
parquet_dir/YYYY/MM/DD/HH/
```

Each file contains one or more completed exchange rows with typed top-level columns for hot fields and JSON string columns for variable structures such as headers and cookies.

## Development

Main modules:

- `src/main.rs`: startup, tracing, probe, shutdown orchestration
- `src/config.rs`: CLI to validated runtime config
- `src/tls.rs`: Rustls frontend and upstream configuration
- `src/proxy.rs`: Rama listener and reverse proxy path
- `src/capture.rs`: exchange capture and body observers
- `src/schema.rs`: Arrow schema and record-batch conversion
- `src/writer.rs`: bounded writer task and Parquet flush logic
- `src/probe.rs`: backend compatibility probe

Integration coverage lives in:

- `tests/basic_proxy.rs`
- `tests/streaming.rs`
- `tests/parquet_output.rs`

## Notes

- Backend URLs must use `https://`.
- Header allowlists and denylists affect logged header JSON only, not forwarded traffic.
- Request and response bodies are always hashed and retained up to `body_max_bytes`, with truncation recorded when the body exceeds that cap.
- JA3 and JA4 are only recorded when Rama exposes enough metadata on the active Rustls path; otherwise those fields are null.
- The proxy aims to be protocol-boring, not magical. It will not spoof backend certificate identity for a different frontend hostname.
