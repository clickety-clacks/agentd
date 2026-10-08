#!/usr/bin/env bash
set -euo pipefail

export LC_ALL=C
export PATH=/usr/bin:/bin
umask 077

AGENTD_REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
AGENTD_MANIFEST=${1:-verification/candidate-artifacts.sha256}
AGENTD_TEST_PLAN=${2:-verification/test-executables.txt}
AGENTD_RUST_HOST_FILE=${3:-verification/rust-host.txt}
AGENTD_COMMIT_FILE=${4:-verification/candidate-commit.txt}

agentd_fail() {
  echo "candidate test runner: $*" >&2
  exit 1
}

[[ "$(uname -s)" == Linux ]] || agentd_fail "requires Linux"
[[ "$(uname -m)" == x86_64 ]] || agentd_fail "requires x86_64"
AGENTD_RUNNER_HOST=$(uname -n)
[[ "${AGENTD_RUNNER_HOST,,}" == racter ]] || agentd_fail "requires the authorized Racter runner"

for AGENTD_TOOL in sha256sum setsid env awk find mktemp cut chmod mkdir rm; do
  command -v "$AGENTD_TOOL" >/dev/null 2>&1 || agentd_fail "missing runner tool: $AGENTD_TOOL"
done

for AGENTD_FILE in "$AGENTD_MANIFEST" "$AGENTD_TEST_PLAN" "$AGENTD_RUST_HOST_FILE" "$AGENTD_COMMIT_FILE"; do
  [[ "$AGENTD_FILE" != /* && "$AGENTD_FILE" != *".."* ]] || agentd_fail "expected safe repository-relative paths"
  [[ -f "$AGENTD_REPO_ROOT/$AGENTD_FILE" ]] || agentd_fail "missing $AGENTD_FILE"
done

AGENTD_RUST_HOST=$(<"$AGENTD_REPO_ROOT/$AGENTD_RUST_HOST_FILE")
[[ "$AGENTD_RUST_HOST" =~ ^[A-Za-z0-9_-]+$ ]] || agentd_fail "invalid Rust host triple"
AGENTD_CANDIDATE_COMMIT=$(<"$AGENTD_REPO_ROOT/$AGENTD_COMMIT_FILE")
[[ "$AGENTD_CANDIDATE_COMMIT" =~ ^[0-9a-f]{40}$ ]] || agentd_fail "invalid candidate commit"

(
  cd -- "$AGENTD_REPO_ROOT"
  sha256sum --check --strict "$AGENTD_MANIFEST"
)

agentd_manifest_has() {
  awk -v expected="$1" '$2 == expected { found = 1 } END { exit !found }' \
    "$AGENTD_REPO_ROOT/$AGENTD_MANIFEST"
}

for AGENTD_REQUIRED in \
  "$AGENTD_TEST_PLAN" \
  "$AGENTD_RUST_HOST_FILE" \
  "$AGENTD_COMMIT_FILE" \
  scripts/run-candidate-tests.sh \
  scripts/package-release.sh \
  target/debug/agentd \
  target/debug/agentd-attention; do
  agentd_manifest_has "$AGENTD_REQUIRED" || agentd_fail "manifest omits $AGENTD_REQUIRED"
done

while IFS= read -r -d '' AGENTD_FIXTURE; do
  AGENTD_FIXTURE=${AGENTD_FIXTURE#"$AGENTD_REPO_ROOT"/}
  agentd_manifest_has "$AGENTD_FIXTURE" || agentd_fail "manifest omits fixture $AGENTD_FIXTURE"
done < <(find "$AGENTD_REPO_ROOT/tests/fixtures" -type f -print0)

AGENTD_SCRATCH=$(mktemp -d "/tmp/agentd-attention-runner.XXXXXXXX")
agentd_test_pid=
agentd_cleanup() {
  if [[ -n "$agentd_test_pid" ]]; then
    kill -TERM -- "-$agentd_test_pid" 2>/dev/null || kill -TERM "$agentd_test_pid" 2>/dev/null || true
    wait "$agentd_test_pid" 2>/dev/null || true
  fi
  rm -rf -- "$AGENTD_SCRATCH"
}
trap agentd_cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

mkdir -m 700 "$AGENTD_SCRATCH/home" "$AGENTD_SCRATCH/tmp" \
  "$AGENTD_SCRATCH/config" "$AGENTD_SCRATCH/cache" \
  "$AGENTD_SCRATCH/data" "$AGENTD_SCRATCH/state" "$AGENTD_SCRATCH/bin"

cat >"$AGENTD_SCRATCH/bin/rustc" <<'AGENTD_RUSTC_ADAPTER'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$#" == 1 && "$1" == -vV ]]; then
  printf 'rustc test identity adapter\nhost: %s\n' "$AGENTD_TEST_RUST_HOST"
else
  echo "candidate test runner: Rust compiler execution is unavailable on this runner" >&2
  exit 127
fi
AGENTD_RUSTC_ADAPTER
chmod 0700 "$AGENTD_SCRATCH/bin/rustc"
AGENTD_RUSTC_ADAPTER_SHA256=$(sha256sum "$AGENTD_SCRATCH/bin/rustc" | cut -d' ' -f1)

printf 'runner=%s\n' "$AGENTD_RUNNER_HOST"
printf 'candidate_commit=%s\n' "$AGENTD_CANDIDATE_COMMIT"
printf 'rust_host=%s (identity adapter sha256=%s)\n' "$AGENTD_RUST_HOST" "$AGENTD_RUSTC_ADAPTER_SHA256"
printf 'candidate_root=%s\n' "$AGENTD_REPO_ROOT"
printf 'XDG_RUNTIME_DIR=unset; candidate integration tests provide private per-test runtimes\n'

cd -- "$AGENTD_REPO_ROOT"
declare -A agentd_seen=()
agentd_count=0
while IFS= read -r agentd_test || [[ -n "$agentd_test" ]]; do
  [[ -n "$agentd_test" ]] || continue
  [[ "$agentd_test" != /* && "$agentd_test" != *".."* && "$agentd_test" != *[[:space:]]* ]] \
    || agentd_fail "unsafe test executable path: $agentd_test"
  [[ "$agentd_test" == target/debug/deps/* ]] || agentd_fail "not a Cargo test artifact: $agentd_test"
  agentd_name=${agentd_test##*/}
  case "$agentd_name" in
    agentd-*|agentd_attention-*|captured_procfs-*|integration-*|release-*) ;;
    *) agentd_fail "unexpected test harness: $agentd_test" ;;
  esac
  [[ -z "${agentd_seen[$agentd_test]:-}" ]] || agentd_fail "duplicate test executable: $agentd_test"
  agentd_seen[$agentd_test]=1
  [[ -f "$AGENTD_REPO_ROOT/$agentd_test" && -x "$AGENTD_REPO_ROOT/$agentd_test" ]] \
    || agentd_fail "test executable is missing or not executable: $agentd_test"
  agentd_manifest_has "$agentd_test" || agentd_fail "manifest omits test executable $agentd_test"

  printf 'run: %q --test-threads=1\n' "$agentd_test"
  set +e
  env -i \
    HOME="$AGENTD_SCRATCH/home" \
    TMPDIR="$AGENTD_SCRATCH/tmp" \
    XDG_CONFIG_HOME="$AGENTD_SCRATCH/config" \
    XDG_CACHE_HOME="$AGENTD_SCRATCH/cache" \
    XDG_DATA_HOME="$AGENTD_SCRATCH/data" \
    XDG_STATE_HOME="$AGENTD_SCRATCH/state" \
    AGENTD_TEST_RUST_HOST="$AGENTD_RUST_HOST" \
    PATH="$AGENTD_SCRATCH/bin:/usr/bin:/bin" \
    LC_ALL=C \
    setsid --wait "$AGENTD_REPO_ROOT/$agentd_test" --test-threads=1 &
  agentd_test_pid=$!
  if wait "$agentd_test_pid"; then
    agentd_status=0
  else
    agentd_status=$?
  fi
  kill -TERM -- "-$agentd_test_pid" 2>/dev/null || true
  agentd_test_pid=
  set -e
  printf 'exit=%s path=%s\n' "$agentd_status" "$agentd_test"
  ((agentd_status == 0)) || exit "$agentd_status"
  agentd_test_pid=
  ((agentd_count += 1))
done <"$AGENTD_REPO_ROOT/$AGENTD_TEST_PLAN"

((agentd_count > 0)) || agentd_fail "test plan contained no executables"
printf 'candidate_test_executables_passed=%s\n' "$agentd_count"
