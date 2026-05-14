---
id: cai-uv89
status: closed
deps: []
links: [cai-f4sj]
created: 2026-05-11T15:25:36Z
type: task
priority: 1
assignee: Bruce Mitchener
tags: [compiler, lowering, tape]
---
# Add lowered executable IR and dumps

Implement Interloko lowered executable IR for constants, locals, calls, generic dispatch, object construction, slot reads, arithmetic, and make execution_tape emission consume lowered IR.

## Design

lower owns backend-neutral executable instructions and inspectable dumps; tape owns only the adapter from lowered instructions to execution_tape ordinary calls/host calls.

## Acceptance Criteria

Lowered IR is public and documented; driver exposes lowered and tape dumps; tape emission no longer walks semantic expressions directly; fmt, clippy, tests, docs pass.


## Notes

**2026-05-11T15:35:36Z**

Implemented lowered executable IR, lowered and tape dumps, and execution_tape emission from lowered IR. Validation: fmt, clippy -D warnings, workspace tests, docs, forty-two example, generic-area example.
