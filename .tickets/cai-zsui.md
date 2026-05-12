---
id: cai-zsui
status: closed
deps: []
links: []
created: 2026-05-11T14:58:35Z
type: task
priority: 1
assignee: Bruce Mitchener
---
# Parse class generic and method syntax

Extend the milestone-0 parser and AST for nominal classes with slots, generic declarations, methods specialized on nominal classes, object construction, keyword slot initialization, and slot reads.

## Design

Keep syntax tree lossless-enough and lower to AST forms before semantic IDs. Do not add modules, macros, parametric types, or full Dylan grammar.

## Acceptance Criteria

Parser and AST tests cover class definitions, generic declarations, specialized methods, make(<class>, slot: value), and receiver.slot reads.

