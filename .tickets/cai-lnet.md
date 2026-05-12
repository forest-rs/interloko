---
id: cai-lnet
status: closed
deps: []
links: []
created: 2026-05-12T03:27:58Z
type: feature
priority: 1
assignee: Bruce Mitchener
tags: [compiler, slots, dylan]
---
# Make slots Dylan-shaped

Implement Dylan-shaped slot defaults, top <object> value semantics, init-keyword metadata, required init keywords, construction diagnostics, dumps, corpus, and incremental coverage.

## Acceptance Criteria

Untyped slots default to <object>; <object> is distinct from nominal object handles; runtime/tape can store and read top-value slots; init-keyword and required-init-keyword work; construction diagnostics cover missing, unknown, duplicate, and type mismatch; dumps/corpus/incremental/docs updated; fmt/clippy/tests/docs/corpus pass.


## Notes

**2026-05-12T03:42:28Z**

Implemented Dylan-shaped slot defaults and init keywords. Untyped slots now resolve to top <object> ValueKind::Any, distinct from nominal Object(ClassId). execution_tape lowering boxes top values through cairn.box_value/CAIRN_VALUE_TYPE. Added required-init-keyword/init-keyword diagnostics, dumps/fingerprints, corpus cases, and incremental coverage. Gates run: fmt, clippy, workspace tests, docs, examples, corpus.
