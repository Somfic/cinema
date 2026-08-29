set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

default:
    just dev

# Install Rust crates and frontend (bun) dependencies.
install:
    cargo fetch
    cd frontend && bun install

# Bring up Postgres and apply the migrations. sqlx's `query!` macros talk to a
# live database *while compiling*, so nothing builds until this has run once.
# `--wait` blocks on the compose healthcheck; it returns immediately when the
# container is already up, so every other recipe can depend on this cheaply.
db:
    docker compose up -d --wait db
    sqlx migrate run

# concurrently is resolved from frontend's node_modules, so run it from there
# and bounce back to the root for cargo. `-k` tears down both if either exits.
# Run backend (--dev) and the vite dev server side by side.
dev: schema
    cd frontend && bunx concurrently -k -n backend,frontend -c blue,green \
        "cd .. && cargo run" \
        "bun run dev -- --strictPort"

build: schema
    cargo build --release

# Regenerate the TypeScript schema. `draad::include_generated!` runs the
# whole codegen during macro expansion, so a plain `cargo build` writes the
# fresh `frontend/src/lib/schema/index.ts` as a side effect.
schema: db
    cargo build --quiet

check: db
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    cd frontend && bun run check

clean-schema:
    find frontend/src/lib/schema -maxdepth 1 -name '*.ts' \
        ! -name 'rpc.ts' ! -name 'error.ts' -delete
