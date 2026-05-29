set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

# ts-rs per-type intermediates land here (under target/, never in src/).
# Read by `cinema-schema-codegen` via the same env var.
export TS_RS_EXPORT_DIR := justfile_directory() / "target" / "schema-bindings"

default:
    just dev

dev: schema
    cargo run -- --dev

build: schema
    cargo build --release

# Regenerate every schema artifact: backend `_generated.rs` then the
# `frontend/src/lib/schema/*.ts` files. Run this after editing any trait under
# `src/api/` or any `#[cinema_type]` body.
schema:
    cargo run -p cinema-schema-codegen --quiet -- --rust-only
    cargo test --quiet export_bindings || true
    cargo run -p cinema-schema-codegen --quiet
    cargo fmt -p cinema

check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    cd frontend && bun run check

clean-schema:
    rm -f src/_generated.rs
    rm -rf target/schema-bindings
    find frontend/src/lib/schema -maxdepth 1 -name '*.ts' \
        ! -name 'rpc.ts' ! -name 'error.ts' -delete
