# syntax=docker/dockerfile:1

# BUILD CONTEXT IS THE REPO ROOT, not backend/: the backend path-depends on
# ../core (spec savvagent/nels-oss#5, A2). Build with:
#   docker build -f backend/Dockerfile .
# Deploy with (from the repo root):
#   fly deploy . --config backend/fly.toml

# ---- Builder ----
FROM rust:1-bookworm AS builder
WORKDIR /app

# Native deps: reqwest's default-tls (native-tls) links against OpenSSL.
RUN apt-get update \
    && apt-get install -y --no-install-recommends libssl-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*

# Cache dependency compilation: build against stub sources first.
COPY core/Cargo.toml core/Cargo.lock core/
COPY backend/Cargo.toml backend/Cargo.lock backend/
RUN mkdir -p core/src backend/src \
    && touch core/src/lib.rs \
    && echo "fn main() {}" > backend/src/main.rs \
    && cd backend && cargo build --release \
    && rm -rf /app/core/src /app/backend/src

# Real build. `sqlx::migrate!` embeds backend/migrations at compile time.
COPY core core
COPY backend backend
RUN touch core/src/lib.rs backend/src/main.rs \
    && cd backend && cargo build --release

# ---- Runtime ----
FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/backend/target/release/backend /usr/local/bin/backend

ENV PORT=8080
EXPOSE 8080
CMD ["backend"]
