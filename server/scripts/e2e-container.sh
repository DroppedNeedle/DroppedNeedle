#!/bin/sh
# Container end-to-end test.
#
# 1. ffmpeg: the image's build is LGPL only with its notices beside it, and
#    every format the scan accepts transcodes to every output, with and without
#    a seek, using the argument shape of build_cmd in transcode.rs.
# 2. Smoke: fresh boot, healthy, clean SIGTERM shutdown. Prints cold and
#    warm boot time, idle RSS and shutdown time.
# 3. PUID: a boot with PUID=568 hands the config and cache folders to uid 568
#    and the server writes as it; a boot with PUID back at 1000 hands them back.
# 4. Journeys: on an internal network with no route out, starts the mock
#    slskd and MusicBrainz (e2e-mocks.Dockerfile), then runs each browser
#    journey in frontend/tests/journeys against a fresh server of its own,
#    after checking that nothing outside resolves or routes from it.
#
# Everything runs against containers this script starts under its own names,
# never against an address passed in. They are removed on exit, pass or fail,
# as are the two volumes the PUID check makes. The folders mounted are the audio
# fixtures, read-only, for the ffmpeg check and, as the library at /music, a
# throwaway copy of two of them, removed on exit. The journeys need the
# frontend dependencies and Playwright's Chromium
# (make frontend-install frontend-browser-install).
#
# DN_E2E_IMAGE and DN_E2E_MOCKS_IMAGE name images already built; each one set
# skips that build.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
IMAGE="${DN_E2E_IMAGE:-droppedneedle-v3:e2e}"
MOCKS_IMAGE="${DN_E2E_MOCKS_IMAGE:-droppedneedle-v3:e2e-mocks}"
NAME="dn-e2e-smoke"
APP="dn-e2e-app"
MOCKS="dn-e2e-mocks"
NET="dn-e2e-sealed"
CONFIG_VOLUME="dn-e2e-config"
CACHE_VOLUME="dn-e2e-cache"
FFMPEG="dn-e2e-ffmpeg"
FIXTURES="$ROOT/server/tests/fixtures/library"
WORK=""
OUT=""

cleanup() {
  docker rm -f "$NAME" "$APP" "$MOCKS" "$FFMPEG" >/dev/null 2>&1 || true
  docker network rm "$NET" >/dev/null 2>&1 || true
  docker volume rm "$CONFIG_VOLUME" "$CACHE_VOLUME" >/dev/null 2>&1 || true
  if [ -n "$WORK" ]; then rm -rf "$WORK" || true; fi
  if [ -n "$OUT" ]; then rm -rf "$OUT" || true; fi
}
trap cleanup EXIT
trap 'exit 130' INT TERM

fail() {
  echo "E2E FAILED: $1" >&2
  exit 1
}

elapsed() {
  awk "BEGIN { printf \"%.2f\", $2 - $1 }"
}

# Boot the container and wait for /health, printing "port boot_seconds". Any
# arguments go to docker run ahead of the image.
boot_and_wait() {
  docker run -d --name "$NAME" -p 127.0.0.1::8688 -e RUST_LOG=info "$@" "$IMAGE" >/dev/null || fail "docker run failed"
  START=$(date +%s.%N)
  PORT=""
  for _ in $(seq 1 60); do
    PORT=$(docker port "$NAME" 8688 2>/dev/null | head -1 | sed 's/.*://') || true
    if [ -n "$PORT" ] && curl -sf "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; then
      break
    fi
    PORT=""
    sleep 0.5
  done
  [ -n "$PORT" ] || fail "never became healthy"
  echo "$PORT $(elapsed "$START" "$(date +%s.%N)")"
}

# Run curl inside the journey container and print its exit code.
curl_inside() {
  docker exec "$APP" curl -s -o /dev/null -m 3 "$1" && echo 0 || echo $?
}

# Print some bytes of a file from an offset, as lowercase hex.
hex_at() {
  od -An -tx1 -j "$2" -N "$3" "$1" | tr -d ' \n'
}

# Fail unless a file is what its muxer writes and holds audio, not only headers:
# an MP3 frame sync (after the ID3 tag, when there is one); an Ogg page opening an
# Opus stream and a page after the two header pages; MPEG-TS packets that all
# start with the sync byte, one of them on ffmpeg's first audio PID (0x100).
check_output() {
  case "$1" in
    mp3)
      AT=0
      if [ "$(hex_at "$2" 0 3)" = "494433" ]; then
        # shellcheck disable=SC2046
        set -- "$1" "$2" "$3" $(od -An -tu1 -j 6 -N 4 "$2")
        AT=$((10 + ($4 << 21) + ($5 << 14) + ($6 << 7) + $7))
      fi
      case "$(hex_at "$2" "$AT" 2)" in
        ff[ef]?) ;;
        *) fail "$3: no MPEG frame sync at byte $AT" ;;
      esac
      ;;
    ogg)
      [ "$(hex_at "$2" 0 4)" = "4f676753" ] || fail "$3: no OggS"
      [ "$(hex_at "$2" 28 8)" = "4f70757348656164" ] || fail "$3: no OpusHead"
      PAGES=$(LC_ALL=C grep -ao OggS "$2" | wc -l)
      [ "$PAGES" -ge 3 ] || fail "$3: only $PAGES Ogg pages"
      ;;
    flac)
      [ "$(hex_at "$2" 0 4)" = "664c6143" ] || fail "$3: no fLaC"
      od -An -tx1 -v "$2" | tr -d ' \n' | grep -Eq 'fff[89]' || fail "$3: no FLAC frame sync"
      ;;
    mpegts)
      SIZE=$(wc -c <"$2")
      [ "$SIZE" -gt 0 ] && [ $((SIZE % 188)) -eq 0 ] || fail "$3: $SIZE bytes is not whole MPEG-TS packets"
      AT=0
      AUDIO=""
      while [ "$AT" -lt "$SIZE" ]; do
        [ "$(hex_at "$2" "$AT" 1)" = "47" ] || fail "$3: no MPEG-TS sync byte at $AT"
        case "$(hex_at "$2" $((AT + 1)) 2)" in
          ?100) AUDIO=1 ;;
        esac
        AT=$((AT + 188))
      done
      [ -n "$AUDIO" ] || fail "$3: no MPEG-TS audio packet"
      ;;
  esac
}

# One transcode with the argument shape of build_cmd: input, encoder, bitrate,
# muxer, then optionally -ss and its value. Anything ffmpeg logs at this level
# fails it too, since a stream it could not decode can still exit 0. An empty
# bitrate sends none, as build_cmd does for FLAC.
transcode() {
  INPUT=$1 ENCODER=$2 RATE=$3 MUXER=$4
  shift 4
  LABEL="$INPUT to $MUXER${1:+ with $*}"
  docker exec "$FFMPEG" ffmpeg -hide_banner -loglevel error -nostdin "$@" -i "/fixtures/$INPUT" \
    -vn -map 0:a:0 -c:a "$ENCODER" ${RATE:+-b:a "$RATE"} -f "$MUXER" pipe:1 >"$OUT/out" 2>"$OUT/err" \
    || fail "$LABEL: ffmpeg failed: $(cat "$OUT/err")"
  [ ! -s "$OUT/err" ] || fail "$LABEL: ffmpeg logged: $(cat "$OUT/err")"
  check_output "$MUXER" "$OUT/out" "$LABEL"
}

check_ffmpeg() {
  docker run -d --name "$FFMPEG" --network none --user 1000:1000 --entrypoint sleep \
    -v "$FIXTURES:/fixtures:ro" "$IMAGE" 600 >/dev/null || fail "ffmpeg container did not start"
  OUT=$(mktemp -d)

  FOUND=$(docker exec "$FFMPEG" sh -c 'command -v ffmpeg') || fail "no ffmpeg on PATH"
  [ "$FOUND" = "/usr/local/bin/ffmpeg" ] || fail "ffmpeg on PATH is $FOUND"

  # The licence: the configure line and what the program itself says.
  CONFIGURATION=$(docker exec "$FFMPEG" ffmpeg -version | grep '^configuration:') \
    || fail "ffmpeg -version has no configuration line"
  echo "$CONFIGURATION"
  case "$CONFIGURATION" in
    *--enable-gpl* | *--enable-nonfree*) fail "ffmpeg is not LGPL only" ;;
  esac
  docker exec "$FFMPEG" ffmpeg -hide_banner -L | grep -q 'GNU Lesser General Public' \
    || fail "ffmpeg -L does not name the LGPL"

  # The notices that ship with it.
  for FILE in COPYING.LGPLv2.1 CREDITS LICENSE.md; do
    docker exec "$FFMPEG" test -s "/usr/share/doc/ffmpeg/$FILE" || fail "no $FILE beside ffmpeg"
  done
  docker exec "$FFMPEG" grep -qF -- "$CONFIGURATION" /usr/share/doc/ffmpeg/BUILD \
    || fail "BUILD does not hold the configure line ffmpeg reports"
  VERSION=$(docker exec "$FFMPEG" ffmpeg -version | sed -n 's/^ffmpeg version \([^ ]*\).*/\1/p')
  docker exec "$FFMPEG" test -s "/usr/share/doc/ffmpeg/ffmpeg-$VERSION.tar.xz" \
    || fail "no source tarball for ffmpeg $VERSION"

  COUNT=0
  for INPUT in \
    management_full.flac \
    management_two_icons.flac \
    management_full.mp3 \
    management_full.ogg \
    management_full.opus \
    management_full.m4a \
    alac_short.m4a \
    management_full.aac \
    management_full.wav; do
    for SEEK in "" "-ss 0.200"; do
      # shellcheck disable=SC2086
      transcode "$INPUT" libmp3lame 192k mp3 $SEEK
      # shellcheck disable=SC2086
      transcode "$INPUT" libopus 128k ogg $SEEK
      # shellcheck disable=SC2086
      transcode "$INPUT" aac 256k mpegts $SEEK
      # shellcheck disable=SC2086
      transcode "$INPUT" flac "" flac $SEEK
      COUNT=$((COUNT + 4))
    done
  done
  echo "ffmpeg: $COUNT transcodes passed"
  docker rm -f "$FFMPEG" >/dev/null 2>&1 || true
}

echo "--- build ---"
if [ -z "${DN_E2E_IMAGE:-}" ]; then
  docker build -f "$ROOT/Dockerfile" -t "$IMAGE" "$ROOT" || fail "docker build failed"
fi
if [ -z "${DN_E2E_MOCKS_IMAGE:-}" ]; then
  docker build -f "$ROOT/server/scripts/e2e-mocks.Dockerfile" -t "$MOCKS_IMAGE" "$ROOT" || fail "mocks build failed"
fi

cleanup

echo "image size: $(docker image inspect --format '{{.Size}}' "$IMAGE") bytes"

echo "--- ffmpeg ---"
check_ffmpeg

echo "--- cold boot ---"
read -r PORT COLD_BOOT <<EOF
$(boot_and_wait)
EOF
BODY=$(curl -s "http://127.0.0.1:$PORT/health")
echo "health body: $BODY"
echo "$BODY" | grep -q '"status":"ok"' || fail "unexpected health body: $BODY"
RID=$(curl -sI "http://127.0.0.1:$PORT/health" | grep -i '^x-request-id:' | tr -d '\r' | awk '{print $2}')
[ -n "$RID" ] || fail "missing x-request-id header"
echo "request id header: $RID"
RSS=$(docker stats --no-stream --format '{{.MemUsage}}' "$NAME")
echo "cold boot to healthy: ${COLD_BOOT}s"
echo "idle RSS: $RSS"

echo "--- warm boot ---"
docker stop -t 10 "$NAME" >/dev/null || fail "warm-boot stop failed"
docker rm -f "$NAME" >/dev/null || fail "warm-boot rm failed"
read -r PORT WARM_BOOT <<EOF
$(boot_and_wait)
EOF
echo "warm boot to healthy: ${WARM_BOOT}s"

echo "--- SIGTERM shutdown ---"
START=$(date +%s.%N)
docker stop -t 10 "$NAME" >/dev/null || fail "SIGTERM stop failed"
echo "SIGTERM shutdown: $(elapsed "$START" "$(date +%s.%N)")s"
EXIT_CODE=$(docker inspect "$NAME" --format '{{.State.ExitCode}}')
[ "$EXIT_CODE" = "0" ] || fail "exit code $EXIT_CODE, expected 0"
docker logs "$NAME" 2>&1 | grep -q "shutdown complete" || fail "missing 'shutdown complete' log line"
echo "exit code 0, graceful-shutdown log line present"
docker rm -f "$NAME" >/dev/null 2>&1 || true

# Fail unless the database the server wrote, and everything else in the config
# and cache volumes, belongs to the given uid.
data_owned_by() {
  [ "$(docker exec "$NAME" stat -c %u /app/cache/library.db)" = "$1" ] \
    || fail "PUID=$1: library.db is not owned by uid $1"
  OTHERS=$(docker exec "$NAME" find /app/config /app/cache ! -user "$1")
  [ -z "$OTHERS" ] || fail "PUID=$1: not handed over: $OTHERS"
}

echo "--- PUID ---"
boot_and_wait -e PUID=568 -e PGID=568 \
  -v "$CONFIG_VOLUME:/app/config" -v "$CACHE_VOLUME:/app/cache" >/dev/null
data_owned_by 568
docker stop -t 10 "$NAME" >/dev/null || fail "PUID=568 stop failed"
docker rm -f "$NAME" >/dev/null || fail "PUID=568 rm failed"
boot_and_wait -v "$CONFIG_VOLUME:/app/config" -v "$CACHE_VOLUME:/app/cache" >/dev/null
data_owned_by 1000
echo "PUID=568 took the data folders over, PUID=1000 took them back"
docker rm -f "$NAME" >/dev/null 2>&1 || true
docker volume rm "$CONFIG_VOLUME" "$CACHE_VOLUME" >/dev/null 2>&1 || true

echo "--- journeys ---"
docker network create --internal "$NET" >/dev/null || fail "network create failed"
docker run -d --name "$MOCKS" --network "$NET" "$MOCKS_IMAGE" \
  --slskd 0.0.0.0:5030 --musicbrainz 0.0.0.0:5000 >/dev/null || fail "mocks did not start"
MOCKS_IP=$(docker inspect "$MOCKS" --format '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}')
[ -n "$MOCKS_IP" ] || fail "mocks container has no address"
MOCKS_UP=""
for _ in $(seq 1 60); do
  if curl -sf "http://$MOCKS_IP:5000/ws/2/artist?query=test" >/dev/null 2>&1 \
    && curl -sf -H "X-API-Key: e2e" "http://$MOCKS_IP:5030/api/v0/application" >/dev/null 2>&1; then
    MOCKS_UP=1
    break
  fi
  sleep 0.5
done
[ -n "$MOCKS_UP" ] || fail "mocks never answered at $MOCKS_IP"

WORK=$(mktemp -d)
ALBUM="$WORK/music/Radiohead/OK Computer"
mkdir -p "$ALBUM"
cp "$FIXTURES/flac_full_01.flac" "$ALBUM/01 Airbag.flac"
cp "$FIXTURES/flac_full_02.flac" "$ALBUM/02 Paranoid Android.flac"
# Writable by the server's user, so a write would show in the check below.
chmod -R a+rwX "$WORK/music"

# Start a fresh server on the sealed network, wait for it, and check the seal
# from inside it: a documentation address fails at once with no route (curl
# 7), not after a timeout (curl 28), and the documentation domain does not
# resolve (curl 6). A published port does not listen on an internal network,
# so the browser on the host reaches the app by its address on the bridge.
start_app() {
  docker rm -f "$APP" >/dev/null 2>&1 || true
  docker run -d --name "$APP" --network "$NET" -e TZ=UTC -e RUST_LOG=info \
    -v "$WORK/music:/music" "$IMAGE" >/dev/null || fail "journey container did not start"
  APP_IP=$(docker inspect "$APP" --format '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}')
  [ -n "$APP_IP" ] || fail "journey container has no address"
  BASE_URL="http://$APP_IP:8688"
  HEALTHY=""
  for _ in $(seq 1 60); do
    if curl -sf "$BASE_URL/health" >/dev/null 2>&1; then
      HEALTHY=1
      break
    fi
    sleep 0.5
  done
  [ -n "$HEALTHY" ] || fail "journey container never became healthy at $BASE_URL"
  ROUTE_RC=$(curl_inside "http://192.0.2.1/")
  [ "$ROUTE_RC" = "7" ] || fail "the journey network has a route out (curl exit $ROUTE_RC)"
  NAME_RC=$(curl_inside "http://example.com/")
  [ "$NAME_RC" = "6" ] || fail "outside names resolve on the journey network (curl exit $NAME_RC)"
}

# Each journey gets a server of its own, so none depends on another.
ALL_START=$(date +%s.%N)
for SPEC in "$ROOT"/frontend/tests/journeys/*.spec.ts; do
  JOURNEY=$(basename "$SPEC")
  start_app
  echo "$JOURNEY: sealed server at $BASE_URL"
  START=$(date +%s.%N)
  if ! DN_E2E_BASE_URL="$BASE_URL" \
    DN_E2E_MOCK_MUSICBRAINZ="http://$MOCKS:5000/ws/2" \
    DN_E2E_MOCK_SLSKD="http://$MOCKS:5030" \
    pnpm --dir "$ROOT/frontend" exec playwright test -c tests/journeys "$JOURNEY"; then
    echo "--- app log (last 80 lines) ---"
    docker logs --tail 80 "$APP" 2>&1 || true
    echo "--- mocks log ---"
    docker logs "$MOCKS" 2>&1 || true
    fail "$JOURNEY failed; its trace is in frontend/test-results/journeys"
  fi
  echo "$JOURNEY: $(elapsed "$START" "$(date +%s.%N)")s"
done
echo "journeys: $(elapsed "$ALL_START" "$(date +%s.%N)")s"

# Scanning writes no music file.
if ! cmp -s "$FIXTURES/flac_full_01.flac" "$ALBUM/01 Airbag.flac" \
  || ! cmp -s "$FIXTURES/flac_full_02.flac" "$ALBUM/02 Paranoid Android.flac"; then
  fail "a file in the library folder changed"
fi
ENTRIES=$(find "$WORK/music" -mindepth 1 | wc -l)
[ "$ENTRIES" -eq 4 ] || fail "the library folder holds $ENTRIES entries, not the 4 it started with"
echo "library folder unchanged"

echo "E2E PASSED"
