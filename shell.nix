{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  packages = with pkgs; [
    cargo
    rustc
    rustfmt
    clippy
    sccache
    pkg-config
    openssl
    sqlite
    python3
    ffmpeg
    yt-dlp
  ];

  RUST_BACKTRACE = "1";
}
