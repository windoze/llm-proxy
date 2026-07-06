# syntax=docker/dockerfile:1

# Building on Alpine means the default Rust target is *-unknown-linux-musl,
# which links statically by default (crt-static). No cross-compilation and no
# --target flag are needed: on a native amd64 or arm64 host this produces a
# fully static binary for that architecture out of the box.

ARG RUST_VERSION=1
ARG ALPINE_VERSION=3.21

########################  builder  ########################
FROM rust:${RUST_VERSION}-alpine AS builder

# musl-dev provides the C toolchain bits some crates (e.g. ring) need to build.
RUN apk add --no-cache musl-dev

WORKDIR /app

# Warm the dependency cache first so source-only edits don't refetch crates.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src

COPY . .
# `touch` guarantees the real main.rs is newer than the stub-built artifact so
# cargo relinks the actual binary rather than reusing the placeholder.
RUN touch src/main.rs \
    && cargo build --release --locked \
    && strip target/release/llm-proxy

########################  runtime deps  ########################
# A throwaway stage that only exists to hand tini and the CA bundle to scratch.
# tini-static (not tini) is used because scratch has no musl loader to run a
# dynamically-linked binary.
FROM alpine:${ALPINE_VERSION} AS runtime-deps
RUN apk add --no-cache tini-static ca-certificates

########################  final image  ########################
FROM scratch

COPY --from=runtime-deps /sbin/tini-static /sbin/tini
COPY --from=runtime-deps /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder /app/target/release/llm-proxy /usr/local/bin/llm-proxy

# Bind on all interfaces so the proxy is reachable from outside the container
# (the in-process default is 127.0.0.1:8080).
ENV LLM_PROXY_ADDR=0.0.0.0:8080

EXPOSE 8080

# Run unprivileged; the binary only needs an unprivileged port. scratch has no
# /etc/passwd, so use the numeric nobody UID.
USER 65534:65534

# tini reaps zombies and forwards signals for clean shutdown.
ENTRYPOINT ["/sbin/tini", "--", "/usr/local/bin/llm-proxy"]
