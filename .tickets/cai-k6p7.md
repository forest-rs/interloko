---
id: cai-k6p7
status: open
deps: []
links: []
created: 2026-05-15T18:05:49Z
type: task
priority: 2
assignee: Bruce Mitchener
tags: [grammar, capabilities, deferred]
---
# Define public grammar registry capability policy

Decide how non-builtin grammar environments and staged adapters are registered, selected, trusted, and exposed without making every host adapter implicitly available to source files.

## Design

The registry should remain explicit session/environment input. Future public registration needs capability facts, stable EOP projection, deterministic selection, and clear denial diagnostics.

## Acceptance Criteria

A design note defines registry ownership, adapter trust levels, source/session selection rules, and diagnostic codes for unavailable or denied grammars.

