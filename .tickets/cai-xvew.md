---
id: cai-xvew
status: closed
deps: []
links: []
created: 2026-05-12T02:06:48Z
type: feature
priority: 1
assignee: Bruce Mitchener
tags: [compiler, conditions, restarts]
---
# Add recoverable condition restart slice

Add ordinary-class condition signaling, lexical handle/restart recovery, semantic IR, lowered/tape support, corpus cases, incremental tests, and progress notes for Interloko's first recoverable condition slice.

## Acceptance Criteria

fmt, clippy -D warnings, workspace tests, docs, examples, interloko_lab tests, interloko_corpus pass


## Notes

**2026-05-12T02:16:48Z**

Implemented lexical condition/restart slice with ordinary condition classes, signal(make(...)), handle/restart syntax, AST and semantic IR, lowered lexical restart frames, tape execution through existing instructions, session dumps/fingerprints, corpus pass/fail cases, and incremental restart-handler edit coverage. Validation passed: fmt, clippy -D warnings, workspace tests, docs, examples, interloko_lab dump all, interloko_corpus.
