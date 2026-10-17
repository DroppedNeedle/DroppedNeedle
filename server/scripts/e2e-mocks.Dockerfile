# The mock slskd and MusicBrainz (droppedneedle-mocks) as an image, for the
# browser journeys in e2e-container.sh. Built from the repository root:
#   docker build -f server/scripts/e2e-mocks.Dockerfile .

FROM rust:1.89-bookworm AS builder

WORKDIR /app
COPY rust-toolchain.toml ./
COPY server/ ./server/

RUN --mount=type=cache,id=dn-e2e-mocks-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=dn-e2e-mocks-target,target=/app/server/target \
    cargo build --locked --manifest-path server/Cargo.toml -p droppedneedle-testkit --bin droppedneedle-mocks \
    && cp server/target/debug/droppedneedle-mocks /usr/local/bin/droppedneedle-mocks

FROM debian:bookworm-slim

COPY --from=builder /usr/local/bin/droppedneedle-mocks /usr/local/bin/droppedneedle-mocks

USER nobody
ENTRYPOINT ["/usr/local/bin/droppedneedle-mocks"]
