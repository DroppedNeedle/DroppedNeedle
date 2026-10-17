#!/bin/sh
# Container end-to-end test.
#
# 1. Smoke: fresh boot, healthy, clean SIGTERM shutdown. Prints cold and
#    warm boot time, idle RSS and shutdown time.
# 2. Journeys: starts the image on an internal network with no route out,
#    checks that nothing outside resolves or routes, then runs the browser
#    journeys in frontend/tests/journeys against it.
#
# Everything runs against containers this script starts under its own names,
# never against an address passed in. They are removed on exit, pass or fail,
# and use no volumes. The journeys need the frontend dependencies and
# Playwright's Chromium (make frontend-install frontend-browser-install).
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
IMAGE="droppedneedle-v3:e2e"
NAME="dn-e2e-smoke"
APP="dn-e2e-app"
NET="dn-e2e-sealed"

cleanup() {
  docker rm -f "$NAME" "$APP" >/dev/null 2>&1 || true
  docker network rm "$NET" >/dev/null 2>&1 || true
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

# Boot the container and wait for /health, printing "port boot_seconds".
boot_and_wait() {
  docker run -d --name "$NAME" -p 127.0.0.1::8688 -e RUST_LOG=info "$IMAGE" >/dev/null || fail "docker run failed"
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

echo "--- build ---"
docker build -f "$ROOT/Dockerfile" -t "$IMAGE" "$ROOT" || fail "docker build failed"

cleanup

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

echo "--- journeys ---"
# A published port does not listen on an internal network, so the browser
# on the host reaches the app by its address on the network's bridge.
docker network create --internal "$NET" >/dev/null || fail "network create failed"
docker run -d --name "$APP" --network "$NET" -e TZ=UTC -e RUST_LOG=info "$IMAGE" >/dev/null || fail "journey container did not start"
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

# The seal, checked from inside the app's container: a documentation address
# fails at once with no route (curl 7), not after a timeout (curl 28), and
# the documentation domain does not resolve (curl 6).
ROUTE_RC=$(curl_inside "http://192.0.2.1/")
[ "$ROUTE_RC" = "7" ] || fail "the journey network has a route out (curl exit $ROUTE_RC)"
NAME_RC=$(curl_inside "http://example.com/")
[ "$NAME_RC" = "6" ] || fail "outside names resolve on the journey network (curl exit $NAME_RC)"
echo "sealed: no outside address routes, no outside name resolves"

START=$(date +%s.%N)
if ! DN_E2E_BASE_URL="$BASE_URL" pnpm --dir "$ROOT/frontend" exec playwright test -c tests/journeys; then
  echo "--- app log (last 80 lines) ---"
  docker logs --tail 80 "$APP" 2>&1 || true
  fail "journeys failed; traces are in frontend/test-results/journeys"
fi
echo "journeys: $(elapsed "$START" "$(date +%s.%N)")s"

echo "E2E PASSED"
