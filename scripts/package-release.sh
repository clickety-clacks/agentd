#!/usr/bin/env bash
set -euo pipefail

export LC_ALL=C
umask 022

AGENTD_REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
AGENTD_RELEASE_VERSION=0.3.4
AGENTD_BINARY="$AGENTD_REPO_ROOT/target/release/agentd"
AGENTD_ATTENTION_BINARY="$AGENTD_REPO_ROOT/target/release/agentd-attention"
AGENTD_OUTPUT_DIR="$AGENTD_REPO_ROOT/target/release-assets"
AGENTD_SOURCE_DATE_EPOCH=
AGENTD_DRY_RUN=false
AGENTD_INCLUDE_ATTENTION=false

agentd_usage() {
  echo "usage: scripts/package-release.sh [--dry-run] [--binary PATH] [--attention-binary PATH] [--output-dir DIR] [--source-date-epoch SECONDS]" >&2
  exit 2
}

while (($# > 0)); do
  case "$1" in
    --dry-run)
      AGENTD_DRY_RUN=true
      shift
      ;;
    --binary)
      (($# >= 2)) || agentd_usage
      AGENTD_BINARY=$2
      shift 2
      ;;
    --attention-binary)
      (($# >= 2)) || agentd_usage
      AGENTD_ATTENTION_BINARY=$2
      shift 2
      ;;
    --output-dir)
      (($# >= 2)) || agentd_usage
      AGENTD_OUTPUT_DIR=$2
      shift 2
      ;;
    --source-date-epoch)
      (($# >= 2)) || agentd_usage
      AGENTD_SOURCE_DATE_EPOCH=$2
      shift 2
      ;;
    *)
      agentd_usage
      ;;
  esac
done

[[ -x "$AGENTD_BINARY" ]] || {
  echo "package release: binary is not executable: $AGENTD_BINARY" >&2
  exit 1
}

AGENTD_CARGO_VERSION=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' "$AGENTD_REPO_ROOT/Cargo.toml")
[[ "$AGENTD_CARGO_VERSION" == "$AGENTD_RELEASE_VERSION" ]] || {
  echo "package release: Cargo version is $AGENTD_CARGO_VERSION, expected $AGENTD_RELEASE_VERSION" >&2
  exit 1
}

AGENTD_BINARY_VERSION=$("$AGENTD_BINARY" --version)
[[ "$AGENTD_BINARY_VERSION" == "agentd $AGENTD_RELEASE_VERSION" ]] || {
  echo "package release: binary reports '$AGENTD_BINARY_VERSION', expected 'agentd $AGENTD_RELEASE_VERSION'" >&2
  exit 1
}

agentd_attention_omit() {
  printf 'package release: warning: optional attention payload omitted: %s\n' "$1" >&2
}

if [[ ! -x "$AGENTD_ATTENTION_BINARY" ]]; then
  agentd_attention_omit "binary is absent or not executable"
elif [[ ! -f "$AGENTD_REPO_ROOT/docs/agentd-attention.md" || ! -r "$AGENTD_REPO_ROOT/docs/agentd-attention.md" \
  || ! -f "$AGENTD_REPO_ROOT/packaging/systemd/agentd-attention.service" \
  || ! -r "$AGENTD_REPO_ROOT/packaging/systemd/agentd-attention.service" ]]; then
  agentd_attention_omit "invocation documentation or service unit is unavailable"
elif ! command -v timeout >/dev/null 2>&1; then
  agentd_attention_omit "timeout utility is unavailable for the optional binary check"
else
  if AGENTD_ATTENTION_BINARY_VERSION=$(timeout --signal=TERM --kill-after=1s 5s \
    "$AGENTD_ATTENTION_BINARY" --version 2>&1); then
    if [[ "$AGENTD_ATTENTION_BINARY_VERSION" == "agentd-attention $AGENTD_RELEASE_VERSION" ]]; then
      AGENTD_INCLUDE_ATTENTION=true
    else
      agentd_attention_omit "binary reports '$AGENTD_ATTENTION_BINARY_VERSION', expected 'agentd-attention $AGENTD_RELEASE_VERSION'"
    fi
  else
    agentd_attention_omit "binary version check failed: $AGENTD_ATTENTION_BINARY_VERSION"
  fi
fi

for AGENTD_REQUIRED in \
  README.md \
  packaging/systemd/agentd.service \
  skills/agentd/SKILL.md; do
  [[ -f "$AGENTD_REPO_ROOT/$AGENTD_REQUIRED" ]] || {
    echo "package release: required file is missing: $AGENTD_REQUIRED" >&2
    exit 1
  }
done

if [[ -z "$AGENTD_SOURCE_DATE_EPOCH" ]]; then
  AGENTD_SOURCE_DATE_EPOCH=$(git -C "$AGENTD_REPO_ROOT" show -s --format=%ct HEAD)
fi
[[ "$AGENTD_SOURCE_DATE_EPOCH" =~ ^[0-9]+$ ]] || {
  echo "package release: source date epoch must be a nonnegative integer" >&2
  exit 1
}

AGENTD_RUST_HOST=$(rustc -vV | sed -n 's/^host: //p')
[[ -n "$AGENTD_RUST_HOST" ]] || {
  echo "package release: rustc did not report a host" >&2
  exit 1
}

AGENTD_PACKAGE_NAME="agentd-$AGENTD_RELEASE_VERSION-$AGENTD_RUST_HOST"
AGENTD_ARCHIVE_NAME="$AGENTD_PACKAGE_NAME.tar.gz"

printf 'version=%s\n' "$AGENTD_RELEASE_VERSION"
printf 'rust_host=%s\n' "$AGENTD_RUST_HOST"
printf 'source_date_epoch=%s\n' "$AGENTD_SOURCE_DATE_EPOCH"
printf 'archive=%s\n' "$AGENTD_ARCHIVE_NAME"
printf 'mode=0755 path=%s/\n' "$AGENTD_PACKAGE_NAME"
printf 'mode=0755 path=%s/agentd\n' "$AGENTD_PACKAGE_NAME"
printf 'mode=0644 path=%s/README.md\n' "$AGENTD_PACKAGE_NAME"
printf 'mode=0755 path=%s/packaging/\n' "$AGENTD_PACKAGE_NAME"
printf 'mode=0755 path=%s/packaging/systemd/\n' "$AGENTD_PACKAGE_NAME"
printf 'mode=0644 path=%s/packaging/systemd/agentd.service\n' "$AGENTD_PACKAGE_NAME"
printf 'mode=0755 path=%s/skills/\n' "$AGENTD_PACKAGE_NAME"
printf 'mode=0755 path=%s/skills/agentd/\n' "$AGENTD_PACKAGE_NAME"
printf 'mode=0644 path=%s/skills/agentd/SKILL.md\n' "$AGENTD_PACKAGE_NAME"
if [[ "$AGENTD_INCLUDE_ATTENTION" == true ]]; then
  printf 'optional_attention=included\n'
  printf 'mode=0755 path=%s/agentd-attention\n' "$AGENTD_PACKAGE_NAME"
  printf 'mode=0755 path=%s/docs/\n' "$AGENTD_PACKAGE_NAME"
  printf 'mode=0644 path=%s/docs/agentd-attention.md\n' "$AGENTD_PACKAGE_NAME"
  printf 'mode=0644 path=%s/packaging/systemd/agentd-attention.service\n' "$AGENTD_PACKAGE_NAME"
else
  printf 'optional_attention=omitted\n'
fi

if [[ "$AGENTD_DRY_RUN" == true ]]; then
  exit 0
fi

mkdir -p "$AGENTD_REPO_ROOT/target" "$AGENTD_OUTPUT_DIR"
AGENTD_STAGE_ROOT=$(mktemp -d "$AGENTD_REPO_ROOT/target/agentd-package.XXXXXX")
AGENTD_ARCHIVE_TEMP=$(mktemp "$AGENTD_OUTPUT_DIR/.${AGENTD_ARCHIVE_NAME}.XXXXXX")
agentd_cleanup() {
  rm -rf -- "$AGENTD_STAGE_ROOT"
  rm -f -- "$AGENTD_ARCHIVE_TEMP"
}
trap agentd_cleanup EXIT

install -Dm755 "$AGENTD_BINARY" "$AGENTD_STAGE_ROOT/$AGENTD_PACKAGE_NAME/agentd"
install -Dm644 "$AGENTD_REPO_ROOT/README.md" "$AGENTD_STAGE_ROOT/$AGENTD_PACKAGE_NAME/README.md"
install -Dm644 "$AGENTD_REPO_ROOT/packaging/systemd/agentd.service" \
  "$AGENTD_STAGE_ROOT/$AGENTD_PACKAGE_NAME/packaging/systemd/agentd.service"
install -Dm644 "$AGENTD_REPO_ROOT/skills/agentd/SKILL.md" \
  "$AGENTD_STAGE_ROOT/$AGENTD_PACKAGE_NAME/skills/agentd/SKILL.md"
if [[ "$AGENTD_INCLUDE_ATTENTION" == true ]]; then
  install -Dm755 "$AGENTD_ATTENTION_BINARY" "$AGENTD_STAGE_ROOT/$AGENTD_PACKAGE_NAME/agentd-attention"
  install -Dm644 "$AGENTD_REPO_ROOT/docs/agentd-attention.md" \
    "$AGENTD_STAGE_ROOT/$AGENTD_PACKAGE_NAME/docs/agentd-attention.md"
  install -Dm644 "$AGENTD_REPO_ROOT/packaging/systemd/agentd-attention.service" \
    "$AGENTD_STAGE_ROOT/$AGENTD_PACKAGE_NAME/packaging/systemd/agentd-attention.service"
fi
find "$AGENTD_STAGE_ROOT/$AGENTD_PACKAGE_NAME" -type d -exec chmod 0755 {} +

tar \
  --sort=name \
  --format=ustar \
  --mtime="@$AGENTD_SOURCE_DATE_EPOCH" \
  --owner=0 \
  --group=0 \
  --numeric-owner \
  -cf - \
  -C "$AGENTD_STAGE_ROOT" \
  "$AGENTD_PACKAGE_NAME" | gzip -n -9 >"$AGENTD_ARCHIVE_TEMP"

mv -f -- "$AGENTD_ARCHIVE_TEMP" "$AGENTD_OUTPUT_DIR/$AGENTD_ARCHIVE_NAME"
(
  cd -- "$AGENTD_OUTPUT_DIR"
  sha256sum "$AGENTD_ARCHIVE_NAME" >SHA256SUMS
)
printf 'sha256=%s\n' "$(cut -d' ' -f1 "$AGENTD_OUTPUT_DIR/SHA256SUMS")"
