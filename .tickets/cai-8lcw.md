---
id: cai-8lcw
status: closed
deps: []
links: []
created: 2026-05-11T14:49:13Z
type: task
priority: 2
assignee: Bruce Mitchener
---
# Define stable compiler ID strategy

Decide how SourceFileId, SyntaxNodeId, AstNodeId, FunctionId, LocalId, and SpanId should remain stable across edits before deeper incremental compilation work.

## Design

Prefer explicit IDs and side tables over implicit positions. Document which IDs are durable across reparses and which are per-compilation-session handles.

## Acceptance Criteria

A crate-local ADR or design note defines stability guarantees and tests cover ID preservation for a small edit.


## Notes

**2026-05-11T16:15:42Z**

Implemented stable-ish source, syntax, AST, definition, and artifact identities. Added crate ADR and tests for whitespace/comment edit preservation plus method artifact key preservation across method renumbering. Validation: fmt, clippy -D warnings, workspace tests, docs, examples.
