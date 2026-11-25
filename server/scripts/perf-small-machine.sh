#!/usr/bin/env bash
# Small-machine run: how a published image copes with 2 CPUs and 1 GB on a
# 100,000-track library.
#
# 1. Starts the mock MusicBrainz and slskd (e2e-mocks.Dockerfile) on an
#    internal network with no route out, and the server beside it with
#    --cpus=2 --memory=1g --memory-swap=1g, its library mounted read-only.
#    The seal is checked from inside the server, as e2e-container.sh does.
# 2. Generates the library with gen-library.py, sets up the first admin,
#    adds the library folder, points MusicBrainz at the mock (as the browser
#    journeys do, unpaced, so identification finishes instead of waiting on
#    a host it cannot reach) and follows the first scan that starts.
# 3. Restarts the server over the full database, waits until no work is
#    pending, watches it idle, then reads one large Jellyfin list.
#
# Records: time to a healthy /health (fresh and after the restart), first
# scan files per second, the server's VmHWM and the container's cgroup
# memory.peak (read on the host), out-of-memory kills, writes and CPU while
# idle, and the size, time and memory peak of the Jellyfin list. Writes
# small-machine.json, summary.md and container.log to the output folder, and
# the table to the job summary when GITHUB_STEP_SUMMARY is set.
#
# Fails only on an out-of-memory kill or a server that stops by itself, a
# server not healthy within ten minutes, or a scan that does not finish. The
# Jellyfin list is only reported, a kill during it included. Every other
# figure is reported; the budgets below are where limits go once the first
# runs have set them.
#
# Linux only: it reads the container's cgroup and /proc on the host, through
# sudo where a file needs it. Everything it starts is removed on exit.
#
# DN_PERF_IMAGE names the server image (default the published :dev).
# DN_PERF_MOCKS_IMAGE names a built mocks image; unset builds it.
# DN_PERF_TRACKS (100000), DN_PERF_IDLE_SECONDS (600) and DN_PERF_OUT (the
# output folder, default ./small-machine) change the run.
set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
IMAGE="${DN_PERF_IMAGE:-ghcr.io/droppedneedle/droppedneedle:dev}"
MOCKS_IMAGE="${DN_PERF_MOCKS_IMAGE:-droppedneedle-v3:e2e-mocks}"
TRACKS="${DN_PERF_TRACKS:-100000}"
IDLE_SECONDS="${DN_PERF_IDLE_SECONDS:-600}"
OUT="${DN_PERF_OUT:-$PWD/small-machine}"
HEALTH_SECONDS=600
SCAN_SECONDS=1200
SETTLE_SECONDS=480
APP="dn-perf-app"
MOCKS="dn-perf-mocks"
NET="dn-perf-sealed"
CONFIG_VOLUME="dn-perf-config"
CACHE_VOLUME="dn-perf-cache"
USERNAME="perf-admin"
PASSWORD="small machine perf password"
CLIENT='Client="DroppedNeedle perf", Device="ci", DeviceId="dn-perf", Version="1.0.0"'

# --- Budgets ----------------------------------------------------------------
# None is set yet: the first four weekly runs set them. One "figure maximum"
# pair a line, the figure named as in small-machine.json; a figure over its
# maximum fails the run. For example:
#   cgroup_memory_peak_scan_bytes 943718400
# The idle figures measure an idle server only when after_restart_settled is
# true: after a settle that ran out of time they measure a busy one, so read
# them, and set their budgets, from settled runs only.
BUDGETS=""
# ----------------------------------------------------------------------------

WORK=""
TOKEN=""
BASE_URL=""
RESULT="failed"
REASON="the run stopped early; see the log"
mkdir -p "$OUT"
FIGURES="$OUT/figures.tsv"
: >"$FIGURES"

record() {
  printf '%s\t%s\n' "$1" "$2" >>"$FIGURES"
  echo "$1: $2"
}

fail() {
  REASON="$1"
  echo "SMALL MACHINE FAILED: $1" >&2
  exit 1
}

now() {
  date +%s.%N
}

elapsed() {
  awk "BEGIN { printf \"%.2f\", $2 - $1 }"
}

# Whole seconds since a start time.
seconds_since() {
  elapsed "$1" "$(now)" | cut -d. -f1
}

# Print b - a, or nothing when either is unknown.
delta() {
  if [ -n "$1" ] && [ -n "$2" ]; then echo $(($2 - $1)); fi
}

# Print a count of KiB as bytes, or nothing when it is unknown.
kib() {
  if [ -n "$1" ]; then echo $(($1 * 1024)); fi
}

# Read a value out of a JSON file by a dotted path ("run.counters.new_count").
# Prints nothing for a missing value, null or a file that is not JSON.
jget() {
  python3 -I -c '
import json, sys
try:
    value = json.load(open(sys.argv[1]))
except ValueError:
    sys.exit(0)
for part in sys.argv[2].split("."):
    value = value.get(part) if isinstance(value, dict) else None
if isinstance(value, bool):
    print("true" if value else "false")
elif isinstance(value, (dict, list)):
    print(json.dumps(value))
elif value is not None:
    print(value)
' "$1" "$2"
}

# Read a file on the host, through sudo when it is not readable as is.
# Prints nothing when it cannot be read, so the figure it feeds is left empty.
host_read() {
  cat "$1" 2>/dev/null || sudo -n cat "$1" 2>/dev/null || true
}

write_report() {
  python3 -I -c '
import json, sys
figures, result, reason, out = sys.argv[1:5]
data = {"result": result, "reason": None if result == "passed" else reason}
order = []
for line in open(figures):
    key, _, raw = line.rstrip("\n").partition("\t")
    value = raw
    if raw in ("true", "false"):
        value = raw == "true"
    elif raw == "":
        value = None
    else:
        for kind in (int, float):
            try:
                value = kind(raw)
                break
            except ValueError:
                pass
    if key not in data:
        order.append(key)
    data[key] = value
with open(out + "/small-machine.json", "w") as handle:
    json.dump(data, handle, indent=2)
    handle.write("\n")
with open(out + "/summary.md", "w") as handle:
    handle.write("### Small machine (2 CPUs, 1 GB)\n\n")
    handle.write("Result: **%s**%s\n\n" % (result, "" if result == "passed" else " - " + reason))
    handle.write("| Figure | Value |\n|---|---|\n")
    for key in order:
        shown = data[key]
        if isinstance(shown, int) and key.endswith("_bytes") and shown >= 1048576:
            shown = "%d (%.1f MiB)" % (shown, shown / 1048576)
        handle.write("| %s | %s |\n" % (key, "" if shown is None else shown))
' "$FIGURES" "$RESULT" "$REASON" "$OUT"
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    cat "$OUT/summary.md" >>"$GITHUB_STEP_SUMMARY"
  fi
}

cleanup() {
  local code=$?
  if [ "$code" -eq 0 ]; then RESULT="passed"; fi
  docker logs "$APP" >"$OUT/container.log" 2>&1 || true
  write_report || true
  docker rm -f "$APP" "$MOCKS" >/dev/null 2>&1 || true
  docker network rm "$NET" >/dev/null 2>&1 || true
  docker volume rm "$CONFIG_VOLUME" "$CACHE_VOLUME" >/dev/null 2>&1 || true
  if [ -n "$WORK" ]; then rm -rf "$WORK" || true; fi
  exit "$code"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# One call to the API as the admin. Prints the HTTP status (000 when there
# was no answer); the body is left in $OUT/body.json.
api() {
  local status method=$1 path=$2 body=${3:-}
  local args=(-sS -o "$OUT/body.json" -w '%{http_code}' -m 60 -X "$method"
    -H 'Content-Type: application/json' -H "Origin: $BASE_URL")
  if [ -n "$TOKEN" ]; then args+=(-H "Authorization: Bearer $TOKEN"); fi
  if [ -n "$body" ]; then args+=(--data "$body"); fi
  : >"$OUT/body.json"
  status=$(curl "${args[@]}" "$BASE_URL$path") || true
  echo "${status:-000}"
}

# As api, but fails the run on anything but a 2xx. A server that is gone
# fails with its own reason first.
api_ok() {
  local status
  status=$(api "$@")
  case "$status" in
    2??) ;;
    *)
      check_alive "while $1 $2 was asked"
      fail "$1 $2 answered $status: $(head -c 400 "$OUT/body.json")"
      ;;
  esac
}

# For a poll: as api_ok, but returns 1 on no answer, 429 or 503, so the
# caller asks again until its own limit.
api_poll() {
  local status
  status=$(api "$@")
  case "$status" in
    2??) return 0 ;;
    000 | 429 | 503)
      check_alive "while $1 $2 was asked"
      echo "$1 $2 answered $status; asking again"
      return 1
      ;;
    *)
      check_alive "while $1 $2 was asked"
      fail "$1 $2 answered $status: $(head -c 400 "$OUT/body.json")"
      ;;
  esac
}

running() {
  [ "$(docker inspect -f '{{.State.Running}}' "$APP" 2>/dev/null)" = "true" ]
}

# Fail when the server's container stopped, or it or anything in its
# cgroup was killed for memory.
check_alive() {
  local kills
  if [ "$(docker inspect -f '{{.State.OOMKilled}}' "$APP" 2>/dev/null)" = "true" ]; then
    record "oom_killed" true
    fail "the server was killed for memory $1"
  fi
  kills=$(oom_kills)
  if [ "${kills:-0}" -gt 0 ]; then
    record "cgroup_oom_kills" "$kills"
    fail "the container's cgroup saw $kills out-of-memory kills $1"
  fi
  if ! running; then
    record "stopped_exit_code" "$(docker inspect -f '{{.State.ExitCode}}' "$APP" 2>/dev/null || true)"
    fail "the server stopped by itself $1"
  fi
}

# The container's cgroup folder on the host.
cgroup_dir() {
  local pid
  pid=$(docker inspect -f '{{.State.Pid}}' "$APP" 2>/dev/null || true)
  echo "/sys/fs/cgroup$(sed -n 's/^0:://p' "/proc/$pid/cgroup" 2>/dev/null || true)"
}

cgroup_value() {
  host_read "$(cgroup_dir)/$1"
}

# The server's process id on the host.
server_pid() {
  { docker top "$APP" -eo pid,comm 2>/dev/null || true; } | awk '$2 == "droppedneedle" { print $1; exit }'
}

server_status_kb() {
  host_read "/proc/$(server_pid)/status" | awk -v key="$1:" '$1 == key { print $2 }'
}

server_write_bytes() {
  host_read "/proc/$(server_pid)/io" | awk '$1 == "write_bytes:" { print $2 }'
}

cpu_usec() {
  cgroup_value cpu.stat | awk '$1 == "usage_usec" { print $2 }'
}

# Bytes the cgroup wrote, over every device.
cgroup_write_bytes() {
  cgroup_value io.stat | awk '
    { seen = 1; for (i = 2; i <= NF; i++) if ($i ~ /^wbytes=/) { sub("wbytes=", "", $i); total += $i } }
    END { if (seen) print total + 0 }'
}

oom_kills() {
  cgroup_value memory.events | awk '$1 == "oom_kill" { print $2 }'
}

# Record the memory figures of the server and its cgroup under a suffix.
record_memory() {
  record "server_vmhwm_$1_bytes" "$(kib "$(server_status_kb VmHWM)")"
  record "cgroup_memory_peak_$1_bytes" "$(cgroup_value memory.peak)"
  # memory.peak counts page cache too; anon is the memory the processes hold.
  record "cgroup_memory_anon_$1_bytes" "$(cgroup_value memory.stat | awk '$1 == "anon" { print $2 }')"
  record "cgroup_oom_kills_$1" "$(oom_kills)"
}

# Wait for /health, failing at the limit or when the server stops. Records
# the seconds it took under the given name.
wait_healthy() {
  local start ip
  start=$(now)
  ip=$(docker inspect "$APP" --format '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}')
  [ -n "$ip" ] || fail "the server has no address"
  BASE_URL="http://$ip:8688"
  until curl -sf -m 5 "$BASE_URL/health" >/dev/null 2>&1; do
    check_alive "before it was healthy"
    if [ "$(seconds_since "$start")" -ge "$HEALTH_SECONDS" ]; then
      fail "the server was not healthy within $HEALTH_SECONDS seconds"
    fi
    sleep 0.5
  done
  record "$1" "$(elapsed "$start" "$(now)")"
}

# Wait until no work is pending: no scan running or queued, no job running,
# the job queues unchanged and under 3 CPU seconds used over 30 seconds.
# Records whether it got there within the limit, how long it took and the
# queues it left.
wait_settled() {
  local start previous="" queues="" busy before after used
  start=$(now)
  while :; do
    before=$(cpu_usec)
    sleep 30
    check_alive "while work was pending"
    after=$(cpu_usec)
    used=$(delta "$before" "$after")
    used=$((${used:-0} / 1000))
    # A busy answer counts as work still pending.
    busy="yes"
    queues=""
    if api_poll GET /api/v3/library/scan/runs/current; then
      busy=$(python3 -I -c '
import json, sys
runs = json.load(open(sys.argv[1]))
print("yes" if runs.get("active") or runs.get("queued") else "no")
' "$OUT/body.json")
    fi
    if api_poll GET /api/v3/admin/queue-stats; then
      queues=$(python3 -I -c '
import json, sys
stats = json.load(open(sys.argv[1]))["queues"]
print(" ".join("%s:%s/%s" % (q["kind"], q["queued"], q["running"]) for q in stats), sum(q["running"] for q in stats))
' "$OUT/body.json")
    fi
    if [ -n "$queues" ] && [ "$busy" = "no" ] && [ "${queues##* }" = "0" ] && [ "$queues" = "$previous" ] && [ "$used" -lt 3000 ]; then
      record "$1_settled" true
      break
    fi
    previous=$queues
    if [ "$(seconds_since "$start")" -ge "$SETTLE_SECONDS" ]; then
      record "$1_settled" false
      break
    fi
  done
  record "$1_settle_seconds" "$(elapsed "$start" "$(now)")"
  record "$1_queues" "${queues% *}"
}

curl_inside() {
  docker exec "$APP" curl -s -o /dev/null -m 3 "$1" && echo 0 || echo $?
}

start_app() {
  docker run -d --name "$APP" --network "$NET" \
    --cpus=2 --memory=1g --memory-swap=1g \
    -e TZ=UTC -e RUST_LOG=info \
    -v "$WORK/music:/music:ro" \
    -v "$CONFIG_VOLUME:/app/config" -v "$CACHE_VOLUME:/app/cache" \
    "$IMAGE" >/dev/null || fail "the server container did not start"
}

# Sign in to the Jellyfin API with an app password and time one list of
# every track, sampling the cgroup's memory while it runs. Reported only.
jellyfin_list() {
  local status secret token samples sampler list cgroup bytes seconds
  status=$(api GET /api/v3/settings/connect-apps)
  [ "$status" = "200" ] || { record "jellyfin_setup_status" "$status"; return; }
  status=$(api PUT /api/v3/settings/connect-apps "$(python3 -I -c '
import json, sys
config = json.load(open(sys.argv[1]))
config["jellyfin_enabled"] = True
print(json.dumps(config))
' "$OUT/body.json")")
  [ "$status" = "200" ] || { record "jellyfin_setup_status" "$status"; return; }
  status=$(api POST /api/v3/me/app-passwords '{"name":"perf"}')
  secret=$(jget "$OUT/body.json" secret)
  [ -n "$secret" ] || { record "jellyfin_setup_status" "$status"; return; }
  status=$(curl -sS -o "$OUT/body.json" -w '%{http_code}' -m 60 -X POST \
    -H 'Content-Type: application/json' -H "Authorization: MediaBrowser $CLIENT" \
    --data "{\"Username\":\"$USERNAME\",\"Pw\":\"$secret\"}" \
    "$BASE_URL/jellyfin/Users/AuthenticateByName") || true
  record "jellyfin_sign_in_status" "${status:-000}"
  token=$(jget "$OUT/body.json" AccessToken)
  [ -n "$token" ] || return

  samples="$OUT/jellyfin-memory.txt"
  cgroup=$(cgroup_dir)
  : >"$samples"
  while :; do
    host_read "$cgroup/memory.current" >>"$samples"
    sleep 0.2
  done &
  sampler=$!
  list=$(curl -s -o /dev/null -m 180-w '%{http_code} %{size_download} %{time_total}' \
    -H "Authorization: MediaBrowser Token=\"$token\", $CLIENT" \
    "$BASE_URL/jellyfin/Items?IncludeItemTypes=Audio&Recursive=true&Limit=1000000") || true
  kill "$sampler" 2>/dev/null || true
  wait "$sampler" 2>/dev/null || true
  read -r status bytes seconds <<<"${list:-000 0 0}"
  record "jellyfin_list_status" "$status"
  record "jellyfin_list_body_bytes" "$bytes"
  record "jellyfin_list_seconds" "$seconds"
  record "jellyfin_list_cgroup_memory_max_bytes" "$(sort -n "$samples" | tail -1)"
  if [ "$(docker inspect -f '{{.State.OOMKilled}}' "$APP")" = "true" ] || ! running; then
    record "jellyfin_list_killed_the_server" true
  else
    record "jellyfin_list_killed_the_server" false
    record_memory "end"
  fi
}

echo "--- image ---"
docker image inspect "$IMAGE" >/dev/null 2>&1 || docker pull "$IMAGE" >/dev/null || fail "no image $IMAGE"
record "image" "$IMAGE"
record "image_digest" "$(docker image inspect -f '{{if .RepoDigests}}{{index .RepoDigests 0}}{{end}}' "$IMAGE")"
record "image_revision" "$(docker image inspect -f '{{index .Config.Labels "org.opencontainers.image.revision"}}' "$IMAGE")"
record "image_version" "$(docker image inspect -f '{{index .Config.Labels "org.opencontainers.image.version"}}' "$IMAGE")"
record "image_architecture" "$(docker image inspect -f '{{.Architecture}}' "$IMAGE")"
record "host_cpus" "$(nproc)"
record "host_memory_bytes" "$(kib "$(awk '$1 == "MemTotal:" { print $2 }' /proc/meminfo)")"
if [ -z "${DN_PERF_MOCKS_IMAGE:-}" ]; then
  docker build -f "$ROOT/server/scripts/e2e-mocks.Dockerfile" -t "$MOCKS_IMAGE" "$ROOT" || fail "mocks build failed"
fi

echo "--- library ---"
WORK=$(mktemp -d)
START=$(now)
python3 -I "$ROOT/server/scripts/gen-library.py" "$WORK/music" --tracks "$TRACKS" || fail "the library was not generated"
record "library_tracks" "$TRACKS"
record "library_generate_seconds" "$(elapsed "$START" "$(now)")"
record "library_disk_bytes" "$(kib "$(du -sk "$WORK/music" | cut -f1)")"
chmod -R a+rX "$WORK/music"

echo "--- sealed network ---"
docker rm -f "$APP" "$MOCKS" >/dev/null 2>&1 || true
docker network rm "$NET" >/dev/null 2>&1 || true
docker volume rm "$CONFIG_VOLUME" "$CACHE_VOLUME" >/dev/null 2>&1 || true
docker network create --internal "$NET" >/dev/null || fail "network create failed"
docker run -d --name "$MOCKS" --network "$NET" "$MOCKS_IMAGE" \
  --slskd 0.0.0.0:5030 --musicbrainz 0.0.0.0:5000 >/dev/null || fail "mocks did not start"
MOCKS_IP=$(docker inspect "$MOCKS" --format '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}')
[ -n "$MOCKS_IP" ] || fail "mocks container has no address"
MOCKS_UP=""
for _ in $(seq 1 60); do
  if curl -sf "http://$MOCKS_IP:5000/ws/2/artist?query=test" >/dev/null 2>&1; then
    MOCKS_UP=1
    break
  fi
  sleep 0.5
done
[ -n "$MOCKS_UP" ] || fail "mocks never answered at $MOCKS_IP"

echo "--- fresh boot ---"
start_app
wait_healthy "health_fresh_seconds"
# A documentation address fails at once with no route (curl 7) and the
# documentation domain does not resolve (curl 6).
ROUTE_RC=$(curl_inside "http://192.0.2.1/")
[ "$ROUTE_RC" = "7" ] || fail "the network has a route out (curl exit $ROUTE_RC)"
NAME_RC=$(curl_inside "http://example.com/")
[ "$NAME_RC" = "6" ] || fail "outside names resolve on the network (curl exit $NAME_RC)"
record "sealed" true
record "server_rss_fresh_bytes" "$(kib "$(server_status_kb VmRSS)")"

echo "--- setup ---"
api_ok POST /api/v3/auth/setup \
  "{\"username\":\"$USERNAME\",\"password\":\"$PASSWORD\",\"display_name\":\"Perf Admin\",\"transport\":\"bearer\"}"
TOKEN=$(jget "$OUT/body.json" token)
[ -n "$TOKEN" ] || fail "setup gave no token"
api_ok PUT /api/v3/settings/musicbrainz \
  "{\"source_mode\":\"mirror\",\"api_url\":\"http://$MOCKS:5000/ws/2\",\"rate_limit\":0}"
api_ok GET /api/v3/settings/library
LIBRARY_SAVE=$(python3 -I -c '
import json, sys, uuid
view = json.load(open(sys.argv[1]))
keys = ("acoustid_api_key", "enabled", "naming_script", "staging_path")
settings = {key: view[key] for key in keys if key in view}
settings["library_roots"] = [
    {"id": str(uuid.uuid4()), "label": "Music", "path": "/music", "policy": "automatic", "rules": []}
]
print(json.dumps({"settings": settings, "expected_policy_revision": view["policy_revision"]}))
' "$OUT/body.json")

echo "--- first scan ---"
# Saving a new folder starts its scan by itself: follow that run. The server
# is fresh, so the first run it reports, live or finished, is the folder's.
SCAN_START=$(now)
api_ok PUT /api/v3/settings/library "$LIBRARY_SAVE"
RUN_ID=""
until [ -n "$RUN_ID" ]; do
  if api_poll GET /api/v3/library/scan/runs/current; then
    RUN_ID=$(jget "$OUT/body.json" active.id)
    [ -n "$RUN_ID" ] || RUN_ID=$(jget "$OUT/body.json" queued.id)
  fi
  if [ -z "$RUN_ID" ] && api_poll GET "/api/v3/library/scan/runs/history?limit=1"; then
    RUN_ID=$(python3 -I -c '
import json, sys
items = json.load(open(sys.argv[1])).get("items") or []
print(items[0]["id"] if items else "")
' "$OUT/body.json")
  fi
  if [ -z "$RUN_ID" ]; then
    if [ "$(seconds_since "$SCAN_START")" -ge "$SCAN_SECONDS" ]; then
      fail "the new folder's scan never started"
    fi
    sleep 1
  fi
done
STATE=""
while :; do
  sleep 5
  check_alive "during the first scan"
  if [ "$(seconds_since "$SCAN_START")" -ge "$SCAN_SECONDS" ]; then
    fail "the first scan did not finish within $SCAN_SECONDS seconds (state ${STATE:-unknown})"
  fi
  api_poll GET "/api/v3/library/scan/runs/$RUN_ID" || continue
  STATE=$(jget "$OUT/body.json" run.state)
  case "$STATE" in
    completed) break ;;
    cancelled | failed | superseded_policy_changed) fail "the first scan ended $STATE" ;;
  esac
done
SCAN_TOOK=$(elapsed "$SCAN_START" "$(now)")
record "scan_run_id" "$RUN_ID"
INDEXED=$(jget "$OUT/body.json" run.counters.indexed_count)
record "scan_seconds" "$SCAN_TOOK"
record "scan_discovered" "$(jget "$OUT/body.json" run.counters.discovered_count)"
record "scan_indexed" "$INDEXED"
record "scan_errored" "$(jget "$OUT/body.json" run.counters.errored_count)"
[ "${INDEXED:-0}" -gt 0 ] || fail "the first scan indexed no file"
record "scan_files_per_second" "$(awk "BEGIN { printf \"%.1f\", $INDEXED / $SCAN_TOOK }")"
wait_settled "after_scan"
record_memory "scan"
record "database_bytes" "$(docker exec "$APP" stat -c %s /app/cache/library.db 2>/dev/null || true)"

echo "--- restart over the full database ---"
docker stop -t 30 "$APP" >/dev/null || fail "the server did not stop"
EXIT_CODE=$(docker inspect -f '{{.State.ExitCode}}' "$APP")
[ "$EXIT_CODE" = "0" ] || fail "the server stopped with exit code $EXIT_CODE"
docker start "$APP" >/dev/null || fail "the server did not start again"
wait_healthy "health_restart_seconds"
wait_settled "after_restart"

echo "--- idle ---"
PID=$(server_pid)
WRITE_BEFORE=$(server_write_bytes)
IO_BEFORE=$(cgroup_write_bytes)
CPU_BEFORE=$(cpu_usec)
sleep "$IDLE_SECONDS"
check_alive "while idle"
[ "$(server_pid)" = "$PID" ] || fail "the server restarted while idle"
CPU_USED=$(delta "$CPU_BEFORE" "$(cpu_usec)")
record "idle_seconds" "$IDLE_SECONDS"
record "idle_server_write_bytes" "$(delta "$WRITE_BEFORE" "$(server_write_bytes)")"
record "idle_cgroup_write_bytes" "$(delta "$IO_BEFORE" "$(cgroup_write_bytes)")"
record "idle_cpu_seconds" "$([ -z "$CPU_USED" ] || awk "BEGIN { printf \"%.2f\", $CPU_USED / 1000000 }")"
record "server_rss_idle_bytes" "$(kib "$(server_status_kb VmRSS)")"
record_memory "idle"

echo "--- Jellyfin list ---"
jellyfin_list

echo "--- budgets ---"
OVER=""
while read -r FIGURE MAXIMUM; do
  [ -n "$FIGURE" ] || continue
  VALUE=$(awk -F '\t' -v key="$FIGURE" '$1 == key { value = $2 } END { print value }' "$FIGURES")
  if [ -n "$VALUE" ] && awk "BEGIN { exit !($VALUE > $MAXIMUM) }"; then
    OVER="$OVER $FIGURE=$VALUE>$MAXIMUM"
  fi
done <<<"$BUDGETS"
[ -z "$OVER" ] || fail "over budget:$OVER"

echo "SMALL MACHINE PASSED"
