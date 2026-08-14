FROM rust:1.90-bookworm

RUN rustup component add --toolchain 1.90.0 rustfmt clippy
