fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let emit_ts = std::env::var("CINEMA_EMIT_TS").is_ok();

    let mut cfg = draad::codegen::Config::new()
        .root(&manifest_dir)
        .generated_rs(format!("{out_dir}/_generated.rs"))
        .skip_file("ws");

    if !emit_ts {
        cfg = cfg.rust_only();
    }

    cfg.generate().expect("draad codegen failed");

    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CINEMA_EMIT_TS");
}
