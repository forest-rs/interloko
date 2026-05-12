---
id: cai-kxeb
status: closed
deps: []
links: []
created: 2026-05-11T14:58:35Z
type: task
priority: 1
assignee: Bruce Mitchener
---
# Emit tape runtime dispatch for generic calls

Lower semantic generic calls to ordinary execution_tape calls through generated dispatcher functions, using host calls only for runtime object construction, class lookup, and slot reads.

## Design

Use execution_tape call/br/host_call opcodes; do not add generic opcodes. Runtime host owns object heap; generated dispatchers consult runtime class IDs and call method bodies.

## Acceptance Criteria

Tape and driver tests compile and run area(make(<circle>, radius: 10)) to a deterministic i64 result.

