#!/usr/bin/env bash
# Python checker for .claude/check.sh, run once per uv project (pyproject.toml): ruff format,
# ruff check, pyright if installed, and pytest. Everything runs through `uv run --locked`,
# so uv.lock must be committed and tools pinned in the project win over global ones.
# Usage: python.sh --full | python.sh <repo-relative paths...>
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd -P)
cd "$root"
# shellcheck source-path=SCRIPTDIR source=lib.sh
. "$root/.claude/checks/lib.sh"
lang_init python
require uv

found=""
if [ "${1:-}" = "--full" ]; then
  files=$(repo_files)
  while IFS= read -r f; do
    case "${f##*/}" in pyproject.toml) found+="$(dirname "$f")"$'\n' ;; esac
  done <<<"$files"
else
  for f in "$@"; do
    proj=$(nearest pyproject.toml "$(dirname "$f")")
    if [ -z "$proj" ]; then
      echo "== python: $f is not in a pyproject.toml project, not checked"
      continue
    fi
    found+="$proj"$'\n'
  done
fi

projects=()
while IFS= read -r p; do projects+=("$p"); done < <(printf '%s' "$found" | sort -u)
[ ${#projects[@]} -gt 0 ] || exit 0

for proj in "${projects[@]}"; do
  cd "$root/$proj"
  echo "== python: project $proj"

  step "ruff format"
  uv run --locked ruff format --check .

  step "ruff check"
  uv run --locked ruff check .

  # pyright pinned as a dev dependency lives in .venv; uv run also finds a global one.
  if uv run --locked pyright --version >/dev/null 2>&1; then
    step pyright
    uv run --locked pyright
  else
    skip pyright "not installed"
  fi

  step pytest
  uv run --locked pytest -q
done
