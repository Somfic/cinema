use std::path::Path;

/// CLI entry. Run from the cinema repo root: regenerates `src/_generated.rs`
/// and (without `--rust-only`) every `frontend/src/lib/schema/<module>.ts`.
fn main() {
    let rust_only = std::env::args().skip(1).any(|a| a == "--rust-only");
    cinema_schema_codegen::generate(Path::new("."), rust_only);
}
