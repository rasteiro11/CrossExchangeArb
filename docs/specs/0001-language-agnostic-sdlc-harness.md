# 0001: Language-agnostic SDLC harness

Status: Done
Ticket: none (roadmap M0, `docs/roadmap.md`)

## Problem
The harness only knows Go: `check.sh`, the Stop gate, the gofmt PostToolUse hook, `go-reviewer`, the skills and CI. The engine will be Rust, research Python and the control plane Go, in a subdirectory module. As things stand, Rust and Python changes would pass every gate unchecked. The Stop gate also exits 0 when `go` is missing (`.claude/hooks/stop-gate.sh:14`), so a missing toolchain looks like a green build.

## Non-goals
- TypeScript and proto checkers. They are added with their first code (specs 0008/0011) by dropping in `checks/ts.sh` / `checks/proto.sh` and a rules file. The contract below makes that a one-file change.
- Changing the SDLC flow, gates, tracks or artifacts.
- Replacing the CI review tool (`github.com/yourorg/sdlc-harness`). That placeholder is the user's to fix.
- Installing toolchains. The harness detects them; it does not install them.

## Acceptance criteria
Every AC is proven by `tests/harness/run.sh`. It builds throwaway git repos in a temp dir from clean fixtures under `tests/harness/testdata/` (a Go module in `apps/api/`, a Cargo workspace, a uv Python project). It makes the broken variants by mutating each copy, then checks exit codes and output.

- **AC-1** Given a clean repo with Go (subdir module), Rust and Python, when `.claude/check.sh` runs with no args, then it exits 0, prints the `== <lang>: <step>` lines for all three languages, and ends with `PASS`.
- **AC-2** Given a Rust crate with a clippy warning, when `check.sh` runs, then it exits non-zero and stderr contains `FAIL: rust: clippy`. The same holds for a rustfmt diff (`FAIL: rust: fmt`) and a failing test (`FAIL: rust: test`).
- **AC-3** Given only `.py` paths as args, when `check.sh <paths>` runs, then only `python:` steps appear in the output.
- **AC-4** Given an uncommitted `.rs` change that fails clippy, when `stop-gate.sh` runs with `{}` on stdin, then it exits 2 and the message names the owning crate. Given `{"stop_hook_active":true}`, it exits 0.
- **AC-5** Given `.rs` files but no `cargo` on `PATH`, when `check.sh` runs, then it exits non-zero with `FAIL: rust: cargo not installed`. When an optional tool (`cargo-deny`, `staticcheck`, `govulncheck`, `pyright`) is missing, the run still passes and reports `skipped: <tool>`.
- **AC-6** Given a Go module either at the repo root or in `apps/api/`, when `check.sh` runs, then the Go steps match today's: gofmt, vet, `test -race`, staticcheck if installed, and govulncheck on full runs only. A gofmt violation, vet error or race still fails with `FAIL: go: <step>`.
- **AC-7** Given a repo with no recognized language markers, when `check.sh` runs, then it exits 0 and prints `PASS (no languages detected)`.
- **AC-8** Given an Edit/Write hook payload for a `.go`, `.rs` or `.py` file, when `.claude/hooks/format.sh` runs, then the file is formatted by gofmt / rustfmt / ruff respectively if that tool is installed. It exits 0 either way, including for unknown extensions.
- **AC-9** Given the harness docs, when grepped:
  - `.claude/agents/code-reviewer.md` exists and `go-reviewer.md` does not;
  - `sdlc-review` declares `agent: code-reviewer`;
  - `.claude/rules/{go,rust,python}.md` exist with `paths:` frontmatter;
  - `.claude/SDLC.md` has no Go-specific rules outside a pointer to `.claude/rules/`.
- **AC-10** Given `shellcheck` is installed, when `run.sh` runs, then every script in `.claude/` and `tests/harness/` passes shellcheck. Without `shellcheck`, the run reports it as skipped.

## Interfaces touched
- **`.claude/check.sh [paths...]`.** No args means a full run, as today. Args are repo-relative files or directories, grouped by language. Go package patterns (`./x/...`) are still accepted and routed to Go. Output format is unchanged except that step names gain a `<lang>: ` prefix.
- **Checker contract.** `.claude/checks/<lang>.sh --full | <paths...>` exits non-zero on failure. Detection is by marker (`go.mod`, `Cargo.toml`, `pyproject.toml`) via `git ls-files`.
- **Hooks.**
  - `.claude/hooks/stop-gate.sh` now covers all changed files, not just `*.go`.
  - New `.claude/hooks/format.sh` replaces the inline gofmt command in `settings.json`.
- **Settings.** `settings.json` permissions gain cargo build/check/test/clippy/fmt/metadata/tree, `uv run [--locked] ruff|pytest|pyright` (narrow, so `uv run` can't bypass the `ask` rules), and `tests/harness/run.sh`.
- **Agents and skills.**
  - Agent renamed: `go-reviewer` → `code-reviewer`.
  - All five `sdlc-*` skills lose their Go-only wording.
- **Docs.**
  - `.claude/SDLC.md` is split into shared engineering rules plus `.claude/rules/<lang>.md`.
  - `docs/roadmap.md` already exists.
- **CI.** `.github/workflows/sdlc.yml` always sets up Go, Rust, uv and shellcheck, because `tests/harness/run.sh` exercises all of them. It runs `.claude/check.sh`, then `tests/harness/run.sh`. Go uses `stable` until `apps/control-api/go.mod` exists; then it switches to `go-version-file`.

## Risks
- **Data loss or corruption:** none. The scripts only read the repo and run linters/tests. `format.sh` rewrites only the file named in the hook payload.
- **Exposure:**
  - Fixtures contain no secrets.
  - The `settings.json` deny rules for `.env*` and `secrets/**` are kept unchanged.
- **Breaking callers:**
  - The skills and CI call `check.sh` with no args, so they are unaffected.
  - The only caller passing args is `stop-gate.sh`, which changes in the same commit.
  - The `<lang>: ` prefix changes the `FAIL:` text. Only humans read it.
- **Rollout / rollback:**
  - A broken Stop gate could block every turn. The existing `stop_hook_active` escape (AC-4) bounds that to one retry.
  - Rollback is `git revert` of the single commit.

## Assumptions and open questions
- Python checks run through `uv`. `uv` counts as the required Python toolchain, so missing `uv` with `.py` files present fails, as in AC-5.
- Rust checks use `--locked`, so `Cargo.lock` is committed. That is standard for a workspace with binaries.
- Fixture tests run the real toolchains, so `run.sh` needs cargo, go and uv installed. In CI the check job installs them.
- `.claude/rules/*.md` with `paths:` frontmatter is a Claude Code memory feature. The reviewer agent also reads the rules files explicitly, so review does not depend on auto-loading.
- The Rust rules file sets the workspace lints that the M1 workspace will use: `unsafe_code = "forbid"`, and clippy `unwrap_used`/`expect_used` deny outside tests.
