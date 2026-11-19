{
  description = "content-extract: harness-independent content extraction with per-platform login";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";

    # Only the DSH host under hosts/dsh/ needs it: that host is a cordis bundle,
    # so it has to be built by the harness's own buildDshBundle, which writes the
    # nix-support/dsh-bundles.json manifest the DSH resolver reads. Every other
    # harness host links the C ABI instead and needs nothing from this input.
    deepseek-harness.url = "github:moraxyc/deepseek-harness.nix";
    deepseek-harness.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    { self
    , nixpkgs
    , flake-utils
    , deepseek-harness
    ,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        inherit (pkgs) lib;

        # Every crate in this workspace is a plain native build, so the
        # vendoring and metadata are shared; only the build target and install
        # step differ.
        common = {
          version = "0.3.0";
          src = lib.cleanSourceWith {
            src = ./.;
            filter =
              path: type:
              let
                base = baseNameOf path;
              in
              !(lib.hasPrefix "result" base || base == "target" || lib.hasPrefix "." base);
          };
        };

        cargoLock = {
          lockFile = ./Cargo.lock;
        };

        meta = with lib; {
          description = "Content extraction with per-platform login (bilibili, YouTube, news.wmpvp.com, ...)";
          homepage = "https://github.com/DataEraserC/content-extract";
          license = licenses.mit;
          mainProgram = "cx";
        };
      in
      {
        packages = rec {
          # The CLI. Self-contained: no harness, no runtime deps beyond the
          # system TLS store, which is baked in via `rustls` + `rustls-native-certs`
          # is NOT used — we rely on the OS store through `rustls-tls` with
          # webpki roots for maximum reproducibility inside the sandbox.
          cx = pkgs.rustPlatform.buildRustPackage (common // {
            pname = "cx";
            inherit cargoLock meta;

            buildType = "release";

            # Only the CLI binary is installed; the cdylib comes from
            # `extract-abi` below.
            cargoBuildFlags = [ "-p" "extract-cli" ];

            # `cargo test` needs network for nothing, so the unit tests (cookie
            # codec, ptuiCB parser) can run in the sandbox.
            checkFlags = [ ];

            postInstall = ''
              install -Dm644 README.md $out/share/doc/content-extract/README.md
            '';
          });

          # The C shared library the dsh plugin loads through koffi.
          extract-abi = pkgs.rustPlatform.buildRustPackage (common // {
            pname = "extract-abi";
            inherit cargoLock meta;

            cargoBuildFlags = [ "-p" "extract-abi" ];

            # buildRustPackage installs `bin/` by default; there is none here,
            # so install the cdylib by hand.
            installPhase = ''
              runHook preInstall
              mkdir -p $out/lib
              find target -name 'libextract.so' -exec cp -v {} $out/lib/ \;
              test -f $out/lib/libextract.so
              runHook postInstall
            '';

            # No tests to run for the ABI crate itself.
            doCheck = false;
          });

          # The DeepSeek Harness host: the cordis plugin, its Web UI page and
          # the koffi FFI loader, packaged as an npm bundle with the matching
          # `libextract.so` beside it. Other harnesses get a sibling host
          # directory that links the same C ABI.
          dsh =
            let
              dshPkgs = pkgs.extend deepseek-harness.overlays.default;
              abi = pkgs.rustPlatform.buildRustPackage (common // {
                pname = "extract-abi-native";
                inherit cargoLock meta;
                cargoBuildFlags = [ "-p" "extract-abi" ];
                installPhase = ''
                  runHook preInstall
                  mkdir -p $out/lib
                  find target -name 'libextract.so' -exec cp -v {} $out/lib/ \;
                  test -f $out/lib/libextract.so
                  runHook postInstall
                '';
                doCheck = false;
              });
            in
            # Must be buildDshBundle, not plain buildNpmPackage: the DSH resolver
            # reads nix-support/dsh-bundles.json to discover a bundle, and only
            # this builder writes it (schema 1, one row pointing at the plugin's
            # cordis patch and package root).
            dshPkgs.dsh.buildDshBundle (finalAttrs: {
              pname = "dsh-content-extract";
              version = (lib.importJSON ./hosts/dsh/package.json).version;

              src = ./hosts/dsh;

              npmDepsHash = "sha256-XGaMRfO+3cOmDYwNloTjuc3jycxquxnfsqKTP5mXfAo=";

              # The plugin ships prebuilt JS (no bundler step).
              dontNpmBuild = true;

              # test/live.mjs needs a browser and a real account.
              doCheck = false;

              # The kernel + platforms ship beside the host; the koffi loader finds
              # it at native/ without any environment variable.
              installation = dshPkgs.dsh.dsh-runtime;

              postInstall = ''
                mkdir -p $out/lib/node_modules/dsh-content-extract/native
                install -m 0755 ${abi}/lib/libextract.so \
                  $out/lib/node_modules/dsh-content-extract/native/libextract.so
                test -f $out/lib/node_modules/dsh-content-extract/lib/client.js
                test -f $out/lib/node_modules/dsh-content-extract/native/libextract.so
              '';

              meta = meta // {
                description = "Content extraction for DeepSeek Harness with per-platform login and ffmpeg compression";
                mainProgram = "dsh-content-extract";
              };
            });

          default = cx;
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
            rust-analyzer
            just
          ];
          env.RUST_BACKTRACE = "1";
        };

        formatter = pkgs.nixpkgs-fmt;
      }
    );
}
