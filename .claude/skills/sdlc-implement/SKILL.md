---
name: sdlc-implement
description: Implement an approved spec or fix a bug test-first, AC by AC, until .claude/check.sh is green, in any language the harness supports (Go, Rust, Python). Use when writing or changing code for a ticket, fixing a bug, addressing review blockers, or when /sdlc reaches the implement phase.
argument-hint: "[docs/specs/NNNN-x.md | bug description | review findings]"
---

# Implement test-first

Input: $ARGUMENTS

Rules: `.claude/SDLC.md` (engineering rules, tests) plus `.claude/rules/<lang>.md` for every language you touch. Ponytail mode applies: smallest complete change.

## 1. Scope before code

- Read the spec ACs (and the ADR if there is one). A spec that is not `Approved` stops here: go back to `/sdlc-spec`.
- **Bug** without a spec: write one line, `Bug: <observed> vs <expected>`, as your AC.
- List every place the change must reach: callers, tests, fixtures, config, migrations, docs. That list is the scope; nothing outside it.

## 2. Loop, one AC at a time

1. Write the test first, named after the AC in the language's form from its rules file (Go `t.Run("AC-2 rejects duplicate order", ...)`, Rust `fn ac_2_rejects_duplicate_order`, Python `test_ac_2_rejects_duplicate_order`).
2. Run only that test (the rules file gives the command) and confirm it **fails for the right reason**. A test that passes before the code exists proves nothing.
3. Write the least code that makes it pass. Reuse before you write: existing code, then the standard library, then dependencies already in the manifest.
4. Re-run it. Then move to the next AC.

Error paths are ACs too: invalid input, dependency failure, timeout, cancellation.

## 3. Gate

Run `.claude/check.sh` and fix until it exits 0 (plus `tests/harness/run.sh` when you changed `.claude/` or `tests/harness/`). Never weaken a test, add a lint suppression (`//nolint`, `#[allow]`, `# noqa`), or skip a check to get there. If a check is wrong, say so and ask.

## 4. Self-check before handing to review

- [ ] Every AC maps to a named, passing test
- [ ] Every new concurrent task has an owner and an exit path; cancellation flows from the caller
- [ ] Every error is handled or returned with context; nothing swallowed
- [ ] Bodies, rows, files, connections and timers are closed
- [ ] No new dependency without an ADR; lock files were changed by the tool only
- [ ] Public identifiers have doc comments
- [ ] The **Self-check** section of each touched language's rules file

End with: `Ready for /sdlc-review`, plus one line on anything you skipped or couldn't verify.
