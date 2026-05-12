---
id: cai-240w
status: closed
deps: []
links: []
created: 2026-05-12T02:25:17Z
type: feature
priority: 1
assignee: Bruce Mitchener
tags: [compiler, conditions, restarts]
---
# Bind condition object in restart handlers

Allow restart handlers to bind the signaled condition object, e.g. with restart e :: <invalid-radius> e.fallback end, and thread that binding through AST, semantic IR, lowering, dumps, corpus, and tests.

## Acceptance Criteria

fmt, clippy -D warnings, workspace tests, docs, examples, cairn_lab tests, cairn_corpus pass


## Notes

**2026-05-12T02:34:45Z**

Implemented optional restart condition bindings, scoped semantic locals, lowering move on the signal edge, dumps/fingerprints, corpus coverage, and regression tests. Verified fmt, clippy -D warnings, workspace tests, docs, examples, and corpus.
