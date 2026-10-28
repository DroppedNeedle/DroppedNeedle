FROM node:25-alpine AS frontend-build

WORKDIR /app/frontend

ENV PNPM_HOME="/pnpm"
ENV PATH="$PNPM_HOME:$PATH"

RUN npm install -g pnpm@10.33.0

COPY frontend/package.json ./
COPY frontend/pnpm-lock.yaml ./
COPY frontend/pnpm-workspace.yaml ./

RUN --mount=type=cache,id=pnpm,target=/pnpm/store pnpm install --frozen-lockfile

COPY frontend/ .

# Build SvelteKit with the literal base-path placeholder instead of an empty
# prefix so container startup can stamp in any BASE_PATH without rebuilding
# (see frontend/svelte.config.js and backend/maintenance/configure_frontend_base.py).
ENV DROPPEDNEEDLE_BASE_PATH_PLACEHOLDER=1
RUN pnpm run build

FROM python:3.13.5-slim AS python-deps

COPY backend/requirements.txt /tmp/requirements.txt
RUN pip install --no-cache-dir --prefix=/install -r /tmp/requirements.txt

FROM python:3.13.5-slim

ARG COMMIT_TAG
ARG BUILD_DATE
ARG DROPPEDNEEDLE_SOURCE_REVISION=unknown

LABEL org.opencontainers.image.title="DroppedNeedle" \
      org.opencontainers.image.description="Music request and discovery app with a built-in native library + download engine" \
      org.opencontainers.image.url="https://github.com/DroppedNeedle/DroppedNeedle" \
      org.opencontainers.image.source="https://github.com/DroppedNeedle/DroppedNeedle" \
      org.opencontainers.image.version="${COMMIT_TAG}" \
      org.opencontainers.image.created="${BUILD_DATE}" \
      org.opencontainers.image.licenses="AGPL-3.0" \
      org.droppedneedle.source-revision="${DROPPEDNEEDLE_SOURCE_REVISION}"

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    MALLOC_ARENA_MAX=2 \
    PORT=8688 \
    DROPPEDNEEDLE_STATIC_DIR=/app/cache/frontend-static \
    COMMIT_TAG=${COMMIT_TAG} \
    BUILD_DATE=${BUILD_DATE} \
    DROPPEDNEEDLE_SOURCE_REVISION=${DROPPEDNEEDLE_SOURCE_REVISION}

WORKDIR /app

# libchromaprint-tools provides fpcalc (Tier-3 fingerprinting). Its version is
# pinned reproducibly via the pinned python:3.13.5-slim (bookworm) base; apt
# version-pinning is avoided because Debian drops old versions from the mirror.
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl tini gosu libchromaprint-tools ffmpeg loudgain \
    && rm -rf /var/lib/apt/lists/*

COPY --from=python-deps /install /usr/local

# Bake the user at the entrypoint's default PUID/PGID (1000) so the common
# deployment needs no runtime usermod/groupmod remap (which can stall startup).
# The home stays outside /app: `usermod -u` chowns the whole home tree, which
# copies every application file up into the container's writable layer.
RUN groupadd -r -g 1000 droppedneedle \
    && useradd -r -u 1000 -g droppedneedle -d /nonexistent -s /sbin/nologin droppedneedle

COPY backend/ .
# Pristine, never-served input for entrypoint.sh, which writes the resolved
# frontend to DROPPEDNEEDLE_STATIC_DIR before uvicorn starts.
COPY --from=frontend-build /app/frontend/build ./static-template
COPY entrypoint.sh /entrypoint.sh

RUN find /app -type f -print0 \
      | sort -z \
      | xargs -0 sha256sum \
      | sha256sum \
      | cut -d' ' -f1 > /app/.droppedneedle-source-revision \
    && test -s /app/.droppedneedle-source-revision

RUN mkdir -p /app/cache /app/config /app/plugins /app/imports \
    && chown -R droppedneedle:droppedneedle /app \
    && chmod +x /entrypoint.sh

EXPOSE ${PORT}

# Shell form is required: ${PORT}/${BASE_PATH} must expand at probe time from
# container env. Empty BASE_PATH reproduces today's URL exactly; a nonempty
# value reaches the same prefixed surface reverse proxies forward to.
HEALTHCHECK --interval=30s --timeout=10s --start-period=10m --retries=3 \
    CMD curl -f http://localhost:${PORT}${BASE_PATH:-}/health || exit 1

ENTRYPOINT ["tini", "--", "/entrypoint.sh"]
CMD ["python", "-m", "maintenance.automatic_upgrade", "--start-target"]
