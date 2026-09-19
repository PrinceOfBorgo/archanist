# syntax=docker/dockerfile:1.7

# ------------------------------------------------------------
# Runtime image
# ------------------------------------------------------------
FROM alpine:latest AS runtime
WORKDIR /app

# Runtime deps:
#   - ca-certificates: reqwest / rustls TLS root store for release sources
#   - tini: PID 1 with sensible signal handling for `docker stop`
RUN apk add --no-cache ca-certificates tini

ARG TARGETPLATFORM
ARG BIN_PATH_AMD64
ARG BIN_PATH_ARM64
ARG BIN_PATH_ARMV7

# Copy the pre-built binaries for every supported platform, then keep only
# the one matching the platform currently being built.
COPY ${BIN_PATH_AMD64} /app/archanist-amd64
COPY ${BIN_PATH_ARM64} /app/archanist-arm64
COPY ${BIN_PATH_ARMV7} /app/archanist-armv7

RUN case "${TARGETPLATFORM}" in \
        "linux/amd64")  mv /app/archanist-amd64 /usr/local/bin/archanist ;; \
        "linux/arm64")  mv /app/archanist-arm64 /usr/local/bin/archanist ;; \
        "linux/arm/v7") mv /app/archanist-armv7 /usr/local/bin/archanist ;; \
        *) echo "unsupported TARGETPLATFORM: ${TARGETPLATFORM}" >&2; exit 1 ;; \
    esac && \
    chmod +x /usr/local/bin/archanist && \
    rm -f /app/archanist-*

# Data dir: config.toml, state.toml, components/, log dir. Bind-mount
# this from the host so state survives container replacement.
WORKDIR /app/data

ENTRYPOINT ["/sbin/tini", "--", "/usr/local/bin/archanist"]
CMD ["--help"]
