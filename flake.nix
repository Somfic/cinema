{
  description = "Cinema dev environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { nixpkgs, rust-overlay, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      eachSystem = f: nixpkgs.lib.genAttrs systems (system: f (
        import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        }
      ));
    in
    {
      devShells = eachSystem (pkgs: {
        default = pkgs.mkShell {
          buildInputs = with pkgs; [
            # Rust
            (rust-bin.stable.latest.default.override {
              extensions = [ "rust-src" "rust-analyzer" ];
            })
            cargo-watch
            sqlx-cli # `just db` applies migrations before the query! macros run
            just

            # Frontend
            bun
            ffmpeg
            yt-dlp
            deno # JS runtime yt-dlp needs to solve YouTube's nsig/JS challenges

            pkg-config
            openssl
            # Apple SDK frameworks come from the stdenv on darwin now — the old
            # `darwin.apple_sdk.frameworks` stubs were removed from nixpkgs.
          ];

          env = {
          };
        };
      });
    };
}
