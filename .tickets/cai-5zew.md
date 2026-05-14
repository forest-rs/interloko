---
id: cai-5zew
status: closed
deps: []
links: []
created: 2026-05-12T00:23:49Z
type: feature
priority: 2
assignee: Bruce Mitchener
---
# Build small-program corpus runner

Add a small-program corpus plus a workspace runner that validates expected runtime results, diagnostics, and dump stability through the session API.

## Acceptance Criteria

Corpus covers arithmetic, locals, direct calls, classes, slots, object construction, generic dispatch, expected diagnostics, and dump expectations. Runner and tests pass with full workspace gates.


## Notes

**2026-05-12T00:28:53Z**

Implemented corpus fixtures, expectation header parser, interloko_corpus check/list/dump commands, runner tests, and progress notes. Full fmt/clippy/test/doc/examples/workbench/corpus validation passed locally.
