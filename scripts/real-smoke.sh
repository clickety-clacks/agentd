#!/usr/bin/env bash
set -euo pipefail

AGENTD_REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
AGENTD_UID=$(id -u)
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$AGENTD_UID}
AGENTD_CODEX=$(command -v codex)
AGENTD_CLAUDE=$(command -v claude)
AGENTD_STRACE=$(command -v strace)
AGENTD_UNIT_SOURCE="$AGENTD_REPO_ROOT/packaging/systemd/agentd.service"
AGENTD_BINARY_TARGET="$HOME/.local/bin/agentd"
AGENTD_UNIT_TARGET="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/agentd.service"
AGENTD_RUN_ID=$(date -u +%Y%m%dT%H%M%SZ)-$$
AGENTD_EVIDENCE="$AGENTD_REPO_ROOT/target/agentd-smoke/$AGENTD_RUN_ID"
AGENTD_SHARED_CWD="$AGENTD_EVIDENCE/shared-cwd"
AGENTD_FIXTURES="$AGENTD_EVIDENCE/fixtures"
AGENTD_SECRET="agentd-private-$AGENTD_RUN_ID"
AGENTD_INSTALLED=0
AGENTD_WATCH_PID=""
AGENTD_TOOL_UNITS=(
  agentd-smoke-codex-1.service
  agentd-smoke-codex-2.service
  agentd-smoke-codex-3.service
  agentd-smoke-claude.service
  agentd-smoke-agentd-trace.service
)

for AGENTD_REQUIRED in cargo jq rg systemctl systemd-run journalctl ss strace tail; do
  command -v "$AGENTD_REQUIRED" >/dev/null
done
test -n "${XDG_RUNTIME_DIR:-}"
test -S "$XDG_RUNTIME_DIR/systemd/private"
if test -e "$AGENTD_BINARY_TARGET" || test -e "$AGENTD_UNIT_TARGET"; then
  echo "real smoke refuses to overwrite an existing agentd binary or unit" >&2
  exit 1
fi

mkdir -p "$AGENTD_EVIDENCE" "$AGENTD_SHARED_CWD" "$AGENTD_FIXTURES"

agentd_cleanup() {
  set +e
  if test -n "$AGENTD_WATCH_PID"; then
    kill "$AGENTD_WATCH_PID" 2>/dev/null
    wait "$AGENTD_WATCH_PID" 2>/dev/null
  fi
  for AGENTD_UNIT in "${AGENTD_TOOL_UNITS[@]}"; do
    systemctl --user stop "$AGENTD_UNIT" >/dev/null 2>&1
  done
  if test "$AGENTD_INSTALLED" -eq 1; then
    systemctl --user disable --now agentd.service >/dev/null 2>&1
    rm -f "$AGENTD_UNIT_TARGET" "$AGENTD_BINARY_TARGET"
    systemctl --user daemon-reload >/dev/null 2>&1
  fi
}
trap agentd_cleanup EXIT

cd "$AGENTD_REPO_ROOT"
git rev-parse HEAD >"$AGENTD_EVIDENCE/commit.txt"
printf 'scripts/real-smoke.sh\n' >"$AGENTD_EVIDENCE/command.txt"
printf '%s\n' "$XDG_RUNTIME_DIR/agentd.sock" >"$AGENTD_EVIDENCE/socket-path.txt"
cargo build --release >"$AGENTD_EVIDENCE/build.log" 2>&1
install -Dm755 target/release/agentd "$AGENTD_BINARY_TARGET"
install -Dm644 "$AGENTD_UNIT_SOURCE" "$AGENTD_UNIT_TARGET"
AGENTD_INSTALLED=1
systemctl --user daemon-reload
systemctl --user enable --now agentd.service
systemctl --user is-enabled agentd.service >"$AGENTD_EVIDENCE/service-enabled.txt"
systemctl --user is-active agentd.service >"$AGENTD_EVIDENCE/service-active.txt"
systemctl --user show agentd.service --property=ExecStart --value --no-pager \
  >"$AGENTD_EVIDENCE/exec-start.txt"
rg -F "path=$AGENTD_BINARY_TARGET" "$AGENTD_EVIDENCE/exec-start.txt" >/dev/null
rg -F "argv[]=$AGENTD_BINARY_TARGET daemon ;" "$AGENTD_EVIDENCE/exec-start.txt" >/dev/null
if rg -q -- '--socket|agentd\.sock' "$AGENTD_EVIDENCE/exec-start.txt"; then
  echo "installed ExecStart contains a socket-path argument" >&2
  exit 1
fi
test "$(stat -c '%a' "$XDG_RUNTIME_DIR/agentd.sock")" = 600

"$AGENTD_BINARY_TARGET" watch --json >"$AGENTD_EVIDENCE/watch.ndjson" \
  2>"$AGENTD_EVIDENCE/watch.stderr" &
AGENTD_WATCH_PID=$!

agentd_start_tool() {
  local AGENTD_UNIT_NAME=$1
  local AGENTD_TOOL_PATH=$2
  local AGENTD_TOOL_KIND=$3
  local AGENTD_LAUNCHER
  if test "$AGENTD_TOOL_KIND" = codex; then
    AGENTD_LAUNCHER="tail -f /dev/null | /bin/bash -lc 'exec -a $AGENTD_SECRET $AGENTD_TOOL_PATH app-server'"
  else
    AGENTD_LAUNCHER="tail -f /dev/null | /bin/bash -lc 'exec -a $AGENTD_SECRET $AGENTD_TOOL_PATH --print --input-format stream-json --output-format stream-json'"
  fi
  systemd-run --user --unit="$AGENTD_UNIT_NAME" --collect \
    --property="WorkingDirectory=$AGENTD_SHARED_CWD" \
    --setenv="AGENTD_SMOKE_SECRET=$AGENTD_SECRET" \
    /bin/bash -lc "$AGENTD_LAUNCHER" >/dev/null
}

agentd_start_tool agentd-smoke-codex-1 "$AGENTD_CODEX" codex
agentd_start_tool agentd-smoke-codex-2 "$AGENTD_CODEX" codex
agentd_start_tool agentd-smoke-codex-3 "$AGENTD_CODEX" codex
agentd_start_tool agentd-smoke-claude "$AGENTD_CLAUDE" claude

AGENTD_START_MS=$(date +%s%3N)
AGENTD_DEADLINE_MS=$((AGENTD_START_MS + 2000))
while :; do
  "$AGENTD_BINARY_TARGET" list --json >"$AGENTD_EVIDENCE/four-agents.json"
  AGENTD_MATCHING=$(jq --arg cwd "$AGENTD_SHARED_CWD" \
    '[.agents[] | select(.cwd.state == "known" and .cwd.value == $cwd)] | length' \
    "$AGENTD_EVIDENCE/four-agents.json")
  if test "$AGENTD_MATCHING" -eq 4; then
    break
  fi
  if test "$(date +%s%3N)" -gt "$AGENTD_DEADLINE_MS"; then
    echo "four real agents were not published within two seconds" >&2
    exit 1
  fi
  sleep 0.05
done
AGENTD_DISCOVERY_MS=$(( $(date +%s%3N) - AGENTD_START_MS ))
printf '%s\n' "$AGENTD_DISCOVERY_MS" >"$AGENTD_EVIDENCE/discovery-ms.txt"
jq --arg cwd "$AGENTD_SHARED_CWD" -e '
  [.agents[] | select(.cwd.state == "known" and .cwd.value == $cwd)] as $agents |
  ($agents | length) == 4 and
  ([$agents[] | select(.harness == "codex")] | length) == 3 and
  ([$agents[] | select(.harness == "claude")] | length) == 1 and
  all($agents[]; .presence.state == "present" and .activity.state == "unknown")
' "$AGENTD_EVIDENCE/four-agents.json" >/dev/null

printf '%s\n' "$AGENTD_SHARED_CWD" >"$AGENTD_FIXTURES/cwd"
jq -r --arg cwd "$AGENTD_SHARED_CWD" \
  '.agents[] | select(.cwd.value == $cwd) | .id.pid' \
  "$AGENTD_EVIDENCE/four-agents.json" | sort -n >"$AGENTD_FIXTURES/pids"
while read -r AGENTD_PROCESS_PID; do
  mkdir -p "$AGENTD_FIXTURES/$AGENTD_PROCESS_PID"
  cp "/proc/$AGENTD_PROCESS_PID/stat" "$AGENTD_FIXTURES/$AGENTD_PROCESS_PID/stat"
  cp "/proc/$AGENTD_PROCESS_PID/status" "$AGENTD_FIXTURES/$AGENTD_PROCESS_PID/status"
  readlink "/proc/$AGENTD_PROCESS_PID/cwd" >"$AGENTD_FIXTURES/$AGENTD_PROCESS_PID/cwd"
done <"$AGENTD_FIXTURES/pids"
AGENTD_CAPTURED_PROCFS_DIR="$AGENTD_FIXTURES" \
  cargo test --test captured_procfs -- --ignored >"$AGENTD_EVIDENCE/fixture-replay.log" 2>&1

AGENTD_ACTIVITY_PID=$(jq -r --arg cwd "$AGENTD_SHARED_CWD" '
  [.agents[] |
    select(.cwd.state == "known" and .cwd.value == $cwd and .harness == "codex") |
    .id.pid] | min // empty
' "$AGENTD_EVIDENCE/four-agents.json")
test -n "$AGENTD_ACTIVITY_PID"
"$AGENTD_BINARY_TARGET" activity --pid "$AGENTD_ACTIVITY_PID" --state active
AGENTD_ACTIVITY_DEADLINE=$(( $(date +%s%3N) + 2000 ))
while :; do
  "$AGENTD_BINARY_TARGET" list --json >"$AGENTD_EVIDENCE/activity.json"
  if jq -e --argjson pid "$AGENTD_ACTIVITY_PID" \
    '.agents[] | select(.id.pid == $pid and .activity.state == "active" and .activity.source == "hook")' \
    "$AGENTD_EVIDENCE/activity.json" >/dev/null; then
    break
  fi
  test "$(date +%s%3N)" -le "$AGENTD_ACTIVITY_DEADLINE"
  sleep 0.05
done

AGENTD_EXIT_START_MS=$(date +%s%3N)
systemctl --user stop agentd-smoke-claude.service
AGENTD_EXIT_DEADLINE=$((AGENTD_EXIT_START_MS + 2000))
while :; do
  "$AGENTD_BINARY_TARGET" list --json >"$AGENTD_EVIDENCE/after-exit.json"
  AGENTD_MATCHING=$(jq --arg cwd "$AGENTD_SHARED_CWD" \
    '[.agents[] | select(.cwd.state == "known" and .cwd.value == $cwd)] | length' \
    "$AGENTD_EVIDENCE/after-exit.json")
  if test "$AGENTD_MATCHING" -eq 3; then
    break
  fi
  if test "$(date +%s%3N)" -gt "$AGENTD_EXIT_DEADLINE"; then
    echo "real Claude exit was not published within two seconds" >&2
    exit 1
  fi
  sleep 0.05
done
printf '%s\n' "$(( $(date +%s%3N) - AGENTD_EXIT_START_MS ))" >"$AGENTD_EVIDENCE/exit-ms.txt"

AGENTD_FIRST_INSTANCE=$(jq -r .instanceId "$AGENTD_EVIDENCE/after-exit.json")
AGENTD_FAILED_PID=$(systemctl --user show agentd.service -p MainPID --value)
kill -ABRT "$AGENTD_FAILED_PID"
wait "$AGENTD_WATCH_PID" 2>/dev/null || true
AGENTD_WATCH_PID=""
AGENTD_RESTART_DEADLINE=$(( $(date +%s%3N) + 5000 ))
AGENTD_SECOND_INSTANCE=$AGENTD_FIRST_INSTANCE
while test "$AGENTD_SECOND_INSTANCE" = "$AGENTD_FIRST_INSTANCE"; do
  if "$AGENTD_BINARY_TARGET" list --json >"$AGENTD_EVIDENCE/after-restart.json" 2>/dev/null; then
    AGENTD_SECOND_INSTANCE=$(jq -r .instanceId "$AGENTD_EVIDENCE/after-restart.json")
  fi
  if test "$(date +%s%3N)" -gt "$AGENTD_RESTART_DEADLINE"; then
    echo "systemd did not restart agentd after an unsuccessful exit" >&2
    exit 1
  fi
  sleep 0.05
done
printf 'before=%s\nafter=%s\n' "$AGENTD_FIRST_INSTANCE" "$AGENTD_SECOND_INSTANCE" \
  >"$AGENTD_EVIDENCE/instance-ids.txt"
jq -e 'all(.agents[]; .activity.state == "unknown")' \
  "$AGENTD_EVIDENCE/after-restart.json" >/dev/null

AGENTD_DAEMON_PID=$(systemctl --user show agentd.service -p MainPID --value)
ss -lntp >"$AGENTD_EVIDENCE/inet-listeners.txt"
ss -ntp >"$AGENTD_EVIDENCE/inet-connections.txt"
ss -xlpn >"$AGENTD_EVIDENCE/unix-listeners.txt"
if rg -q "pid=$AGENTD_DAEMON_PID" \
  "$AGENTD_EVIDENCE/inet-listeners.txt" "$AGENTD_EVIDENCE/inet-connections.txt"; then
  echo "agentd opened an IPv4, IPv6, or outbound network connection" >&2
  exit 1
fi
rg -q "agentd.sock" "$AGENTD_EVIDENCE/unix-listeners.txt"

systemctl --user stop agentd.service
AGENTD_TRACE_STOP_DEADLINE=$(( $(date +%s%3N) + 5000 ))
while test -e "$XDG_RUNTIME_DIR/agentd.sock"; do
  if test "$(date +%s%3N)" -gt "$AGENTD_TRACE_STOP_DEADLINE"; then
    echo "agentd socket survived trace-phase service stop" >&2
    exit 1
  fi
  sleep 0.05
done
systemd-run --user --unit=agentd-smoke-agentd-trace.service --collect \
  --property=Type=simple \
  "$AGENTD_STRACE" -f -tt -e trace=network,file \
  -o "$AGENTD_EVIDENCE/network-trace.log" "$AGENTD_BINARY_TARGET" daemon >/dev/null
AGENTD_TRACE_START_DEADLINE=$(( $(date +%s%3N) + 5000 ))
while ! test -S "$XDG_RUNTIME_DIR/agentd.sock"; do
  if test "$(date +%s%3N)" -gt "$AGENTD_TRACE_START_DEADLINE"; then
    echo "traced installed daemon did not create its socket" >&2
    exit 1
  fi
  sleep 0.05
done
AGENTD_TRACE_SCAN_START=$(date +%s%3N)
sleep 0.35
"$AGENTD_BINARY_TARGET" activity --pid "$AGENTD_ACTIVITY_PID" --state idle
"$AGENTD_BINARY_TARGET" list --json >"$AGENTD_EVIDENCE/network-trace-activity.json"
jq -e --argjson pid "$AGENTD_ACTIVITY_PID" \
  '.agents[] | select(.id.pid == $pid and .activity.state == "idle" and .activity.source == "hook")' \
  "$AGENTD_EVIDENCE/network-trace-activity.json" >/dev/null
printf 'scan_window_start_unix_ms=%s\nactivity_pid=%s\nactivity_state=idle\n' \
  "$AGENTD_TRACE_SCAN_START" "$AGENTD_ACTIVITY_PID" \
  >"$AGENTD_EVIDENCE/network-trace-window.txt"
systemctl --user stop agentd-smoke-agentd-trace.service
rg -q '/proc' "$AGENTD_EVIDENCE/network-trace.log"
rg -q 'socket\(AF_UNIX' "$AGENTD_EVIDENCE/network-trace.log"
if rg -q 'socket\(AF_INET6?|connect\([^\n]*sa_family=AF_INET6?' \
  "$AGENTD_EVIDENCE/network-trace.log"; then
  echo "dynamic trace found an IPv4, IPv6, or outbound network syscall" >&2
  exit 1
fi
systemctl --user start agentd.service
AGENTD_TRACE_RESTART_DEADLINE=$(( $(date +%s%3N) + 5000 ))
while ! test -S "$XDG_RUNTIME_DIR/agentd.sock"; do
  if test "$(date +%s%3N)" -gt "$AGENTD_TRACE_RESTART_DEADLINE"; then
    echo "agentd service did not recover after the trace phase" >&2
    exit 1
  fi
  sleep 0.05
done

journalctl --user -u agentd.service --since "5 minutes ago" --no-pager \
  >"$AGENTD_EVIDENCE/agentd-journal.txt"
if rg -q "$AGENTD_SECRET" "$AGENTD_EVIDENCE/four-agents.json" \
  "$AGENTD_EVIDENCE/watch.ndjson" "$AGENTD_EVIDENCE/watch.stderr" \
  "$AGENTD_EVIDENCE/agentd-journal.txt"; then
  echo "privacy sentinel appeared in agentd output" >&2
  exit 1
fi

AGENTD_STOP_START_MS=$(date +%s%3N)
systemctl --user stop agentd.service
while test -e "$XDG_RUNTIME_DIR/agentd.sock"; do
  if test $(( $(date +%s%3N) - AGENTD_STOP_START_MS )) -gt 5000; then
    echo "agentd socket survived service stop for more than five seconds" >&2
    exit 1
  fi
  sleep 0.05
done
printf '%s\n' "$(( $(date +%s%3N) - AGENTD_STOP_START_MS ))" >"$AGENTD_EVIDENCE/stop-ms.txt"
printf 'success\n' >"$AGENTD_EVIDENCE/teardown-result.txt"

echo "$AGENTD_EVIDENCE"
