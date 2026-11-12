# DroppedNeedle v3 image. Single-process by design: the
# binary is one tokio runtime and compose must never scale it past 1, because
# durable-operation ownership lives in-process.

# Base images name the exact versions in .github/node-version and
# rust-toolchain.toml; CI fails when they drift apart.
FROM node:22.23.4-bookworm-slim AS frontend

WORKDIR /app/frontend

ENV PNPM_HOME="/pnpm"
ENV PATH="$PNPM_HOME:$PATH"
ENV COREPACK_ENABLE_DOWNLOAD_PROMPT=0

RUN corepack enable

# corepack runs the pnpm version named by packageManager in package.json.
COPY frontend/package.json frontend/pnpm-lock.yaml frontend/pnpm-workspace.yaml ./
RUN --mount=type=cache,id=pnpm,target=/pnpm/store pnpm install --frozen-lockfile

COPY frontend/ ./

# Bake the literal base-path placeholder into every root-relative URL so the
# server can stamp in any BASE_PATH at startup without a rebuild (see
# frontend/svelte.config.js and server/src/web.rs).
ENV DROPPEDNEEDLE_BASE_PATH_PLACEHOLDER=1
RUN pnpm run build

FROM rust:1.89.0-bookworm AS chef

# Audio decode: opusic-sys (via symphonia-adapter-libopus) builds a
# bundled C library through the `cmake` crate, which shells out to the cmake
# binary. The rust base image ships gcc but not cmake.
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake \
    && rm -rf /var/lib/apt/lists/*

RUN cargo install cargo-chef --version 0.1.78 --locked

WORKDIR /app
COPY rust-toolchain.toml ./
# The same SQLite the tests run: see .cargo/config.toml.
ENV LIBSQLITE3_FLAGS=-DSQLITE_DEFAULT_MEMSTATUS=0

# The recipe is the manifests and the lock file with every source stripped,
# so the layer that compiles third-party crates is keyed on nothing else.
FROM chef AS planner
COPY server/ ./server/
RUN cd server && cargo chef prepare --recipe-path /app/recipe.json

FROM chef AS dependencies
COPY --from=planner /app/recipe.json ./recipe.json
RUN mkdir -p server \
    && cd server \
    && cargo chef cook --release --locked --recipe-path /app/recipe.json --bins -p droppedneedle -p droppedneedle-tool

FROM dependencies AS builder
COPY server/ ./server/
# The Python plugin helper is compiled into the binary.
COPY sdk/ ./sdk/
# The custom CSS rules, which the server's check shares with the web UI's.
COPY frontend/src/lib/stores/customCssRules.json ./frontend/src/lib/stores/customCssRules.json

# Only the binaries stay in this layer, which keeps the build cache small.
RUN cargo build --release --locked --manifest-path server/Cargo.toml --bins -p droppedneedle -p droppedneedle-tool \
    && mkdir /out \
    && cp server/target/release/droppedneedle server/target/release/droppedneedle-tool /out/ \
    && rm -rf server/target

# A slim audio-only ffmpeg for transcoding, LGPL only: never --enable-gpl or
# --enable-nonfree. libmp3lame and libopus come from Debian, linked dynamically.
# The lists cover what the scan accepts and plugins may spool: a format the
# server should transcode is added here with a transcode in
# server/scripts/e2e-container.sh.
FROM debian:bookworm-slim AS ffmpeg

ARG FFMPEG_VERSION=9.0.2
ARG FFMPEG_SHA256=8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e

RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential ca-certificates curl libmp3lame-dev libopus-dev nasm pkg-config xz-utils \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# The image carries what the LGPL asks of whoever ships the program: the licence
# files, the exact source and, in BUILD, the version and configure line.
# libmp3lame's Debian source is in lame/, fetched by the step below.
RUN curl -fsSL --retry 3 -o "ffmpeg-${FFMPEG_VERSION}.tar.xz" "https://ffmpeg.org/releases/ffmpeg-${FFMPEG_VERSION}.tar.xz" \
    && echo "${FFMPEG_SHA256}  ffmpeg-${FFMPEG_VERSION}.tar.xz" | sha256sum -c - \
    && tar xf "ffmpeg-${FFMPEG_VERSION}.tar.xz" \
    && cd "ffmpeg-${FFMPEG_VERSION}" \
    && ./configure \
        --disable-debug \
        --disable-everything --disable-autodetect --disable-network \
        --disable-doc --disable-ffplay --disable-ffprobe --disable-avdevice --disable-swscale \
        --enable-libmp3lame --enable-libopus \
        --enable-protocol=file,pipe \
        --enable-demuxer=aac,flac,matroska,mov,mp3,ogg,wav \
        --enable-parser=aac,flac,mpegaudio,opus,vorbis \
        --enable-decoder=aac,adpcm_ima_wav,adpcm_ms,alac,flac,mp3,mp3float,opus,pcm_alaw,pcm_f32le,pcm_f64le,pcm_mulaw,pcm_s16le,pcm_s24le,pcm_s32le,pcm_u8,vorbis \
        --enable-encoder=aac,flac,libmp3lame,libopus \
        --enable-muxer=flac,mp3,mpegts,ogg \
        --enable-filter=aformat,anull,aresample,atrim \
    && make -j"$(nproc)" ffmpeg \
    && mkdir -p /out/doc \
    && cp ffmpeg /out/ffmpeg \
    && cp COPYING.* CREDITS LICENSE.md /out/doc/ \
    && ./ffmpeg -version > /out/doc/BUILD \
    && cp "../ffmpeg-${FFMPEG_VERSION}.tar.xz" /out/doc/

# libmp3lame is LGPL too, so its Debian source goes beside FFmpeg's: the
# source package and version dpkg records for the -dev package the build links.
# The apt sandbox user cannot write to /out, so apt would run this download as
# root with a warning; the option drops the warning.
RUN sed -i 's/^Types: deb$/Types: deb deb-src/' /etc/apt/sources.list.d/debian.sources \
    && apt-get update \
    && src=$(dpkg-query -W -f='${source:Package}=${source:Version}' libmp3lame-dev) \
    && ver=$(dpkg-query -W -f='${Version}' libmp3lame-dev) \
    && mkdir -p /out/doc/lame \
    && cd /out/doc/lame \
    && apt-get -o APT::Sandbox::User=root source --download-only "$src" \
    && echo "$ver" > VERSION \
    && rm -rf /var/lib/apt/lists/*

FROM debian:bookworm-slim

ENV PORT=8688

WORKDIR /app

# The two encoder libraries the slim ffmpeg links, gosu for the PUID/PGID drop
# in entrypoint.sh, tini as PID 1, curl for the health check, tzdata so TZ
# resolves.
# python3 runs Python plugins; build with --build-arg PLUGIN_PYTHON=0 for
# an image without it (plugins that ship their own program still run).
ARG PLUGIN_PYTHON=1
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl gosu libmp3lame0 libopus0 tini tzdata \
    && if [ "$PLUGIN_PYTHON" = "1" ]; then apt-get install -y --no-install-recommends python3; fi \
    && rm -rf /var/lib/apt/lists/*

# Bake the user at the entrypoint's default PUID/PGID (1000) so the common
# deployment needs no runtime usermod/groupmod remap. The home is /nonexistent,
# not /app: a remap then edits only the account files, and entrypoint.sh hands
# the data folders over itself.
RUN groupadd -r -g 1000 droppedneedle \
    && useradd -r -u 1000 -g droppedneedle -d /nonexistent -s /sbin/nologin droppedneedle

COPY --from=builder /out/droppedneedle /usr/local/bin/droppedneedle
COPY --from=builder /out/droppedneedle-tool /usr/local/bin/droppedneedle-tool
COPY --from=ffmpeg /out/ffmpeg /usr/local/bin/ffmpeg
COPY --from=ffmpeg /out/doc /usr/share/doc/ffmpeg
# Fails the build when the runtime libmp3lame0 is not the version of the source
# beside it, so a Debian update between the two stages cannot ship a binary
# without its source.
RUN lame=$(dpkg-query -W -f='${Version}' libmp3lame0) \
    && [ -n "$lame" ] \
    && [ "$lame" = "$(cat /usr/share/doc/ffmpeg/lame/VERSION)" ] \
    || { echo "libmp3lame0 $lame does not match the source in /usr/share/doc/ffmpeg/lame; build again without the cache" >&2; exit 1; }
# The pristine web UI; the server stamps a copy into /app/cache/static at
# startup (DROPPEDNEEDLE_STATIC_DIR defaults to /app/static).
COPY --from=frontend /app/frontend/build /app/static
COPY entrypoint.sh /app/entrypoint.sh

RUN mkdir -p /app/cache /app/config /app/plugins /app/imports \
    && chown droppedneedle:droppedneedle /app/cache /app/config /app/plugins /app/imports \
    && chmod 0755 /app/entrypoint.sh

# The server reads these two at start and the About page shows them. They come
# last so a new tag or date reuses every layer above. VCS_REF is the commit,
# for the revision label only.
ARG COMMIT_TAG=dev
ARG BUILD_DATE=
ARG VCS_REF=
ENV COMMIT_TAG=${COMMIT_TAG} BUILD_DATE=${BUILD_DATE}

LABEL org.opencontainers.image.title="DroppedNeedle" \
      org.opencontainers.image.description="Self-hosted music requests, discovery and library management, with Subsonic and Jellyfin APIs for music apps" \
      org.opencontainers.image.url="https://github.com/DroppedNeedle/DroppedNeedle" \
      org.opencontainers.image.source="https://github.com/DroppedNeedle/DroppedNeedle" \
      org.opencontainers.image.documentation="https://github.com/DroppedNeedle/DroppedNeedle/tree/v3/docs" \
      org.opencontainers.image.vendor="DroppedNeedle" \
      org.opencontainers.image.version="${COMMIT_TAG}" \
      org.opencontainers.image.revision="${VCS_REF}" \
      org.opencontainers.image.created="${BUILD_DATE}" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later"

EXPOSE 8688

# Shell form so ${PORT} and ${BASE_PATH} expand from the container env.
HEALTHCHECK --interval=30s --timeout=10s --start-period=60s --retries=3 \
    CMD curl -fsS "http://localhost:${PORT}${BASE_PATH:-}/health" || exit 1

ENTRYPOINT ["tini", "--", "/app/entrypoint.sh"]
CMD ["droppedneedle"]
