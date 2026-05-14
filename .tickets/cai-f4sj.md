---
id: cai-f4sj
status: closed
deps: []
links: [cai-uv89]
created: 2026-05-11T14:49:13Z
type: task
priority: 3
assignee: Bruce Mitchener
---
# Split lowered IR from tape emission

Introduce the backend-neutral lowered executable IR from DESIGN.md once the direct semantic-to-tape path starts accumulating backend-specific logic.

## Design

Keep execution_tape emission as an adapter over lowered IR. Do not expose high-level Interloko semantics through tape opcodes.

## Acceptance Criteria

A lowered IR dump exists, tape emission consumes it, and existing milestone-0 programs still run through main() => 42.


## Notes

**2026-05-11T15:35:39Z**

Implemented by cai-uv89: lower::Module now owns backend-neutral executable IR and tape::emit consumes lowered IR rather than semantic expressions.
