---
name: sdlc-ship
description: Prepare a reviewed change for merge with semver call, CHANGELOG entry, PR description and commit message, then hand the push to a human. Use when /sdlc reaches ship or the user asks to prepare a PR or release notes.
argument-hint: "[docs/specs/NNNN-x.md]"
---

# Ship (prepare, don't push)

Spec: $ARGUMENTS

## 1. Confirm the gates

`.claude/check.sh` exits 0 now, the last `/sdlc-review` said `VERDICT: PASS`, and nothing changed since. If any of that is false, stop and say which.

## 2. Semver

Look at what changed in the public surface (`git diff <base>`):
- Go exported identifiers (`apidiff` if installed), Rust `pub` items (`cargo semver-checks` if installed), proto (`buf breaking` if installed), DB schema, required config, CLI flags.

Then:
- Removed or changed public API, wire format or required config → **major** (Go v2+ modules also need a `/vN` path)
- New public API or behavior, backward compatible → **minor**
- Fix with no API change → **patch**

## 3. Write

- **CHANGELOG.md**, only if the repo has one: add under `## [Unreleased]` in Keep a Changelog sections (Added / Changed / Fixed / Removed / Security).
- **Spec**: set `Status: Done`.
- **PR description**, printed for the human:

```markdown
## What
<1-3 sentences>

## Why
Spec: docs/specs/NNNN-x.md · ADR: docs/adr/NNNN-x.md (if any)

## Evidence
- AC-1 → TestX/AC-1 ✓ …
- `.claude/check.sh` green · review VERDICT: PASS

## Risk and rollback
<Risks from the spec, what you did about each, and how to roll back>

## Ops
<Migrations, flags, config, new deps, or "none">
```

- **Commit message**, Conventional Commits: `feat(order): reject duplicate orders (#123)`. Add `BREAKING CHANGE:` if major.

## 4. Hand off

Print the exact commands for the human (`git add`, `git commit`, `git push`, `gh pr create`). Don't run push, merge or tag yourself.
