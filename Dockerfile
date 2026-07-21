FROM rust:1.86-slim AS builder

WORKDIR /app

# Copy Cargo files.
# If you commit Cargo.lock, it will be used automatically.
COPY Cargo.* ./

# Build dependencies first for better Docker caching.
RUN mkdir src \
    && echo "fn main() {}" > src/main.rs \
    && cargo build --release \
    && rm -rf src

COPY src ./src

RUN touch src/main.rs && cargo build --release

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/omni-stream-backend /usr/local/bin/omni-stream-backend

EXPOSE 3000

CMD ["omni-stream-backend"]
