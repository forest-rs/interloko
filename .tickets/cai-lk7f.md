---
id: cai-lk7f
status: closed
deps: []
links: []
created: 2026-05-11T16:21:22Z
type: feature
priority: 1
assignee: Bruce Mitchener
tags: [session, workbench, cli, invalidation]
---
# Build compiler session and workbench CLI

Build Interloko's incremental compiler workbench and session API with named source files, per-stage artifacts, stable artifact keys, dumps, diff reporting, fixture tests, and validation gates.

## Design

Session owns named source files, per-stage artifact snapshots, artifact-key reporting, dumps, and edit comparisons. The CLI is a thin std adapter over the no_std compiler/session API.

## Acceptance Criteria

Session API exposes syntax, AST, semantic IR, lowered IR, tape, diagnostics, runtime results, artifact keys, and dumps. tools/interloko_lab supports run, dump, artifacts, and diff. Fixtures prove harmless edits preserve IDs and focused method edits affect expected artifacts. fmt, clippy -D warnings, tests, docs, examples, and CLI fixture tests pass.


## Notes

**2026-05-11T16:29:13Z**

Implemented session API, stage dumps, artifact graph, session diff, tools/interloko_lab CLI, fixtures, and CLI/session tests. Validation: fmt, clippy -D warnings, workspace tests, docs, existing examples, and explicit CLI run/dump/artifacts/diff commands.
