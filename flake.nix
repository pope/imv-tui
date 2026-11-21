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
        "aarch64-darwin"
      ];
      overlay = final: _prev: {
        imv-tui = final.callPackage ./nix/imv-tui.nix { };
        imv-tui-static = final.pkgsStatic.callPackage ./nix/imv-tui.nix { };
        imv-tui-static-aarch64 =
          final.pkgsCross.aarch64-multiplatform-musl.pkgsStatic.callPackage ./nix/imv-tui.nix
            { };
      };
      sys-config =
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ overlay ];
          };
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
            cargo-tarpaulin
            cargo-watch
            clang
            clippy
            llvmPackages.bintools
            rust-analyzer
            rustc
            rustfmt
            samply
            self.formatter.${system}
          ];
        in
        {
          packages.${system} = rec {
            inherit (pkgs) imv-tui;
            default = imv-tui;
          }
          // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            inherit (pkgs) imv-tui-static;
          }
          // pkgs.lib.optionalAttrs (system == "x86_64-linux") {
            inherit (pkgs) imv-tui-static-aarch64;
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
    builtins.foldl' nixpkgs.lib.recursiveUpdate {
      overlays.default = overlay;
    } (map sys-config systems);
}
