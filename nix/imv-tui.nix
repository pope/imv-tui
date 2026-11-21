{
  lib,
  llvmPackages,
  rustPlatform,
  stdenv,
  # CPU target for rustc (`-C target-cpu=<cpu>`) and C deps (`-march=<cpu>`).
  # Examples: null (target baseline), "x86-64-v2", "x86-64-v3", "native".
  cpu ? (if stdenv.hostPlatform.isLinux && stdenv.hostPlatform.isx86_64 then "x86-64-v3" else null),
}:
let
  rustTargetEnv = lib.replaceStrings [ "-" ] [ "_" ] (
    lib.toUpper stdenv.hostPlatform.rust.rustcTargetSpec
  );
  useLld =
    stdenv.hostPlatform.isLinux && stdenv.hostPlatform.isx86_64 && !stdenv.hostPlatform.isStatic;
  extraRustFlags =
    lib.optionals useLld [
      "-C link-arg=-fuse-ld=lld"
      "-C split-debuginfo=unpacked"
    ]
    ++ lib.optionals (cpu != null) [
      "-C target-cpu=${cpu}"
    ];
in
rustPlatform.buildRustPackage {
  pname = "imv-tui";
  version = "0.1.0";
  src = ../.;

  cargoLock = {
    lockFile = ../Cargo.lock;
  };

  nativeBuildInputs = lib.optionals useLld [
    llvmPackages.bintools
  ];

  env =
    lib.optionalAttrs (extraRustFlags != [ ]) {
      "CARGO_TARGET_${rustTargetEnv}_RUSTFLAGS" = lib.concatStringsSep " " extraRustFlags;
    }
    // lib.optionalAttrs (cpu != null) {
      NIX_CFLAGS_COMPILE = "-march=${cpu}";
    };

  meta = {
    description = "TUI for viewing images with zooming and panning";
    mainProgram = "imv-tui";
  };
}
