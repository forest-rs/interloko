---
id: cai-mwkg
status: closed
deps: []
links: []
created: 2026-05-11T14:58:35Z
type: task
priority: 1
assignee: Bruce Mitchener
---
# Resolve nominal classes and generic methods

Add semantic IDs for classes, slots, generics, and methods; resolve object construction, slot reads, direct calls, and generic calls.

## Design

Semantic IR owns language meaning. Tape remains backend adapter. Generic calls remain generic in semantic IR until tape emits dispatcher functions.

## Acceptance Criteria

Semantic tests cover ClassId, SlotId, GenericFunctionId, MethodId, GenericCall resolution, DirectCall preservation, and diagnostics for missing slots/methods/classes.

