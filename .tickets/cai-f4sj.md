---
id: cai-f4sj
status: open
deps: []
links: []
created: 2026-05-11T14:49:13Z
type: task
priority: 3
assignee: Bruce Mitchener
---
# Split lowered IR from tape emission

Introduce the backend-neutral lowered executable IR from DESIGN.md once the direct semantic-to-tape path starts accumulating backend-specific logic.

## Design

Keep execution_tape emission as an adapter over lowered IR. Do not expose high-level Cairn semantics through tape opcodes.

## Acceptance Criteria

A lowered IR dump exists, tape emission consumes it, and existing milestone-0 programs still run through main() => 42.

