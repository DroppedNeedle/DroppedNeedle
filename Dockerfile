# DroppedNeedle v3 image. Single-process by design: the
# binary is one tokio runtime and compose must never scale it past 1, because
# durable-operation ownership lives in-process.

# The Cargo profile the binaries are built with (server/Cargo.toml). Shipping
# images use release; CI's container boot test passes ci-e2e and `make image`
# passes local, both quicker builds of the same code.
ARG CARGO_PROFILE=release

# The Node base image names the exact version in .github/node-version, and
# the Rust build installs the one rust-toolchain.toml names; CI fails when
# they drift apart.
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
# The source maps name the packages in the build, for its licence notices; the
# script removes them once read.
RUN pnpm run build --sourcemap hidden \
    && node scripts/notices.mjs build /app/NOTICES-web-ui.txt

# The binaries are built on AlmaLinux 8, so they need glibc 2.28 at most and
# run on RHEL 8, Debian 10, Ubuntu 20.04 and later. The image, the release
# files and the packages all ship this one build; glibc-floor.sh holds it.
FROM almalinux:8.10@sha256:768bedc30fe0a536adb0a65d2eddb7caad364e514fbddd0ae9e026975f590342 AS chef

# Audio decode: opusic-sys (via symphonia-adapter-libopus) builds a
# bundled C library through the `cmake` crate, which shells out to the cmake
# binary. gcc-toolset-13 is the C compiler and linker.
RUN dnf install -y --setopt=install_weak_deps=False cmake gcc-toolset-13-gcc gcc-toolset-13-binutils make \
    && dnf clean all

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:/opt/rh/gcc-toolset-13/root/usr/bin:$PATH

# rustup, pinned by version and checksum, installs the Rust version
# rust-toolchain.toml names.
ARG RUSTUP_VERSION=1.29.3
RUN case "$(uname -m)" in \
        x86_64) sha256=b1c8cb14e30889e7e2999e6b69e62d9e9b9ffc0f4df8180564d306d9680e6c55 ;; \
        aarch64) sha256=9f608c142e0e38a78957d0661ba1dfaf0b6fb37184cfaa4b9a0aa57af6184282 ;; \
        *) echo "no pinned rustup for $(uname -m)" >&2; exit 1 ;; \
    esac \
    && curl -fsSL --retry 3 -o /tmp/rustup-init "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/$(uname -m)-unknown-linux-gnu/rustup-init" \
    && echo "${sha256}  /tmp/rustup-init" | sha256sum -c - \
    && chmod 0755 /tmp/rustup-init \
    && /tmp/rustup-init -y --no-modify-path --profile minimal --default-toolchain none \
    && rm /tmp/rustup-init

WORKDIR /app
COPY rust-toolchain.toml ./
RUN rustup toolchain install && cargo --version

RUN cargo install cargo-chef --version 0.1.78 --locked

# The same SQLite the tests run: see .cargo/config.toml.
ENV LIBSQLITE3_FLAGS=-DSQLITE_DEFAULT_MEMSTATUS=0

# The recipe is the manifests and the lock file with every source stripped,
# so the layer that compiles third-party crates is keyed on nothing else.
FROM chef AS planner
COPY server/ ./server/
RUN cd server && cargo chef prepare --recipe-path /app/recipe.json

# cargo-about writes the licence notices for the crates the binaries link
# (server/about.toml). It is built apart from the dependencies, so their
# cached layer stays valid.
FROM chef AS about
RUN cargo install cargo-about --version 0.9.3 --locked --features cli

FROM chef AS dependencies
ARG CARGO_PROFILE
COPY --from=planner /app/recipe.json ./recipe.json
RUN mkdir -p server \
    && cd server \
    && cargo chef cook --profile "$CARGO_PROFILE" --locked --recipe-path /app/recipe.json --bins -p droppedneedle -p droppedneedle-tool

# The licence notices are written beside the compile, from the crates the
# dependencies stage fetched.
FROM dependencies AS notices
COPY server/ ./server/
COPY --from=about /usr/local/cargo/bin/cargo-about /usr/local/cargo/bin/cargo-about
# The tool links the server, so its graph holds every crate of both binaries.
# A clarification in about.toml that cannot be fetched or checked only warns,
# and its crate falls back to a placeholder copyright line, so that warning
# fails the build.
RUN mkdir /notices \
    && cargo about generate --locked --fail --manifest-path server/tool/Cargo.toml --config server/about.toml \
        --output-file /notices/NOTICES-server.txt server/about.hbs 2>/notices/log \
    && ! grep -q clarification /notices/log \
    || { cat /notices/log >&2; exit 1; }

FROM dependencies AS builder
ARG CARGO_PROFILE
COPY server/ ./server/
# The Python plugin helper is compiled into the binary.
COPY sdk/ ./sdk/
# The custom CSS rules, which the server's check shares with the web UI's.
COPY frontend/src/lib/stores/customCssRules.json ./frontend/src/lib/stores/customCssRules.json

# Only the binaries stay in this layer, which keeps the build cache small. A
# profile's output folder is target/<profile name>.
RUN cargo build --profile "$CARGO_PROFILE" --locked --manifest-path server/Cargo.toml --bins -p droppedneedle -p droppedneedle-tool \
    && mkdir /out \
    && cp "server/target/$CARGO_PROFILE/droppedneedle" "server/target/$CARGO_PROFILE/droppedneedle-tool" /out/ \
    && rm -rf server/target \
    && server/scripts/glibc-floor.sh 2.28 /out/droppedneedle /out/droppedneedle-tool

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

# The files attached to each GitHub release, for running without Docker: the
# two binaries and the web UI, each with its licence and notices. The release
# workflow builds this stage alone; the image is the stage below.
FROM scratch AS release-files
COPY --from=builder /out/ /server/
COPY --from=notices /notices/NOTICES-server.txt /server/NOTICES-server.txt
COPY LICENSE /server/LICENSE
COPY --from=frontend /app/frontend/build /web-ui/static
COPY --from=frontend /app/NOTICES-web-ui.txt /web-ui/NOTICES-web-ui.txt
COPY LICENSE /web-ui/LICENSE

FROM debian:bookworm-slim

ENV PORT=8688

WORKDIR /app

# The two encoder libraries the slim ffmpeg links, tini as PID 1, curl for
# compose files that run it in their own health check, tzdata so TZ resolves.
# entrypoint.sh drops to PUID/PGID with setpriv, which the base image has.
# python3 runs Python plugins; build with --build-arg PLUGIN_PYTHON=0 for
# an image without it (plugins that ship their own program still run).
ARG PLUGIN_PYTHON=1
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl libmp3lame0 libopus0 tini tzdata \
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
# DroppedNeedle's licence, and the notices for the crates and the web UI
# packages it is built from.
COPY LICENSE /usr/share/doc/droppedneedle/LICENSE
COPY --from=notices /notices/NOTICES-server.txt /usr/share/doc/droppedneedle/NOTICES-server.txt
COPY --from=frontend /app/NOTICES-web-ui.txt /usr/share/doc/droppedneedle/NOTICES-web-ui.txt
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
      org.opencontainers.image.documentation="https://github.com/DroppedNeedle/DroppedNeedle/tree/main/docs" \
      org.opencontainers.image.vendor="DroppedNeedle" \
      org.opencontainers.image.version="${COMMIT_TAG}" \
      org.opencontainers.image.revision="${VCS_REF}" \
      org.opencontainers.image.created="${BUILD_DATE}" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later"

EXPOSE 8688

# The server checks itself, on the address and base path it serves. The start
# period covers a long upgrade, which runs after the port binds; the first
# success ends it, and checks every 5 seconds until then report a normal boot
# healthy quickly.
HEALTHCHECK --interval=30s --timeout=10s --start-period=600s --start-interval=5s --retries=3 \
    CMD ["droppedneedle", "--health-check"]

ENTRYPOINT ["tini", "--", "/app/entrypoint.sh"]
CMD ["droppedneedle"]
