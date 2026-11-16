{
  description = "video-login: harness-independent multi-channel login for video sites";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    { self
    , nixpkgs
    , flake-utils
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
          version = "0.1.0";
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
          description = "Multi-channel login for video sites (bilibili, douyin, YouTube, ...)";
          homepage = "https://github.com/DataEraserC/video-login";
          license = licenses.mit;
          mainProgram = "vlogin";
        };
      in
      {
        packages = rec {
          # The CLI. Self-contained: no harness, no runtime deps beyond the
          # system TLS store, which is baked in via `rustls` + `rustls-native-certs`
          # is NOT used — we rely on the OS store through `rustls-tls` with
          # webpki roots for maximum reproducibility inside the sandbox.
          vlogin = pkgs.rustPlatform.buildRustPackage (common // {
            pname = "vlogin";
            inherit cargoLock meta;

            buildType = "release";

            # Only the CLI binary is installed; the cdylib comes from
            # `vlogin-abi` below.
            cargoBuildFlags = [ "-p" "vlogin-cli" ];

            # `cargo test` needs network for nothing, so the unit tests (cookie
            # codec, ptuiCB parser) can run in the sandbox.
            checkFlags = [ ];

            postInstall = ''
              install -Dm644 README.md $out/share/doc/video-login/README.md
            '';
          });

          # The C shared library the dsh plugin loads through koffi.
          vlogin-abi = pkgs.rustPlatform.buildRustPackage (common // {
            pname = "vlogin-abi";
            inherit cargoLock meta;

            cargoBuildFlags = [ "-p" "vlogin-abi" ];

            # buildRustPackage installs `bin/` by default; there is none here,
            # so install the cdylib by hand.
            installPhase = ''
              runHook preInstall
              mkdir -p $out/lib
              find target -name 'libvlogin.so' -exec cp -v {} $out/lib/ \;
              test -f $out/lib/libvlogin.so
              runHook postInstall
            '';

            # No tests to run for the ABI crate itself.
            doCheck = false;
          });

          # The DeepSeek Harness host: the cordis plugin, its Web UI page and
          # the koffi FFI loader, packaged as an npm bundle with the matching
          # `libvlogin.so` beside it. Other harnesses get a sibling host
          # directory that links the same C ABI.
          dsh =
            let
              abi = pkgs.rustPlatform.buildRustPackage (common // {
                pname = "vlogin-abi-native";
                inherit cargoLock meta;
                cargoBuildFlags = [ "-p" "vlogin-abi" ];
                installPhase = ''
                  runHook preInstall
                  mkdir -p $out/lib
                  find target -name 'libvlogin.so' -exec cp -v {} $out/lib/ \;
              test -f $out/lib/libvlogin.so
                  runHook postInstall
                '';
                doCheck = false;
              });
            in
            pkgs.buildNpmPackage {
              pname = "dsh-video-fetch";
              version = (lib.importJSON ./hosts/dsh/package.json).version;

              src = ./hosts/dsh;

              npmDepsHash = "sha256-XGaMRfO+3cOmDYwNloTjuc3jycxquxnfsqKTP5mXfAo=";

              # The plugin ships prebuilt JS (no bundler step); the package.json
              # `files` whitelist drives the copy.
              dontNpmBuild = true;

              # test/live.mjs needs a browser and a real account.
              doCheck = false;

              installPhase = ''
                runHook preInstall
                mkdir -p $out/lib/node_modules/dsh-video-fetch
                cp -r lib cordis.patch.yml package.json $out/lib/node_modules/dsh-video-fetch/
                mkdir -p $out/lib/node_modules/dsh-video-fetch/native
                cp ${abi}/lib/libvlogin.so $out/lib/node_modules/dsh-video-fetch/native/libvlogin.so
                runHook postInstall
              '';

              meta = meta // {
                description = "Video downloading for DeepSeek Harness with per-channel login and ffmpeg compression";
                mainProgram = "dsh-video-fetch";
              };
            };

          default = vlogin;
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
