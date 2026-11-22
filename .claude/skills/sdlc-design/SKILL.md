---
name: sdlc-design
description: Decide whether an approved spec needs a design, and if so write an ADR in docs/adr/ for package/crate layout, interfaces, concurrency, data and dependencies in any language the harness supports. Use after spec approval, when asked for a design/ADR/architecture, or when /sdlc reaches the design phase.
argument-hint: "[docs/specs/NNNN-x.md]"
---

# Design (only when needed)

Spec: $ARGUMENTS

## 1. Does this need a design?

Design is required if **any** of these hold. Otherwise reply `Design skipped: <reason>` and stop:

- It adds a package, crate or module, a dependency, or a concurrent task (goroutine, async task, worker, background job)
- It changes an exported API, a wire format (HTTP/gRPC/events), or a DB schema
- It crosses a trust boundary (authn/z, external input, secrets)
- The approved spec lists more than one plausible approach

## 2. Write the ADR

Path: `docs/adr/NNNN-slug.md` (next number, as for specs). Keep it to one page:

```markdown
# ADR NNNN: <decision in a few words>

Status: Proposed
Spec: docs/specs/NNNN-x.md

## Context
<Forces and constraints. Link code by path.>

## Options
1. **Smallest change**: <reuse existing code / standard library>. Cost, risk.
2. <Alternative>. Cost, risk.
<Option 1 is always listed. Pick another only with a concrete reason.>

## Decision
<What and why, in plain words.>

## Shape
- Packages, crates and files touched/added:
- New/changed types and interfaces or traits (consumer-side, 1-3 methods):
- Concurrency: who starts each task, who stops it, how cancellation reaches it:
- Errors: sentinels, types or enum variants callers branch on:
- Data: migrations as expand → migrate → contract; backward compatible at every step:
- New dependencies: module or crate, license, why the standard library or existing deps won't do:
- Observability: logs/metrics/traces using what the repo already has:

## Consequences and rollback
<What gets harder. How to undo it in production.>
```

## 3. Gate

Stop: `Approve ADR NNNN? (yes / changes)`. On "yes", set `Status: Accepted`.
