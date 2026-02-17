FROM rust:latest

RUN apt-get update && apt-get install -y --no-install-recommends \
    clang \
    llvm \
    libssl-dev \
    pkg-config \
    ca-certificates \
    git \
  && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_NO_UPDATE_CHECK=1 \
  RUSTUP_AUTO_INSTALL=0 \
  RUSTUP_TOOLCHAIN=stable \
  CARGO_TERM_COLOR=always \
  RUST_BACKTRACE=1

RUN rustup default stable && \
  rustup component add rustfmt clippy || true
RUN cargo install cargo-tarpaulin || true

WORKDIR /usr/src/reaper

# default to an interactive shell; helper scripts will run specific commands.
CMD ["/bin/bash"]
