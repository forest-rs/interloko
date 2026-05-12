---
id: cai-8lcw
status: open
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

