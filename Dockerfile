FROM rust:1.86-slim AS builder

# 1. Fix the OpenSSL / pkg-config build error
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# 2. Copy Cargo.toml first to cache dependencies
COPY Cargo.toml ./

# 3. Create a dummy main.rs to build dependencies
RUN mkdir src \
    && echo "fn main() {}" > src/main.rs \
    && cargo build --release \
    && rm -rf src

# 4. Copy the actual source code and build the real binary
COPY src ./src
RUN cargo build --release

# --- Runtime Stage ---
FROM debian:bookworm-slim

# 5. Install runtime SSL libraries and CA certificates for Upstash Redis TLS
RUN apt-get update && apt-get install -y \
    libssl3 \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/crypto-terminal-backend /app/crypto-terminal-backend

ENV PORT=10000
EXPOSE 10000

CMD ["/app/crypto-terminal-backend"]
