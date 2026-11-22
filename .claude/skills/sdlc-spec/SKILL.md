---
name: sdlc-spec
description: Turn a ticket or feature request into a one-page spec with testable acceptance criteria in docs/specs/. Use when starting a feature, when asked to write or refine a spec/requirements, or when /sdlc reaches the spec phase.
argument-hint: "[ticket text | issue URL | spec path to revise]"
---

# Write the spec

Input: $ARGUMENTS

## Steps

1. Read the ticket and the code it touches. Find every caller, handler, schema, config and test the change could reach.
2. If an ambiguity would change an acceptance criterion, ask, at most 3 questions, in one message. Otherwise write the assumption into the spec and move on.
3. Next number: highest `docs/specs/NNNN-*.md` + 1, zero-padded to 4 digits (start at `0001`). Slug: kebab-case, ≤5 words.
4. Write the spec with the template below. One page; cut, don't shrink.
5. Stop at the gate: `Approve spec NNNN? (yes / changes)`. On "yes", set `Status: Approved`.

## Template

```markdown
# NNNN: <title>

Status: Draft
Ticket: <link or "none">

## Problem
<1-3 sentences: who is hurt, how, and how we know.>

## Non-goals
- <What we are deliberately not doing. YAGNI lives here.>

## Acceptance criteria
- **AC-1** Given <state>, when <action>, then <observable result>.
- **AC-2** ...
<Each one is testable by an automated test or a command. No "should be fast"; give numbers.>

## Interfaces touched
<Public API (Go exported identifiers, Rust `pub` items), HTTP/gRPC endpoints and proto, DB schema, events, config, CLI flags. "None" is a valid answer.>

## Risks
- Data loss or corruption:
- Exposure (authz, PII, secrets):
- Breaking callers (API, wire format, schema):
- Rollout / rollback:

## Assumptions and open questions
- ...
```

## Quality bar

- Every AC is binary: a reviewer can say met or not met.
- Error paths get ACs too (invalid input, dependency down, timeout, duplicate).
- An empty Risks bullet means "checked, none" only if you actually checked it.
