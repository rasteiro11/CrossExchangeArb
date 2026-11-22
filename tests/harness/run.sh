#!/usr/bin/env bash
# Harness self-test for spec 0001 (docs/specs/0001-language-agnostic-sdlc-harness.md).
# Each case builds a throwaway git repo from this repo's .claude/ plus fixtures in
# testdata/, runs the real harness against it and checks exit code and output.
# Broken variants are made by mutating a copy, so the fixtures on disk stay clean.
# Needs go, cargo, uv and jq. Usage: tests/harness/run.sh [case-name-substring]
# shellcheck disable=SC2317,SC2329 # the ac_* cases are invoked indirectly, by name
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export CARGO_TARGET_DIR="$work/target" # one build cache shared by every case
export GIT_AUTHOR_NAME=harness GIT_AUTHOR_EMAIL=harness@example.invalid
export GIT_COMMITTER_NAME=harness GIT_COMMITTER_EMAIL=harness@example.invalid
filter=${1:-}

code=0 so="" se="" out="" tc="" tc_failed=0

# new_repo NAME [FIXTURE...]: print the path of a committed repo holding the harness and fixtures.
new_repo() {
  local d="$work/$1" f
  shift
  mkdir -p "$d"
  cp -R "$repo/.claude" "$d/"
  for f in "$@"; do cp -R "$here/testdata/$f/." "$d/"; done
  echo "# fixture" >"$d/README.md"
  git -C "$d" init -q
  git -C "$d" add -A
  git -C "$d" commit -qm fixture
  printf '%s\n' "$d"
}

# run DIR [VAR=value...] CMD...: run CMD in DIR with stdin $input; set code, so, se, out.
input=""
run() {
  local d="$1"
  shift
  (cd "$d" && printf '%s' "$input" | env "$@") >"$work/so" 2>"$work/se"
  code=$?
  so=$(cat "$work/so")
  se=$(cat "$work/se")
  out="$so"$'\n'"$se"
  input=""
}

# path_without TOOL...: a PATH that finds every command on $PATH except the named tools.
# It is a fresh directory of symlinks, so hiding one tool never hides its neighbours
# (cargo-deny next to cargo, pyright next to uv).
path_without() {
  local all="$work/bin-all" bin t
  if [ ! -d "$all" ]; then
    local p f parts
    mkdir -p "$all"
    IFS=: read -ra parts <<<"$PATH"
    for p in "${parts[@]}"; do
      for f in "$p"/*; do
        # The first PATH entry wins, as in a normal lookup.
        if [ -f "$f" ] && [ -x "$f" ] && [ ! -L "$all/${f##*/}" ]; then ln -s "$f" "$all/${f##*/}"; fi
      done
    done
  fi
  bin=$(mktemp -d "$work/bin.XXXXXX")
  cp -P "$all"/* "$bin"/
  for t in "$@"; do rm -f "$bin/$t"; done
  printf '%s\n' "$bin"
}

# has_go_tool TOOL: the Go checker will find TOOL (it adds the first GOPATH entry's bin to PATH).
has_go_tool() {
  local gp gb
  gp=$(go env GOPATH)
  gb=$(go env GOBIN)
  command -v "$1" >/dev/null || [ -x "${gb:-${gp%%:*}/bin}/$1" ]
}

fail() {
  printf 'FAIL %s: %s\n' "$tc" "$*"
  printf '%s\n' "$out" | tail -n 25 | sed 's/^/    | /'
  tc_failed=1
}
expect_code() { [ "$code" -eq "$1" ] || fail "exit $code, want $1"; }
expect_nonzero() { [ "$code" -ne 0 ] || fail "exit 0, want non-zero"; }
expect_out() { grep -qF -- "$1" <<<"$out" || fail "output lacks: $1"; }
expect_err() { grep -qF -- "$1" <<<"$se" || fail "stderr lacks: $1"; }
expect_no_out() { ! grep -qF -- "$1" <<<"$out" || fail "output has: $1"; }
expect_last_line() { [[ "$(tail -n 1 <<<"$so")" == "$1"* ]] || fail "last stdout line is not $1…"; }

# Mutations that break a fixture copy in one specific way.
rust_clippy_warning() { printf '\n/// Two.\npub fn two() -> i32 {\n    return 2;\n}\n' >>"$1/crates/demo/src/lib.rs"; }
rust_fmt_violation() { printf '\npub fn ugly()->i32{1}\n' >>"$1/crates/demo/src/lib.rs"; }
rust_failing_test() {
  mkdir -p "$1/crates/demo/tests"
  printf '#[test]\nfn fails() {\n    assert_eq!(demo::add(1, 1), 3);\n}\n' >"$1/crates/demo/tests/fail.rs"
}

ac_1_full_run_covers_every_language() {
  local d
  d=$(new_repo ac1 go rust python)
  run "$d" .claude/check.sh
  expect_code 0
  for s in "go: gofmt" "go: vet" "go: test -race" "rust: fmt" "rust: clippy" "rust: test" \
    "python: ruff format" "python: ruff check" "python: pytest"; do
    expect_out "== $s"
  done
  expect_last_line PASS
}

ac_2_rust_failures_fail_the_gate() {
  local d
  d=$(new_repo ac2-clippy rust)
  rust_clippy_warning "$d"
  run "$d" .claude/check.sh
  expect_nonzero
  expect_err "FAIL: rust: clippy"

  d=$(new_repo ac2-fmt rust)
  rust_fmt_violation "$d"
  run "$d" .claude/check.sh
  expect_nonzero
  expect_err "FAIL: rust: fmt"

  d=$(new_repo ac2-test rust)
  rust_failing_test "$d"
  run "$d" .claude/check.sh
  expect_nonzero
  expect_err "FAIL: rust: test"
}

ac_3_scoped_run_checks_only_owning_language() {
  local d
  d=$(new_repo ac3 go rust python)
  run "$d" .claude/check.sh research/calc.py research/tests/test_calc.py
  expect_code 0
  expect_out "== python: pytest"
  expect_no_out "== go:"
  expect_no_out "== rust:"
}

ac_4_stop_gate_checks_changed_files() {
  local d
  d=$(new_repo ac4 rust)
  input='{}'
  run "$d" CLAUDE_PROJECT_DIR="$d" .claude/hooks/stop-gate.sh
  expect_code 0 # nothing changed
  expect_no_out "== rust:"

  rust_clippy_warning "$d"
  input='{}'
  run "$d" CLAUDE_PROJECT_DIR="$d" .claude/hooks/stop-gate.sh
  expect_code 2
  expect_err "FAIL: rust: clippy"
  expect_err "packages demo"

  input='{"stop_hook_active":true}'
  run "$d" CLAUDE_PROJECT_DIR="$d" .claude/hooks/stop-gate.sh
  expect_code 0

  # A staged rename is checked under its new path.
  d=$(new_repo ac4-rename rust)
  rust_failing_test "$d"
  git -C "$d" add -A
  git -C "$d" commit -qm "add test"
  git -C "$d" mv crates/demo/tests/fail.rs crates/demo/tests/renamed.rs
  input='{}'
  run "$d" CLAUDE_PROJECT_DIR="$d" .claude/hooks/stop-gate.sh
  expect_code 2
  expect_err "FAIL: rust: test"

  # A git status failure blocks the turn instead of looking like "nothing changed".
  d=$(new_repo ac4-badindex rust)
  printf 'garbage' >"$d/.git/index"
  input='{}'
  run "$d" CLAUDE_PROJECT_DIR="$d" .claude/hooks/stop-gate.sh
  expect_code 2
  expect_err "git status failed"

  # A repo git refuses to read blocks too; only "no .git at all" lets the turn end.
  d=$(new_repo ac4-badconfig rust)
  git -C "$d" config core.repositoryformatversion 99
  input='{}'
  run "$d" CLAUDE_PROJECT_DIR="$d" .claude/hooks/stop-gate.sh
  expect_code 2
  expect_err "git failed"

  d=$(new_repo ac4-nogit rust)
  input='{}'
  run "$d" PATH="$(path_without git)" CLAUDE_PROJECT_DIR="$d" .claude/hooks/stop-gate.sh
  expect_code 2
  expect_err "git not installed"

  # A file name git status would quote (space) still reaches its checker.
  d=$(new_repo ac4-space python)
  printf 'import os\n' >"$d/research/odd name.py"
  input='{}'
  run "$d" CLAUDE_PROJECT_DIR="$d" .claude/hooks/stop-gate.sh
  expect_code 2
  expect_err "FAIL: python: ruff check"
}

ac_5_missing_toolchains() {
  local d
  # The helper itself must keep a tool that shares a directory with a hidden one.
  mkdir -p "$work/shared-bin"
  printf '#!/bin/sh\n' >"$work/shared-bin/pyright"
  chmod +x "$work/shared-bin/pyright"
  ln -sf "$(command -v uv)" "$work/shared-bin/uv"
  local saved="$PATH" hidden
  PATH="$work/shared-bin:$saved"
  hidden=$(path_without pyright)
  PATH="$saved"
  if [ ! -x "$hidden/uv" ] || [ -e "$hidden/pyright" ]; then fail "path_without pyright must hide pyright and keep uv"; fi
  rm -rf "$work/bin-all" # rebuilt from the real PATH by the next call
  d=$(new_repo ac5-rust rust)
  run "$d" PATH="$(path_without cargo)" .claude/check.sh
  expect_nonzero
  expect_err "FAIL: rust: cargo not installed"

  d=$(new_repo ac5-go go)
  run "$d" PATH="$(path_without go)" .claude/check.sh
  expect_nonzero
  expect_err "FAIL: go: go not installed"

  d=$(new_repo ac5-python python)
  run "$d" PATH="$(path_without uv)" .claude/check.sh
  expect_nonzero
  expect_err "FAIL: python: uv not installed"

  # git is required too: an unlistable checkout fails instead of "no languages detected".
  d=$(new_repo ac5-nogit rust)
  rm -rf "$d/.git"
  run "$d" GIT_CEILING_DIRECTORIES="$work" .claude/check.sh
  expect_nonzero
  expect_err "git ls-files failed"

  # Optional tools missing: still green, and the summary names each one.
  d=$(new_repo ac5-optional go rust python)
  touch "$d/deny.toml" # so cargo-deny is skipped for being missing, not for lacking config
  run "$d" PATH="$(path_without staticcheck govulncheck cargo-deny pyright)" GOPATH="$work/empty-gopath" \
    .claude/check.sh
  expect_code 0
  expect_out "cargo-deny skipped (not installed)"
  expect_last_line "PASS (skipped:"
  for t in staticcheck govulncheck cargo-deny pyright; do
    grep -q "skipped:.*$t" <<<"$(tail -n 1 <<<"$so")" || fail "summary does not list $t"
  done
}

ac_6_go_checks_unchanged() {
  local d
  d=$(new_repo ac6-sub go)
  run "$d" .claude/check.sh
  expect_code 0
  for s in "go: gofmt" "go: vet" "go: test -race"; do expect_out "== $s"; done
  for t in staticcheck govulncheck; do
    if has_go_tool "$t"; then expect_out "== go: $t"; else expect_out "$t skipped"; fi
  done

  run "$d" .claude/check.sh apps/api/sum.go
  expect_code 0
  expect_out "== go: test -race"
  expect_no_out "govulncheck" # full runs only

  d=$(new_repo ac6-root go)
  git -C "$d" mv apps/api/go.mod apps/api/sum.go apps/api/sum_test.go .
  git -C "$d" commit -qm "root module"
  run "$d" .claude/check.sh
  expect_code 0
  expect_out "== go: test -race"

  d=$(new_repo ac6-gofmt go)
  printf '\nfunc Bad()  {}\n' >>"$d/apps/api/sum.go"
  run "$d" .claude/check.sh
  expect_nonzero
  expect_err "FAIL: go: gofmt"

  # Files behind build tags are format-checked too, on full and scoped runs.
  d=$(new_repo ac6-tagged-gofmt go)
  printf '//go:build integration\n\npackage api\n\nfunc  helper( ) {}\n' >"$d/apps/api/it_test.go"
  run "$d" .claude/check.sh
  expect_nonzero
  expect_err "FAIL: go: gofmt"
  run "$d" .claude/check.sh apps/api/it_test.go
  expect_nonzero
  expect_err "FAIL: go: gofmt"

  # A clean package whose files are all behind tags passes (nothing to build here).
  d=$(new_repo ac6-tag-only go)
  mkdir -p "$d/apps/api/integ"
  printf '//go:build integration\n\npackage integ\n\nimport "testing"\n\nfunc TestIt(t *testing.T) {}\n' \
    >"$d/apps/api/integ/it_test.go"
  run "$d" .claude/check.sh apps/api/integ/it_test.go
  expect_code 0
  run "$d" .claude/check.sh
  expect_code 0

  # ... and a badly formatted file in such a package fails the full run (./... skips the dir).
  printf '\nfunc  helper( ) {}\n' >>"$d/apps/api/integ/it_test.go"
  run "$d" .claude/check.sh
  expect_nonzero
  expect_err "FAIL: go: gofmt"

  d=$(new_repo ac6-vet go)
  printf 'package api\n\nimport "fmt"\n\n// Bad misuses Sprintf.\nfunc Bad() string { return fmt.Sprintf("%%d", "x") }\n' \
    >"$d/apps/api/bad.go"
  run "$d" .claude/check.sh
  expect_nonzero
  expect_err "FAIL: go: vet"

  d=$(new_repo ac6-race go)
  cat >"$d/apps/api/race_test.go" <<'EOF'
package api

import "testing"

func TestRace(t *testing.T) {
	x := 0
	done := make(chan struct{})
	go func() {
		x++
		close(done)
	}()
	x++
	<-done
	_ = x
}
EOF
  run "$d" .claude/check.sh
  expect_nonzero
  expect_err "FAIL: go: test -race"
}

ac_7_no_languages_detected() {
  local d
  d=$(new_repo ac7)
  run "$d" .claude/check.sh
  expect_code 0
  expect_last_line "PASS (no languages detected)"

  run "$d" .claude/check.sh README.md
  expect_code 0
  expect_last_line "PASS (no languages detected)"
}

# format FILE [VAR=value...]: feed format.sh a PostToolUse payload for FILE.
format() {
  local f="$1"
  shift
  input=$(printf '{"tool_input":{"file_path":"%s"}}' "$f")
  run "$work" "$@" "$repo/.claude/hooks/format.sh"
  expect_code 0
}

ac_8_format_hook() {
  local d="$work/ac8"
  mkdir -p "$d"
  printf 'package a\nfunc  F( ) {}\n' >"$d/a.go"
  printf 'fn  main( ){}\n' >"$d/a.rs"
  printf 'x=1\n' >"$d/a.py"
  printf 'x=1\n' >"$d/a.txt"
  cp "$d/a.go" "$d/b.go"
  cp "$d/a.rs" "$d/b.rs"
  cp "$d/a.py" "$d/b.py"

  format "$d/a.go"
  grep -qF 'func F() {}' "$d/a.go" || fail "a.go not gofmt-ed"
  format "$d/a.rs"
  grep -qF 'fn main() {}' "$d/a.rs" || fail "a.rs not rustfmt-ed"
  format "$d/a.py"
  grep -qF 'x = 1' "$d/a.py" || fail "a.py not ruff-formatted"
  format "$d/a.txt"
  grep -qxF 'x=1' "$d/a.txt" || fail "a.txt changed"
  format "$d/missing.go"

  mkdir -p "$d/m"
  printf 'mod child;\nfn  main( ){}\n' >"$d/m/main.rs"
  printf 'fn  x( ){}\n' >"$d/m/child.rs"
  cp "$d/m/child.rs" "$d/m/child.rs.orig"
  format "$d/m/main.rs"
  grep -qF 'fn main() {}' "$d/m/main.rs" || fail "m/main.rs not rustfmt-ed"
  cmp -s "$d/m/child.rs" "$d/m/child.rs.orig" || fail "formatting main.rs rewrote child.rs"

  # rustfmt honours the rustfmt.toml next to the file, not the hook's working directory.
  mkdir -p "$d/tabs"
  printf 'hard_tabs = true\n' >"$d/tabs/rustfmt.toml"
  printf 'fn main() {\nlet x = 1;\n}\n' >"$d/tabs/main.rs"
  format "$d/tabs/main.rs"
  grep -q "$(printf '^\tlet x = 1;')" "$d/tabs/main.rs" || fail "tabs/main.rs ignored its rustfmt.toml"

  # Inside a uv project the hook uses the project's pinned ruff, like the gate does.
  cp -R "$here/testdata/python/research" "$d/py"
  printf 'x=1\n' >"$d/py/fmt_me.py"
  format "$d/py/fmt_me.py" PATH="$(path_without ruff)"
  grep -qF 'x = 1' "$d/py/fmt_me.py" || fail "py/fmt_me.py not formatted by the project's ruff"

  local nopath
  nopath=$(path_without gofmt rustfmt ruff)
  for f in b.go b.rs b.py; do
    cp "$d/$f" "$d/$f.orig"
    format "$d/$f" PATH="$nopath"
    cmp -s "$d/$f" "$d/$f.orig" || fail "$f changed without its formatter"
  done
}

ac_9_harness_docs_are_language_agnostic() {
  out=""
  [ -f "$repo/.claude/agents/code-reviewer.md" ] || fail "no .claude/agents/code-reviewer.md"
  [ ! -e "$repo/.claude/agents/go-reviewer.md" ] || fail ".claude/agents/go-reviewer.md still exists"
  grep -qx 'agent: code-reviewer' "$repo/.claude/skills/sdlc-review/SKILL.md" ||
    fail "sdlc-review does not use code-reviewer"
  for l in go rust python; do
    local f="$repo/.claude/rules/$l.md"
    [ -f "$f" ] || { fail "missing $f"; continue; }
    if [ "$(head -n 1 "$f")" != "---" ] || ! grep -q '^paths:' "$f"; then fail "$f has no paths: frontmatter"; fi
  done
  out=$(grep -nE 'gofmt|go vet|t\.Run|errors\.(Is|As)|log/slog|http\.(DefaultClient|MaxBytesReader)|go\.mod|go mod|goroutine|\(Go\)' \
    "$repo/.claude/SDLC.md")
  [ -z "$out" ] || fail "SDLC.md still has Go-specific rules"
  grep -qF '.claude/rules/' "$repo/.claude/SDLC.md" || fail "SDLC.md does not point to .claude/rules/"
}

ac_10_scripts_pass_shellcheck() {
  if ! command -v shellcheck >/dev/null; then
    echo "     skipped: shellcheck not installed"
    return
  fi
  local f
  for f in "$repo"/.claude/check.sh "$repo"/.claude/checks/*.sh "$repo"/.claude/hooks/*.sh "$repo"/tests/harness/run.sh; do
    run "$(dirname "$f")" shellcheck -x "$(basename "$f")"
    expect_code 0
  done
}

failed=0
for tc in ac_1_full_run_covers_every_language ac_2_rust_failures_fail_the_gate \
  ac_3_scoped_run_checks_only_owning_language ac_4_stop_gate_checks_changed_files \
  ac_5_missing_toolchains ac_6_go_checks_unchanged ac_7_no_languages_detected \
  ac_8_format_hook ac_9_harness_docs_are_language_agnostic ac_10_scripts_pass_shellcheck; do
  [ -n "$filter" ] && [[ "$tc" != *"$filter"* ]] && continue
  tc_failed=0
  "$tc"
  if [ "$tc_failed" -eq 0 ]; then echo "ok   $tc"; else failed=$((failed + 1)); fi
done

if [ "$failed" -ne 0 ]; then
  echo "FAIL: $failed case(s)"
  exit 1
fi
echo "PASS"
