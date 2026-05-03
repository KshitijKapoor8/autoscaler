# ── Stage 1: build ──────────────────────────────────────────────────────────
FROM rust:latest AS builder
WORKDIR /build

RUN rustup target add x86_64-unknown-linux-musl \
    && apt-get update \
    && apt-get install -y --no-install-recommends musl-tools \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main(){}' > src/main.rs \
    && cargo build --release --target x86_64-unknown-linux-musl \
    && rm -rf src

COPY src ./src
RUN touch src/main.rs && cargo build --release --target x86_64-unknown-linux-musl

FROM alpine:3
RUN apk add --no-cache ca-certificates

COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/autoscaler /usr/local/bin/autoscaler

EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/autoscaler"]
