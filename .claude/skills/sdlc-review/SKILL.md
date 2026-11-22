---
name: sdlc-review
description: Independent code review gate for the current branch or a diff, in any language the harness supports (Go, Rust, Python, shell). It checks correctness against the spec ACs, concurrency, errors, security and over-engineering, and ends with VERDICT PASS or FAIL. Use before merge, when asked to review changes, or when /sdlc reaches review.
argument-hint: "[spec path | base ref]"
context: fork
agent: code-reviewer
background: false
---

# Review gate

Focus: $ARGUMENTS

You did not write this change. Assume it has bugs until the code shows otherwise.

## Get the change

- **With tools**: base is `git merge-base HEAD origin/HEAD` (fall back to `origin/main`). Read `git diff <base>` and open the surrounding code for anything non-trivial. Run `.claude/check.sh`. If the diff touches `.claude/` or `tests/harness/`, also run `tests/harness/run.sh`. A non-zero exit from either is an automatic blocker.
- **Headless (diff in the message)**: review that diff. Say in "Not checked" what you couldn't see.
- **Spec**: read the matching `docs/specs/NNNN-*.md` if one is in the diff, named in the focus, or named in the branch. Check every AC against a named test.
- **Rules**: read `.claude/rules/<lang>.md` for each language in the diff.

## Checklist (blockers first)

**Blocker**: fail the gate.
- An AC is not met, or has no test that would fail without the change
- Data race, leaked concurrent task (no owner or exit path, cancellation not propagated), unsynchronized shared state
- Error dropped, swallowed or ignored where failure matters; a crash on a request or trading path
- Resource leak: unclosed body, file, connection or rows; a timer or ticker never stopped
- Injection (SQL, shell, template), path traversal, SSRF, missing authz check, unbounded read of untrusted input
- Secrets or PII in logs, errors or test fixtures
- Breaking change to a public API, wire format (HTTP, gRPC/proto, events) or schema with no migration or version
- External call without a timeout
- Binary floating point for prices, quantities, fees or P&L
- Every item in the **Review blockers** section of each touched language's rules file

**Should fix**: list them; they don't fail the gate alone.
- Errors without context, or logged and returned
- Over-engineering: unused params or options, single-implementation interfaces or traits, speculative generics, a new package, crate or dependency the spec didn't need
- A risky branch with no test; flaky patterns (sleeps, ordering assumptions, shared globals)
- Missing doc comments on public identifiers

**Nit**: only if there are three or fewer.

## Output

```
| Sev | Location | Issue | Fix |
|-----|----------|-------|-----|
| blocker | crates/execution/src/journal.rs:88 | order sent before its intent row commits | await the insert, then send |

AC coverage: AC-1 ✓ ac_1_rejects_stale_book · AC-2 ✗ no test
Not checked: <what you could not verify>
VERDICT: FAIL
```

The last line is exactly `VERDICT: PASS` or `VERDICT: FAIL`. It is FAIL if there is any blocker, or if a check failed.
