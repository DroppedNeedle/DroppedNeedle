# DroppedNeedle v3 image. Single-process by design: the
# binary is one tokio runtime and compose must never scale it past 1, because
# durable-operation ownership lives in-process.
FROM rust:1.89-bookworm AS builder

# Audio decode: opusic-sys (via symphonia-adapter-libopus) builds a
# bundled C library through the `cmake` crate, which shells out to the cmake
# binary. The rust base image ships gcc but not cmake.
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY rust-toolchain.toml ./
COPY server/ ./server/

RUN cargo build --release --manifest-path server/Cargo.toml

FROM debian:bookworm-slim

ARG COMMIT_TAG=dev
ARG BUILD_DATE=unknown

LABEL org.opencontainers.image.title="DroppedNeedle v3" \
      org.opencontainers.image.description="DroppedNeedle v3 backend (Rust auth service)" \
      org.opencontainers.image.url="https://github.com/DroppedNeedle/DroppedNeedle" \
      org.opencontainers.image.source="https://github.com/DroppedNeedle/DroppedNeedle" \
      org.opencontainers.image.version="${COMMIT_TAG}" \
      org.opencontainers.image.created="${BUILD_DATE}" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later"

ENV PORT=8688 \
    RUST_LOG=info

WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends curl ca-certificates tini \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd -r -g 1000 droppedneedle \
    && useradd -r -u 1000 -g droppedneedle -d /app -s /sbin/nologin droppedneedle

COPY --from=builder /app/server/target/release/droppedneedle /app/droppedneedle
RUN mkdir -p /app/cache /app/config \
    && chown -R droppedneedle:droppedneedle /app/droppedneedle /app/cache /app/config

USER droppedneedle

EXPOSE 8688

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:${PORT}/health || exit 1

ENTRYPOINT ["tini", "--", "/app/droppedneedle"]
