---
id: cai-6adc
status: closed
deps: []
links: []
created: 2026-05-11T14:58:35Z
type: task
priority: 1
assignee: Bruce Mitchener
---
# Track generic dispatch invalidation queries

Add a thin invalidation/query shell using invalidation 0.2 for parse, bind, method lookup, lowering, and tape emission artifacts.

## Design

Cairn owns artifacts and scheduling. invalidation only models dependency edges and affected ordering.

## Acceptance Criteria

Tests show that changing one method invalidates that method lookup/lowered/tape artifacts without invalidating unrelated method artifacts.

