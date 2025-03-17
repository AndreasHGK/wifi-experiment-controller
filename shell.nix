{
  pkgs ? import <nixpkgs> { },
}:
pkgs.mkShell {
  packages = with pkgs; [
    cargo
    openssl
    pkg-config
    rustc
    rustfmt
    rust-analyzer
  ];
}
