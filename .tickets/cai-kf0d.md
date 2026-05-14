---
id: cai-kf0d
status: closed
deps: []
links: []
created: 2026-05-12T02:54:37Z
type: feature
priority: 1
assignee: Bruce Mitchener
tags: [compiler, objects, runtime]
---
# Build richer slot type model

Add explicit slot type declarations for primitive integer, primitive boolean, and nominal object slots; thread slot kinds through AST, semantic IR, lowering, tape/runtime host calls, dumps, corpus, incremental tests, and docs while keeping objects runtime-managed.

## Acceptance Criteria

fmt, clippy -D warnings, workspace tests, docs, examples, interloko_lab tests, and interloko_corpus pass; progress notes document slot typing semantics and deferred native aggregate design


## Notes

**2026-05-12T03:11:15Z**

Implemented explicit slot types for integer, boolean, and nominal object slots; initializer checking; typed slot-read semantic/lowered/tape flow; mixed Value runtime storage; dumps/fingerprints; corpus and incremental tests. Verified fmt, clippy -D warnings, workspace tests, docs, examples, and corpus.
