# archipel's desktop app, built from this checkout. Wrapped for NixOS by
# ./default.nix; use that one.
{ lib
, pkgs
, makeRustPlatform
  # A Rust toolchain with the wasm32-unknown-unknown target, for the UI.
, rustToolchain
, src ? ../.
, pkg-config
, wrapGAppsHook3
, webkitgtk_4_1
, gtk3
, librsvg
, openssl
, glib-networking
, trunk
, binaryen
}:

let
  # The UI is Rust compiled to wasm32, so it needs a toolchain with that target.
  rustPlatform = makeRustPlatform { cargo = rustToolchain; rustc = rustToolchain; };

  # trunk insists on a wasm-bindgen CLI of exactly the version in Cargo.lock (it
  # would otherwise download one, which the sandbox forbids). nixpkgs keeps one
  # attribute per version, so pick the one the lockfile names; a bump upstream
  # past what nixpkgs carries fails here, by name, rather than deep in trunk.
  lock = builtins.fromTOML (builtins.readFile "${src}/Cargo.lock");
  wasmBindgenVersion = (lib.findFirst (p: p.name == "wasm-bindgen") null lock.package).version;
  wasm-bindgen-cli = pkgs."wasm-bindgen-cli_${lib.replaceStrings [ "." ] [ "_" ] wasmBindgenVersion}";

  version = "0.1.0-${src.shortRev or "dirty"}";

  # crates/archipel-ui/dist, which the archipel crate embeds (rust-embed) into
  # the desktop binary.
  ui = rustPlatform.buildRustPackage {
    pname = "archipel-ui";
    inherit version src;
    cargoLock.lockFile = "${src}/Cargo.lock";

    nativeBuildInputs = [ trunk wasm-bindgen-cli binaryen ];

    buildPhase = ''
      runHook preBuild
      cd crates/archipel-ui
      HOME=$TMPDIR trunk build --release --offline
      runHook postBuild
    '';
    installPhase = ''
      runHook preInstall
      cp -r dist $out
      runHook postInstall
    '';
    doCheck = false;
  };
in
rustPlatform.buildRustPackage {
  pname = "archipel";
  # No releases yet: the version names the commit when the source knows it.
  inherit version src;

  cargoLock.lockFile = "${src}/Cargo.lock";

  preBuild = ''
    cp -r ${ui} crates/archipel-ui/dist
    chmod -R u+w crates/archipel-ui/dist
  '';

  # What `cargo tauri build` does, minus the bundling (.deb, AppImage): the
  # custom-protocol feature is what makes it a production build.
  cargoBuildFlags = [ "--package" "archipel-desktop" ];
  buildFeatures = [ "tauri/custom-protocol" ];
  # The tests need root and cgroups (they're archipel the server's).
  doCheck = false;

  nativeBuildInputs = [ pkg-config wrapGAppsHook3 ];
  buildInputs = [ webkitgtk_4_1 gtk3 librsvg openssl glib-networking ];

  postInstall = ''
    for size in 32x32 128x128; do
      install -Dm644 crates/archipel-desktop/icons/$size.png \
        $out/share/icons/hicolor/$size/apps/archipel.png
    done
    install -Dm644 crates/archipel-desktop/icons/128x128@2x.png \
      $out/share/icons/hicolor/256x256/apps/archipel.png
  '';

  # WebKitGTK's DMA-BUF renderer draws a blank window on the nvidia driver.
  preFixup = ''
    gappsWrapperArgs+=(--set-default WEBKIT_DISABLE_DMABUF_RENDERER 1)
  '';

  passthru = { inherit ui; };

  meta = {
    description = "Game servers on your own computer";
    homepage = "https://github.com/Sigma-studios/archipel";
    license = lib.licenses.mit;
    mainProgram = "archipel-desktop";
    platforms = lib.platforms.linux;
  };
}
