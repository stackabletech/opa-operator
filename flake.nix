{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };
  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem
      (system:
        let
          overlays = [ (import rust-overlay) ];
          pkgs = import nixpkgs {
            inherit system overlays;
          };
          rustToolchain = pkgs.pkgsBuildHost.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          beku = pkgs.python3Packages.buildPythonApplication rec {
            pname = "beku-stackabletech";
            version = "0.0.10";
            pyproject = true;
            src = pkgs.fetchFromGitHub {
              owner = "stackabletech";
              repo = "beku.py";
              rev = "${version}";
              sha256 = "sha256-LaJdrNG5/fwpBl+Z0OmhSsM3uZXk7KX8QTCEC3xWXpQ=";
            };
            build-system = with pkgs.python3Packages; [ setuptools ];
            propagatedBuildInputs = with pkgs.python3Packages; [ jinja2 pyyaml ];
            postConfigure = "echo -e \"from setuptools import setup\\nsetup()\" > setup.py";
          };
          nativeBuildInputs = with pkgs; [ rustToolchain pkg-config gnumake cmake crate2nix krb5 zlib glibc clang protobuf ];
          buildInputs = with pkgs; [
            openssl
            kuttl
            python3
            beku
          ] ++ (with pkgs.python3Packages; [
            jinja2
            jinja2-cli
          ]);
        in
        with pkgs;
        {
          devShells.default = mkShell {
            inherit buildInputs nativeBuildInputs;
            LIBCLANG_PATH = "${llvmPackages.libclang.lib}/lib";
            BINDGEN_EXTRA_CLANG_ARGS = "-isystem ${llvmPackages.libclang.lib}/lib/clang/${lib.getVersion clang}/include";
          };
        }
      );
}
