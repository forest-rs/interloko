---
id: cai-ki3x
status: open
deps: []
links: []
created: 2026-05-11T14:49:13Z
type: task
priority: 2
assignee: Bruce Mitchener
---
# Add query invalidation shell after milestone 0

Introduce a thin query/cache shell around the proven syntax, AST, semantic, and tape stages using invalidation 0.2, without making invalidation own recomputation or scheduling.

## Design

Keep stage functions pure where practical. Let Cairn own cached artifacts and use invalidation only for dependency-aware dirty/affected ordering.

## Acceptance Criteria

A small source edit can mark the smallest known affected compiler artifacts dirty; tests cover direct file/function dependency invalidation.

