# Build stage — latest stable Rust toolchain (supports edition 2024)
FROM rust:slim AS builder

WORKDIR /app

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Copy manifests first (layer cache for dependencies)
COPY Cargo.toml ./
COPY Cargo.lock* ./

# Pre-fetch and compile dependencies (cached unless Cargo.toml changes)
RUN mkdir src && \
    echo 'fn main(){}' > src/main.rs && \
    echo 'fn main(){}' > src/storage_gc.rs
RUN cargo build --release --locked --bin realssa-engine --bin storage-gc
RUN rm -rf src

# Copy actual source code and build for real
COPY src ./src

# Capacity guard for the distributed Rust brain:
# - the old worker scanned the full RealSSA sitemap every 6 minutes, which can
#   contain thousands of URLs and caused unbounded Neon writes;
# - the actual news ingestion cron already owns freshness, so the brain only
#   needs the site's RSS feed and a slower 30-minute learning cadence.
RUN sed -i 's/Duration::from_secs(360)/Duration::from_secs(1800)/' src/human_brain.rs
RUN sed -i '/            let own_sources = \[/,/            \];/c\            let own_sources = [\n                "https://www.realssanews.com.ng/feed",\n                "https://realssanews.com.ng/feed",\n            ];' src/human_brain.rs

RUN touch src/main.rs
RUN cargo build --release --locked --bin realssa-engine --bin storage-gc

# ─── Runtime stage — minimal Debian with only what's needed ───────────────────
FROM debian:bookworm-slim

# SSL certificates for HTTPS fetching
RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/realssa-engine /usr/local/bin/realssa-engine
COPY --from=builder /app/target/release/storage-gc /usr/local/bin/storage-gc
COPY start.sh /usr/local/bin/start-realssa.sh
RUN chmod +x /usr/local/bin/start-realssa.sh

# Fly.io uses PORT env var
ENV PORT=8080
EXPOSE 8080

# Run as non-root for security
RUN useradd -r -s /bin/false engine
USER engine

CMD ["/usr/local/bin/start-realssa.sh"]
