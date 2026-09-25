# syntax=docker/dockerfile:1

# ---- Builder ----
FROM rust:1-bookworm AS builder
WORKDIR /app

# Native deps: reqwest's default-tls (native-tls) links against OpenSSL.
RUN apt-get update \
    && apt-get install -y --no-install-recommends libssl-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*

# Cache dependency compilation: build with a dummy main, then the real sources.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs \
    && cargo build --release \
    && rm -rf src

# Real build. `sqlx::migrate!` embeds ./migrations into the binary at compile time.
COPY . .
RUN touch src/main.rs && cargo build --release

# ---- Runtime ----
FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/backend /usr/local/bin/backend

ENV PORT=8080
EXPOSE 8080
CMD ["backend"]
