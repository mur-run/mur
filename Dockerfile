# ── Stage 1: Builder ─────────────────────────────────────────────────────────
FROM rust:1.82-bookworm AS builder

WORKDIR /app

# Cache dependency compilation separately from source
COPY Cargo.toml Cargo.lock ./
COPY mur-common/Cargo.toml ./mur-common/
COPY mur-core/Cargo.toml ./mur-core/

# Create stub libs so cargo can fetch + compile deps without full source
RUN mkdir -p mur-common/src mur-core/src && \
    echo "pub fn stub() {}" > mur-common/src/lib.rs && \
    echo "fn main() {}" > mur-core/src/main.rs

RUN cargo build --release 2>/dev/null || true

# Now copy real source and build for real
COPY mur-common/src ./mur-common/src
COPY mur-core/src   ./mur-core/src

# Touch to bust the cache on stubs
RUN touch mur-common/src/lib.rs mur-core/src/main.rs

RUN cargo build --release --bin mur

# ── Stage 2: Runtime ──────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/mur /usr/local/bin/mur

# Mur data directory
VOLUME ["/root/.mur"]

EXPOSE 3847

ENTRYPOINT ["mur"]
CMD ["serve", "--port", "3847"]
