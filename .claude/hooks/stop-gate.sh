#!/usr/bin/env bash
# Stop hook: Claude can't end a turn while the files it changed fail the gate.
# Every uncommitted file goes to check.sh, which routes it to the language that owns it and
# ignores the rest. Exit 2 sends the failure back to Claude to fix. Exit 0 lets it stop.
set -uo pipefail
input=$(cat)

# This turn already continued once because of this hook: let Claude stop and report
# instead of looping. CI and the review gate still catch what's left. Plain grep, so the
# escape works even without jq.
if grep -qE '"stop_hook_active"[[:space:]]*:[[:space:]]*true' <<<"$input"; then exit 0; fi

root="${CLAUDE_PROJECT_DIR:-$(pwd)}"
cd "$root" || exit 0
if ! command -v git >/dev/null; then
  echo "SDLC gate: git not installed, cannot see what changed" >&2
  exit 2
fi
# Outside a git repo there are no uncommitted changes to check. Any other git error
# (dubious ownership, an unreadable config) blocks instead of reading as "no changes".
if ! err=$(git rev-parse --git-dir 2>&1 >/dev/null); then
  [ -e "$root/.git" ] || exit 0
  printf 'SDLC gate: git failed in %s:\n%s\n' "$root" "$err" >&2
  exit 2
fi

# Staged, unstaged and untracked files, NUL-separated so odd file names survive. Written to a
# file (a variable can't hold NULs) so a git failure blocks instead of reading as "no changes".
listing=$(mktemp)
trap 'rm -f "$listing"' EXIT
if ! err=$(git status --porcelain -z --untracked-files=all 2>&1 >"$listing"); then
  printf 'SDLC gate: git status failed in %s:\n%s\n' "$root" "$err" >&2
  exit 2
fi

# Rename and copy entries carry the new path first, then the old one, which is skipped.
files=()
while IFS= read -r -d '' entry; do
  files+=("${entry:3}")
  case "${entry:0:2}" in *R* | *C*) IFS= read -r -d '' _ ;; esac
done <"$listing"
[ ${#files[@]} -eq 0 ] && exit 0

if ! out=$("$root/.claude/check.sh" "${files[@]}" 2>&1); then
  printf 'SDLC gate failed for changed files:\n\n%s\n' "$(printf '%s\n' "$out" | tail -n 60)" >&2
  exit 2
fi
exit 0
