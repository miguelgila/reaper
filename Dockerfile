FROM rust:latest

RUN apt-get update && apt-get install -y --no-install-recommends \
    clang \
    llvm \
    libssl-dev \
    pkg-config \
    ca-certificates \
    git \
  && rm -rf /var/lib/apt/lists/*

RUN rustup component add rustfmt || true
RUN cargo install cargo-tarpaulin || true

WORKDIR /usr/src/reaper

# default to an interactive shell; helper scripts will run specific commands.
CMD ["/bin/bash"]
