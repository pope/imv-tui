{
  description = "imv-tui is a TUI for viewing images with zooming and panning";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    llm-agents = {
      url = "github:numtide/llm-agents.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    jailed-agents = {
      url = "github:andersonjoseph/jailed-agents";
      inputs = {
        nixpkgs.follows = "nixpkgs";
        llm-agents.follows = "llm-agents";
      };
    };
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      jailed-agents,
      nixpkgs,
      llm-agents,
      self,
      treefmt-nix,
      ...
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      sys-config =
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          treefmt-eval = treefmt-nix.lib.evalModule pkgs (_: {
            projectRootFile = "flake.nix";
            programs = {
              deadnix.enable = true;
              mdformat = {
                enable = true;
                plugins =
                  ps: with ps; [
                    mdformat-frontmatter
                    mdformat-gfm
                  ];
                settings = {
                  end-of-line = "lf";
                  number = true;
                };
              };
              nixfmt.enable = true;
              rustfmt.enable = true;
              statix.enable = true;
            };
            settings.global.excludes = [
              ".envrc"
              "target/*"
            ];
          });
          packages = with pkgs; [
            cargo
            cargo-flamegraph
            cargo-nextest
            cargo-watch
            clang
            clippy
            mold
            rust-analyzer
            rustc
            rustfmt
            samply
            self.formatter.${system}
          ];
        in
        {
          packages.${system} = rec {
            imv-tui = pkgs.rustPlatform.buildRustPackage {
              pname = "imv-tui";
              version = "0.1.0";
              src = ./.;
              cargoLock = {
                lockFile = ./Cargo.lock;
              };
            };
            default = imv-tui;
          }
          // pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
            imv-tui-static = pkgs.pkgsStatic.rustPlatform.buildRustPackage {
              pname = "imv-tui";
              version = "0.1.0";
              src = ./.;
              cargoLock = {
                lockFile = ./Cargo.lock;
              };
              target = "x86_64-unknown-linux-musl";
            };
          };
          devShells.${system}.default = pkgs.mkShell {
            packages = packages ++ [
              (jailed-agents.lib.${system}.makeJailedAgent {
                name = "agy";
                pkg = llm-agents.packages.${system}.antigravity-cli;
                configPaths = [ "~/.gemini" ];
                extraPkgs = packages;
              })
            ];
          };
          formatter.${system} = treefmt-eval.config.build.wrapper;
          checks.${system} = {
            inherit (self.packages.${system}) imv-tui;
            treefmt = treefmt-eval.config.build.check self;
          };
        };
    in
    builtins.foldl' nixpkgs.lib.recursiveUpdate { } (map sys-config systems);
}
