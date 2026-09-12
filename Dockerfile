# syntax=docker/dockerfile:1.7

# ------------------------------------------------------------
# Build stage
# ------------------------------------------------------------
FROM rust:1.90-bookworm AS build
WORKDIR /src

# Cache dependencies first: copy manifests, create a stub main, build.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs && \
    cargo build --release --locked && \
    rm -rf src target/release/deps/archanist* target/release/archanist*

# Real sources.
COPY src ./src
RUN cargo build --release --locked

# ------------------------------------------------------------
# Runtime stage
# ------------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# Runtime deps:
#   - ca-certificates: reqwest / TLS-backed release sources
#   - tini: PID 1 with sensible signal handling for `docker stop`
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tini \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /src/target/release/archanist /usr/local/bin/archanist

# Data dir: config.toml, state.toml, components/, log dir. Bind-mount
# this from the host so state survives container replacement.
WORKDIR /app/data

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/archanist"]
CMD ["--help"]
