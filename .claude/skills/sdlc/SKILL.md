---
name: sdlc
description: Drive a ticket through the team SDLC (spec, design, implement, review, ship) with human gates. Use when the user runs /sdlc or asks to take a ticket, issue or spec end to end.
argument-hint: "[ticket text | issue URL | docs/specs/NNNN-x.md]"
disable-model-invocation: true
---

# /sdlc

Ticket: $ARGUMENTS

Follow `.claude/SDLC.md`. This skill is only the router.

## 1. Find where we are

Check the artifacts, not memory:

- Argument is a spec path, or a spec under `docs/specs/` matches the ticket → read its `Status:` and resume at the first unfinished phase.
- No spec → classify the ticket as **Tiny**, **Bug** or **Feature** (table in SDLC.md). Say which and why in one line. If unsure between Bug and Feature, pick Feature.

## 2. Run the phases for that track, in order

| Phase     | Skill             | Done when                                              |
|-----------|-------------------|--------------------------------------------------------|
| spec      | `/sdlc-spec`      | Human approved → `Status: Approved`                    |
| design    | `/sdlc-design`    | Skipped with a reason, or ADR approved by a human      |
| implement | `/sdlc-implement` | Every AC has a passing test and `.claude/check.sh` is green |
| review    | `/sdlc-review`    | `VERDICT: PASS`                                        |
| ship      | `/sdlc-ship`      | PR text ready; human pushes                            |

Rules:
- At a human gate, **stop and wait**. End your message with the exact question, e.g. `Approve spec 0007? (yes / changes)`.
- Review FAIL → fix the blockers with `/sdlc-implement`, then review again. After the second FAIL, stop and hand it to a human with the findings.
- Requirements change mid-flight → go back to spec, update it, and get it re-approved.

## 3. Report

Each message starts with one line: `[sdlc] <track> · <phase> · next gate: <gate>`.
