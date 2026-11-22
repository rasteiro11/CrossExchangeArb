#!/usr/bin/env bash
# The SDLC gate. The same checks run in the Claude Code Stop hook, locally and in CI.
# Usage: .claude/check.sh [paths...]
#   no args  full run of every language whose marker file is in the repo
#   paths    files or directories (Go-style ./dir/... too); each one goes to the language that owns it
# A language is one registry line below plus .claude/checks/<lang>.sh, which takes --full or
# repo-relative paths and exits non-zero on failure. Optional tools that are not installed are
# skipped and listed in the summary; a missing required toolchain fails.
# Paths under a testdata/ directory are fixtures and are never checked.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd -P)
cd "$root"
# shellcheck source-path=SCRIPTDIR source=checks/lib.sh
. "$root/.claude/checks/lib.sh"

# lang|marker file that makes a project|file-name globs the language owns
registry=(
  "go|go.mod|*.go go.mod go.sum"
  "rust|Cargo.toml|*.rs Cargo.toml Cargo.lock rust-toolchain.toml clippy.toml rustfmt.toml deny.toml"
  "python|pyproject.toml|*.py pyproject.toml uv.lock"
)

SDLC_SKIPPED=$(mktemp)
export SDLC_SKIPPED
trap 'rm -f "$SDLC_SKIPPED"' EXIT

# owned GLOBS: the paths on stdin whose file name matches one of the space-separated GLOBS.
owned() {
  local f g globs
  read -ra globs <<<"$1"
  while IFS= read -r f; do
    for g in "${globs[@]}"; do
      # shellcheck disable=SC2254 # $g is a glob on purpose
      case "${f##*/}" in $g)
        printf '%s\n' "$f"
        break
        ;;
      esac
    done
  done
}

# Expand the arguments into repo-relative file paths.
paths=""
for p in "$@"; do
  p=${p%/...}
  p=${p#"$root"/}
  p=${p#./}
  [ -n "$p" ] || p=.
  if [ -d "$p" ]; then
    paths+=$(repo_files "$p")$'\n'
  else
    paths+="$p"$'\n'
  fi
done
paths=$(printf '%s' "$paths" | grep -Ev '(^|/)testdata/|^$' | sort -u || true)

all=""
[ $# -gt 0 ] || all=$(repo_files)

ran=0
failed=""
for entry in "${registry[@]}"; do
  IFS='|' read -r name marker globs <<<"$entry"
  args=()
  if [ $# -eq 0 ]; then
    grep -qxE "(.*/)?${marker//./\\.}" <<<"$all" || continue
    args=(--full)
  else
    while IFS= read -r f; do args+=("$f"); done < <(owned "$globs" <<<"$paths")
    [ ${#args[@]} -gt 0 ] || continue
  fi
  ran=1
  "$root/.claude/checks/$name.sh" "${args[@]}" || failed+=" $name"
done

if [ -n "$failed" ]; then
  echo "FAIL:$failed" >&2
  exit 1
fi
if [ "$ran" -eq 0 ]; then
  echo "PASS (no languages detected)"
  exit 0
fi
skipped=$(sort -u "$SDLC_SKIPPED" | tr '\n' ' ')
skipped=${skipped% }
echo "PASS${skipped:+ (skipped: $skipped)}"
