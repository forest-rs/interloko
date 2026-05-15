---
id: cai-bq64
status: open
deps: []
links: []
created: 2026-05-15T18:05:43Z
type: task
priority: 2
assignee: Bruce Mitchener
tags: [grammar, diagnostics, deferred]
---
# Improve original-source span remapping for staged forms

Replace approximate staged-region spans with precise mappings from generated AST/frontend objects back to the original source fragments that produced them.

## Design

Reports and diagnostics should be able to link source text, frontend forms, AST definitions, semantic objects, and generated debug text without relying on materialized-source coordinates.

## Acceptance Criteria

A failing staged-source diagnostic points at the original staged source span, and the report relation data can cross-link generated objects to the exact source fragment.

