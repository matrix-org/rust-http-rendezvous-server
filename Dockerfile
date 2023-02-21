# syntax = docker/dockerfile:1.4

# Builds a minimal image with the binary only. It is multi-arch capable,
# cross-building to aarch64 and x86_64. When cross-compiling, Docker sets two
# implicit BUILDARG: BUILDPLATFORM being the host platform and TARGETPLATFORM
# being the platform being built.
#
# Docker platform definitions look like this: linux/arm64 and linux/amd64, so
# there is a small script that translates those platforms to LLVM triples,
# respectively x86-64-unknown-linux-musl and aarch64-unknown-linux-musl

# The Debian version and version name must be in sync
ARG DEBIAN_VERSION=11
ARG DEBIAN_VERSION_NAME=bullseye
ARG RUSTC_VERSION=1.67.1
ARG ZIG_VERSION=0.10.1

####################################################
## Build stage, with cross-compilation done right ##
####################################################
FROM --platform=${BUILDPLATFORM} docker.io/library/rust:${RUSTC_VERSION}-slim-${DEBIAN_VERSION_NAME} AS builder

ARG ZIG_VERSION
ARG RUSTC_VERSION

# Make cargo use the git cli for fetching dependencies
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true

# Install the git, curl and xz
RUN apt update && apt install -y --no-install-recommends \
  git \
  curl \
  xz-utils

# Download zig compiler for cross-compilation
RUN curl -L "https://ziglang.org/download/${ZIG_VERSION}/zig-linux-$(uname -m)-${ZIG_VERSION}.tar.xz" | tar -J -x -C /usr/local && \
  ln -s "/usr/local/zig-linux-$(uname -m)-${ZIG_VERSION}/zig" /usr/local/bin/zig

WORKDIR /app
RUN cargo install --locked cargo-zigbuild cargo-auditable

# Install all cross-compilation targets
RUN rustup target add --toolchain "${RUSTC_VERSION}" \
  x86_64-unknown-linux-musl \
  aarch64-unknown-linux-musl

# Helper script that transforms docker platforms to LLVM triples
COPY ./scripts/docker-arch-to-rust-target.sh /

ARG TARGETPLATFORM

# Build the app
COPY ./ /app
RUN cargo auditable zigbuild \
  --release \
  --package matrix-http-rendezvous-server \
  --no-default-features \
  --target $(/docker-arch-to-rust-target.sh "${TARGETPLATFORM}")

# Move the binary to avoid having to guess its name in the next stage
RUN mv target/$(/docker-arch-to-rust-target.sh "${TARGETPLATFORM}")/release/matrix-http-rendezvous-server /usr/local/bin/matrix-http-rendezvous-server

##################################
## Runtime stage, debug variant ##
##################################
FROM --platform=${TARGETPLATFORM} gcr.io/distroless/static-debian${DEBIAN_VERSION}:debug-nonroot AS debug

COPY --from=builder /usr/local/bin/matrix-http-rendezvous-server /usr/local/bin/matrix-http-rendezvous-server

ENV LISTEN_ADDRESS=0.0.0.0
ENV LISTEN_PORT=8090
WORKDIR /
ENTRYPOINT ["/usr/local/bin/matrix-http-rendezvous-server"]
EXPOSE 8090

###################
## Runtime stage ##
###################
FROM --platform=${TARGETPLATFORM} gcr.io/distroless/static-debian${DEBIAN_VERSION}:nonroot

COPY --from=builder /usr/local/bin/matrix-http-rendezvous-server /usr/local/bin/matrix-http-rendezvous-server

ENV LISTEN_ADDRESS=0.0.0.0
ENV LISTEN_PORT=8090
WORKDIR /
ENTRYPOINT ["/usr/local/bin/matrix-http-rendezvous-server"]
EXPOSE 8090
