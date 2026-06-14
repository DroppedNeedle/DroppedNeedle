##
# Stage 1 — Build frontend
##
FROM node:25-alpine AS frontend-build

WORKDIR /app/frontend

ENV PNPM_HOME="/pnpm"
ENV PATH="$PNPM_HOME:$PATH"

# Install pnpm
RUN npm install -g pnpm@10.33.0

COPY frontend/package.json ./
COPY frontend/pnpm-lock.yaml ./
COPY frontend/pnpm-workspace.yaml ./

RUN --mount=type=cache,id=pnpm,target=/pnpm/store pnpm install --frozen-lockfile

COPY frontend/ .
RUN pnpm run build

##
# Stage 2 — Install Python dependencies
##
FROM python:3.13.5-slim AS python-deps

COPY backend/requirements.txt /tmp/requirements.txt
RUN pip install --no-cache-dir --prefix=/install -r /tmp/requirements.txt

##
# Stage 3 — Final runtime image
##
FROM python:3.13.5-slim

ARG COMMIT_TAG
ARG BUILD_DATE

LABEL org.opencontainers.image.title="MusicSeerr" \
      org.opencontainers.image.description="Music request and discovery app for Lidarr" \
      org.opencontainers.image.url="https://github.com/habirabbu/musicseerr" \
      org.opencontainers.image.source="https://github.com/habirabbu/musicseerr" \
      org.opencontainers.image.version="${COMMIT_TAG}" \
      org.opencontainers.image.created="${BUILD_DATE}" \
      org.opencontainers.image.licenses="AGPL-3.0"

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    MALLOC_ARENA_MAX=2 \
    PORT=8688 \
    COMMIT_TAG=${COMMIT_TAG} \
    BUILD_DATE=${BUILD_DATE}

WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends curl tini gosu \
    && rm -rf /var/lib/apt/lists/*

COPY --from=python-deps /install /usr/local

RUN groupadd -r -g 911 musicseerr \
    && useradd -r -u 911 -g musicseerr -d /app -s /sbin/nologin musicseerr

COPY backend/ .
COPY --from=frontend-build /app/frontend/build ./static
COPY entrypoint.sh /entrypoint.sh

RUN mkdir -p /app/cache /app/config \
    && chown -R musicseerr:musicseerr /app \
    && chmod +x /entrypoint.sh

EXPOSE ${PORT}

HEALTHCHECK --interval=30s --timeout=10s --start-period=15s --retries=3 \
    CMD curl -f http://localhost:${PORT}/health || exit 1

ENTRYPOINT ["tini", "--", "/entrypoint.sh"]
CMD ["sh", "-c", "exec uvicorn main:app --host 0.0.0.0 --port ${PORT} --loop uvloop --http httptools --workers 1"]
