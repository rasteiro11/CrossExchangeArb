# Team SDLC

This repo uses the SDLC harness. Claude follows this file in every session.

## Flow

Start a ticket with `/sdlc <ticket text | issue URL | spec path>`. Pick the smallest track that fits:

| Track   | When                                              | Phases                                             |
|---------|---------------------------------------------------|----------------------------------------------------|
| Tiny    | No behavior change: typo, comment, config value   | implement → check                                  |
| Bug     | Behavior is wrong today                           | implement (failing test first) → review → ship     |
| Feature | New or changed behavior                           | spec → design (only if needed) → implement → review → ship |

Gates. Never skip one, never mark one passed on your own:

1. **Spec approved**: a human says so; set `Status: Approved` in the spec.
2. **Design approved**: only when design ran; a human says so.
3. **Check green**: `.claude/check.sh` exits 0. It runs each language's formatter, linters and tests (with the race detector where the language has one), plus vulnerability and license scans when installed. Changes to `.claude/` or `tests/harness/` must also pass `tests/harness/run.sh`.
4. **Review PASS**: `/sdlc-review` ends with `VERDICT: PASS`. Two FAIL rounds in a row means stop and ask a human.
5. **Ship**: a human pushes, merges and tags. Claude prepares; it never pushes.

Artifacts live in the repo, so any session can resume:
- Roadmap: `docs/roadmap.md`
- Specs: `docs/specs/NNNN-slug.md` (`Status: Draft | Approved | Done`)
- Decisions: `docs/adr/NNNN-slug.md`
- Tests name the acceptance criterion they prove (`AC-2` / `ac_2` in the test name)

## Ponytail and this harness

The Ponytail plugin is on for this repo (full mode). Ponytail decides **how much code**; this file decides **which gates**.
They don't conflict. Tests for risky logic, input validation, error handling, the spec and the review are required scope, never "unrequested".
Everything else is Ponytail's call: skip what isn't needed, reuse what exists, the standard library before new dependencies.

## Languages

Language-specific rules live in `.claude/rules/<lang>.md`: `go.md`, `rust.md`, `python.md`. Read the file for every language you touch. Its **Review blockers** and **Self-check** sections are part of the review and implement gates. The rules below apply to every language.

Each language has a checker, `.claude/checks/<lang>.sh`. To add a language, add five things: a checker (it takes `--full` or repo-relative paths, exits non-zero on failure, and uses `checks/lib.sh`), its line in the registry in `.claude/check.sh`, a rules file with `paths:` frontmatter, its toolchain in `.github/workflows/sdlc.yml`, and cases in `tests/harness/run.sh`. That change is a Feature with its own spec.

## Engineering rules (all languages)

- **Versions**: use the toolchain version the repo pins (manifest or toolchain file) and what that version offers.
- **Errors**: return or propagate them; don't panic or crash on expected failures. Add context once per layer. Branch on error types or values, never on message strings. Never log *and* return the same error.
- **Cancellation**: anything that does I/O or may block takes the caller's cancellation signal and honors it. Don't store it in long-lived structs.
- **Interfaces and traits**: declare them where they are consumed, keep them to 1–3 methods, and add one only when two implementations exist today. Return concrete types.
- **Concurrency**: every concurrent task has an owner and an exit path. The sender closes channels. Shared state sits behind a lock or inside one owning task. Channels are bounded.
- **Packages and modules**: split by responsibility, not by layer. No `util`, `common` or `helpers`. Keep non-public code private. Don't add a package or crate for one file's worth of code.
- **State**: no global mutable state, no import-time side effects. Pass config explicitly.
- **I/O**: every external call has a timeout. Cap untrusted input sizes. Close what you open. SQL is parameterized only, and a transaction that isn't committed is rolled back.
- **Money**: never binary floating point for prices, quantities, fees or P&L. Use decimal or fixed-point types end to end.
- **Logging**: use the repo's logger with structured keys only. No secrets or PII.
- **Generics**: only when two or more concrete types need it today.
- **Dependencies**: a new dependency needs an ADR line covering why the standard library or the existing deps won't do, plus license and maintenance. Lock files are changed by the tool, never by hand.
- **Docs**: every public identifier has a doc comment.

## Tests (all languages)

- The test name carries the AC id it proves. The exact form for each language is in its rules file.
- Table-driven or parametrized. Run in parallel when there is no shared state. Use the framework's temp dirs and env helpers.
- Hand-written fakes over mock frameworks. Use fake servers for HTTP and WebSocket peers.
- Never sleep to synchronize: control time or wait on a signal.
- Test behavior through the public API unless internals are the point.
- Parsers and decoders of untrusted input get a fuzz or property test.

## Never

Never push, merge, tag or force anything. Never weaken a test or a check to get green. Never commit secrets. Never add a dependency without an ADR.
