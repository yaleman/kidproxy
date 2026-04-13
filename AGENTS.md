# Agents

You aren't done with a task until `mise check` passes and the system runs as requested.

Use package managers to manage dependencies instead of direct file editing. Refactor and reduce code sprawl while working. Don't plan for extensibility or backwards compatibility unless explicitly told to. Prefer simple, direct approaches instead of abstractions.

- `pnpm` instead of `npm`.
- Avoid OpenSSL at all costs.
- Avoid stringly-typed error categories and machine-readable status values. Use enums or dedicated structs internally, and only convert them to strings at storage or presentation boundaries.
- Never use the SeaORM CLI. Define and run migrations programmatically from Rust code only.

## UI rules

- Avoid subtitles in UIs unless they are clearly necessary.
- In documentation and comments, always use project-relative paths. Never use full on-disk paths because they are not portable and may expose private information.

## Rust Packages

- `clap` for CLI/environment/config
- `serde` for handling (de)serialization
- `sea-orm` for database-related things

## Blocked dependencies

- `serde_yaml` is blocked. Use `yaml-rust` for YAML parsing.

## Rust rules

- `.expect("<message>")` not `.unwrap()`
