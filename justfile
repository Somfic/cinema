set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

# `.envrc` sets these, but it starts with `use flake` and aborts there when nix
# is missing -- taking the exports with it. Falling back to the credentials in
# docker-compose.yaml keeps a bare `just` working; a value already in the
# environment (direnv, CI) always wins.
export DATABASE_URL := env("DATABASE_URL", "postgresql://root:password@localhost:5434/cinema_db")
export CINEMA_DATABASE_URL := env("CINEMA_DATABASE_URL", DATABASE_URL)
export TS_RS_EXPORT_DIR := env("TS_RS_EXPORT_DIR", "./target/draad-bindings/")

default:
    just dev

# Install Rust crates and frontend (bun) dependencies.
install:
    cargo fetch
    cd frontend && bun install

# sqlx's `query!` macros talk to a live database *while compiling*, so nothing
# builds until this has run once. `--wait` blocks on the compose healthcheck and
# returns immediately when the container is already up, so every other recipe
# can depend on this cheaply. Migrations are applied by `sqlx::migrate!` at
# startup (src/app.rs), so no `sqlx migrate run` and no sqlx-cli on PATH.
# Bring up Postgres.
db:
    docker compose up -d --wait db

# Needs only bun and a generated `frontend/src/lib/schema/index.ts` (run
# `just schema` once to create it); skips the database and the Rust build.
# Run only the vite dev server, against an already-running backend.
frontend:
    cd frontend && bun run dev -- --strictPort

# concurrently is resolved from frontend's node_modules, so run it from there
# and bounce back to the root for cargo. `-k` tears down both if either exits.
# Run the backend and the vite dev server side by side.
dev: schema
    cd frontend && bunx concurrently -k -n backend,frontend -c blue,green \
        "cd .. && cargo run" \
        "bun run dev -- --strictPort"

build: schema
    cargo build --release

# `draad::include_generated!` runs the whole codegen during macro expansion, so
# a plain `cargo build` writes `frontend/src/lib/schema/index.ts` as a side
# effect.
# Regenerate the TypeScript schema.
schema: db
    cargo build --quiet

check: db
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    cd frontend && bun run check

clean-schema:
    find frontend/src/lib/schema -maxdepth 1 -name '*.ts' \
        ! -name 'rpc.ts' ! -name 'error.ts' -delete
