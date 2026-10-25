# archipel for NixOS:
#
#   archipel = pkgs.callPackage "${archipel-src}/nix" { rustToolchain = …; };
#
# rustToolchain needs the wasm32-unknown-unknown target (rust-overlay's
# `rust-bin.stable.latest.default.override { targets = [ "wasm32-unknown-unknown" ]; }`
# for example). Pass `src` to build another checkout than this one.
#
# The game servers archipel starts are binaries it downloads for usual
# distributions: an Adoptium JRE (Minecraft), DepotDownloader (Steam games),
# Puck, Urban Terror. None of them find a loader or libraries on NixOS, so the
# whole app runs in an FHS environment and its servers inherit it. Some Steam
# servers are 32-bit (Fistful of Frags), hence multiArch: without it multiPkgs
# come in 64-bit only, and there is no /lib/ld-linux.so.2. Landlock still
# confines each server: archipel allows /nix and the FHS paths alike.
{ callPackage
, buildFHSEnv
, rustToolchain
, src ? ../.
}:

let
  unwrapped = callPackage ./package.nix { inherit rustToolchain src; };

  # Source servers link Debian's libcurl3-gnutls: libcurl-gnutls.so.4, its
  # symbols versioned CURL_GNUTLS_3 (Team Fortress 2's 64-bit replay_srv.so
  # will not load without it, and the server stops there). nixpkgs' gnutls
  # curl has the file name but versions its symbols CURL_GNUTLS_4, which the
  # loader refuses; Debian patches the 4 to 3, so do the same. And that file
  # name is only a symlink to a library named libcurl.so.4: the FHS
  # environment finds libraries through ldconfig, which goes by a library's
  # own name, so it needs a copy named libcurl-gnutls.so.4, as Debian's is.
  curlGnutls3 = p: (p.curlWithGnuTls.overrideAttrs (old: {
    postPatch = (old.postPatch or "") + ''
      substituteInPlace lib/libcurl.vers.in \
        --replace-fail '@CURL_LIBCURL_VERSIONED_SYMBOLS_SONAME@' 3
    '';
    postFixup = (old.postFixup or "") + ''
      rm $out/lib/libcurl-gnutls.so.4
      cp "$(readlink -f $out/lib/libcurl.so.4)" $out/lib/libcurl-gnutls.so.4
      patchelf --set-soname libcurl-gnutls.so.4 $out/lib/libcurl-gnutls.so.4
    '';
  })).out;
in
buildFHSEnv {
  name = "archipel";
  # DepotDownloader is self-contained .NET with invariant globalization, so no
  # ICU, but it loads libssl.so.3 at run time and aborts without it.
  targetPkgs = p: [ unwrapped p.openssl ];
  multiArch = true;
  multiPkgs = p: with p; [ glibc zlib stdenv.cc.cc.lib (curlGnutls3 p) ];
  runScript = "archipel-desktop";
  # archipel reads the firewall's units (and the store scripts they run) to
  # tell whether its ports are open; the FHS env's /etc leaves systemd out,
  # and without it archipel assumes the firewall turns players away.
  extraBwrapArgs = [ "--ro-bind" "/etc/systemd" "/etc/systemd" ];
  # Icons for desktop entries.
  extraInstallCommands = ''
    ln -s ${unwrapped}/share $out/share
  '';
  passthru = { inherit unwrapped; };
  meta = unwrapped.meta // { mainProgram = "archipel"; };
}
