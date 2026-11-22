# shellcheck shell=bash
# shellcheck disable=SC2317,SC2329 # functions here are called by the scripts that source this file
# Helpers shared by .claude/check.sh and .claude/checks/<lang>.sh. Sourced, never run.

lang=""
current=""

# lang_init LANG: name this checker's steps and print "FAIL: <lang>: <step>" when one fails.
lang_init() {
  lang="$1"
  current="$lang: setup"
  trap 'echo "FAIL: $current" >&2' ERR
}

# step NAME: announce the step that is about to run.
step() {
  current="$lang: $1"
  echo "== $current"
}

# skip TOOL REASON: an optional tool did not run; check.sh lists it in its summary.
skip() {
  echo "== $lang: $1 skipped ($2)"
  if [ -n "${SDLC_SKIPPED:-}" ]; then echo "$1" >>"$SDLC_SKIPPED"; fi
}

# require TOOL: a language with files but without its toolchain fails, it never passes silently.
require() {
  command -v "$1" >/dev/null && return 0
  echo "FAIL: $lang: $1 not installed" >&2
  exit 1
}

# repo_files [PATH]: tracked plus untracked-but-not-ignored files, minus testdata/ fixtures.
# Fails when git cannot list files, so "nothing found" never hides a broken checkout.
repo_files() {
  local listed
  listed=$(git ls-files --cached --others --exclude-standard -- "${1:-.}") || {
    echo "FAIL: git ls-files failed in $(pwd)" >&2
    return 1
  }
  grep -Ev '(^|/)testdata/' <<<"$listed" || true
}

# nearest MARKER DIR: the closest of DIR and its ancestors (up to the repo root) holding MARKER.
nearest() {
  local d="$2"
  while :; do
    if [ -f "$d/$1" ]; then
      printf '%s\n' "$d"
      return
    fi
    case "$d" in . | /) return ;; esac
    d=$(dirname "$d")
  done
}
