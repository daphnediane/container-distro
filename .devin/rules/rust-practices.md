---
description: Rust coding practices and testing guidelines
trigger: glob
globs: **/*.rs,**/Cargo.toml
---

# Rust Development Practices

## General

- Target latest stable Rust toolchain
- `cargo clippy` before committing; treat warnings as errors
- Format with `rustfmt` (default settings)
- Use `thiserror` for library errors, `anyhow` for application errors

## Code Style

- `snake_case` for functions, variables, modules
- `CamelCase` for types and traits
- `SCREAMING_SNAKE_CASE` for constants and statics
- Explicit types on public APIs, elide when obvious locally
- Keep functions short and focused
- Avoid `unwrap()`/`expect()` in non-test code; use `?`
- Use `#[must_use]` on important return values

## Derive and Traits

- Derive `Debug` on all public types
- Derive `Clone`, `PartialEq` where meaningful
- Derive `Serialize`, `Deserialize` for types crossing serialization boundaries
- Use `#[serde(rename_all = "camelCase")]` for JSON schema compliance

## Module Organization

- One primary type per file; related small types may share
- Re-export public items from `mod.rs` for clean external API
- Group `use` statements: std, external crates, crate-internal

## Testing

- Every data module must have `#[cfg(test)] mod tests` block
- Test serialization round-trips for all data structs
- Use `include_str!` or test fixtures in `tests/` directory for JSON samples
- Name tests descriptively: `test_event_deserialize_minimal`
- Run `cargo test` at workspace root before committing

## Dependencies

- Pin major versions in `Cargo.toml` (e.g., `serde = "1"`)
- Keep dependency tree minimal; prefer well-maintained crates
- Use feature flags for optional functionality

## CLI Specifics

- Forward exit codes from spawned child processes
- Use `exec()` for terminal passthrough commands (TTY, signals)
- Parse `container` CLI JSON output via serde structs, not string scraping
