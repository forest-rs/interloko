---
id: cai-4xsj
status: open
deps: []
links: []
created: 2026-05-15T18:05:57Z
type: feature
priority: 3
assignee: Bruce Mitchener
tags: [lab, report, deferred]
---
# Plan live grammar report workbench

Turn the static HTML report shape into an interactive workbench later, with edit sessions and incremental refresh instead of one-shot generated files.

## Design

Reuse session artifacts and relation data. Keep the live tool separate from compiler semantics; it needs browser RPC, incremental updates, persistent view state, and better graph layout.

## Acceptance Criteria

A future goal identifies the UI/runtime crate boundary, transport, data model, and first live interaction before implementation starts.

