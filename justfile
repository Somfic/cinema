set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

# ts-rs per-type intermediates land here (under target/, never in src/).
# Read by `draad-codegen` via the same env var.
export TS_RS_EXPORT_DIR := justfile_directory() / "target" / "draad-bindings"

default:
    just dev

dev: schema
    cargo run -- --dev

build: schema
    cargo build --release

# Regenerate the TypeScript schema artifacts. The Rust `_generated.rs` is
# produced by `build.rs` on every `cargo build` and lands in `OUT_DIR`, so
# only the ts-rs export and the draad TS pass need an explicit pass.
schema:
    cargo test --quiet export_bindings || true
    CINEMA_EMIT_TS=1 cargo build --quiet

check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    cd frontend && bun run check

clean-schema:
    rm -f src/_generated.rs
    rm -rf target/draad-bindings
    find frontend/src/lib/schema -maxdepth 1 -name '*.ts' \
        ! -name 'rpc.ts' ! -name 'error.ts' -delete
