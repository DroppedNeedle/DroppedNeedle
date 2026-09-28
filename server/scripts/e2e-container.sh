#!/bin/sh
# Stage-1 E2E: fresh container boot -> healthy -> clean SIGTERM shutdown.
# Prints budget numbers (cold/warm boot, idle RSS, shutdown time) for the
# stage report. Safe to run beside prod: ephemeral host port, own image tag,
# own container name, no volumes.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
IMAGE="droppedneedle-v3:e2e"
NAME="droppedneedle-v3-e2e"

cleanup() {
  docker rm -f "$NAME" >/dev/null 2>&1 || true
}

fail() {
  echo "E2E FAILED: $1" >&2
  cleanup
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

echo "--- build ---"
docker build -f "$ROOT/Dockerfile.v3" -t "$IMAGE" "$ROOT" || fail "docker build failed"

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
cleanup

echo "E2E PASSED"
