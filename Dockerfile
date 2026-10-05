# DroppedNeedle v3 image. Single-process by design: the
# binary is one tokio runtime and compose must never scale it past 1, because
# durable-operation ownership lives in-process.

FROM node:22-bookworm-slim AS frontend

WORKDIR /app/frontend

ENV PNPM_HOME="/pnpm"
ENV PATH="$PNPM_HOME:$PATH"

RUN npm install -g pnpm@10.33.0

COPY frontend/package.json frontend/pnpm-lock.yaml frontend/pnpm-workspace.yaml ./
RUN --mount=type=cache,id=pnpm,target=/pnpm/store pnpm install --frozen-lockfile

COPY frontend/ ./

# Bake the literal base-path placeholder into every root-relative URL so the
# server can stamp in any BASE_PATH at startup without a rebuild (see
# frontend/svelte.config.js and server/src/web.rs).
ENV DROPPEDNEEDLE_BASE_PATH_PLACEHOLDER=1
RUN pnpm run build

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

RUN cargo build --release --locked --manifest-path server/Cargo.toml --bins

FROM debian:bookworm-slim

ARG COMMIT_TAG=dev
ARG BUILD_DATE=unknown

LABEL org.opencontainers.image.title="DroppedNeedle v3" \
      org.opencontainers.image.description="DroppedNeedle v3 server: music requests, library management and streaming" \
      org.opencontainers.image.url="https://github.com/DroppedNeedle/DroppedNeedle" \
      org.opencontainers.image.source="https://github.com/DroppedNeedle/DroppedNeedle" \
      org.opencontainers.image.version="${COMMIT_TAG}" \
      org.opencontainers.image.created="${BUILD_DATE}" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later"

ENV PORT=8688

WORKDIR /app

# ffmpeg for transcoding, gosu for the PUID/PGID drop in entrypoint.sh,
# tini as PID 1, curl for the health check, tzdata so TZ resolves.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl ffmpeg gosu tini tzdata \
    && rm -rf /var/lib/apt/lists/*

# Bake the user at the entrypoint's default PUID/PGID (1000) so the common
# deployment needs no runtime usermod/groupmod remap.
RUN groupadd -r -g 1000 droppedneedle \
    && useradd -r -u 1000 -g droppedneedle -d /app -s /sbin/nologin droppedneedle

COPY --from=builder /app/server/target/release/droppedneedle /usr/local/bin/droppedneedle
COPY --from=builder /app/server/target/release/droppedneedle-tool /usr/local/bin/droppedneedle-tool
# The pristine web UI; the server stamps a copy into /app/cache/static at
# startup (DROPPEDNEEDLE_STATIC_DIR defaults to /app/static).
COPY --from=frontend /app/frontend/build /app/static
COPY entrypoint.sh /app/entrypoint.sh

RUN mkdir -p /app/cache /app/config /app/plugins /app/imports \
    && chown droppedneedle:droppedneedle /app/cache /app/config /app/plugins /app/imports \
    && chmod 0755 /app/entrypoint.sh

EXPOSE 8688

# Shell form so ${PORT} and ${BASE_PATH} expand from the container env.
HEALTHCHECK --interval=30s --timeout=10s --start-period=10m --retries=3 \
    CMD curl -fsS "http://localhost:${PORT}${BASE_PATH:-}/health" || exit 1

ENTRYPOINT ["tini", "--", "/app/entrypoint.sh"]
CMD ["droppedneedle"]
