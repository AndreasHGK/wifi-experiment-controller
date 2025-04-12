{
  pkgs ? import <nixpkgs> { },
}:
pkgs.mkShell {
  packages = with pkgs; [
    cargo
    libpcap
    openssl
    pkg-config
    rustc
    rustfmt
    rust-analyzer
    tshark
  ];
}
