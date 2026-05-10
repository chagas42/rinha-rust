# syntax=docker/dockerfile:1.7
FROM --platform=linux/amd64 rust:1.83-slim-bookworm AS builder
WORKDIR /app

RUN apt-get update \
 && apt-get install -y --no-install-recommends musl-tools \
 && rm -rf /var/lib/apt/lists/* \
 && rustup target add x86_64-unknown-linux-musl

COPY Cargo.toml Cargo.lock* ./
COPY src ./src

ENV RUSTFLAGS="-C target-cpu=haswell -C target-feature=+crt-static"
RUN cargo test --release --lib --target x86_64-unknown-linux-musl
RUN cargo build --release --target x86_64-unknown-linux-musl

COPY references.json.gz ./
ARG N_CENTROIDS=1024
ARG KMEANS_ITERS=10
RUN /app/target/x86_64-unknown-linux-musl/release/preprocess ./references.json.gz /app/index.bin "$N_CENTROIDS" "$KMEANS_ITERS" \
 && rm ./references.json.gz

FROM scratch
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/rinha-2026 /usr/local/bin/rinha-2026
COPY --from=builder /app/index.bin /app/index.bin
ENV INDEX_PATH=/app/index.bin
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/rinha-2026"]
