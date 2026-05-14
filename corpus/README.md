# Cairn Small-Program Corpus

This directory contains small `.cairn` programs used by `tools/interloko_corpus`.
Each program declares its expectations in leading comments:

```text
// expect: run 42
// expect: diagnostics 0
// expect: diagnostic contains unresolved local
// expect: dump lowered contains i64.add
```

Supported expectations:

- `run N`: compile and run zero-argument `main`, expecting integer `N`.
- `diagnostics N`: expect exactly `N` diagnostics.
- `diagnostic contains TEXT`: expect at least one diagnostic whose message
  contains `TEXT`.
- `dump STAGE contains TEXT`: render `STAGE` and expect it to contain `TEXT`.
  Stages are `syntax`, `ast`, `sem`, `mop`, `eop`, `lowered`, `tape`, and
  `all`.
- `// host-class: <name> slot:<type>`: declare host-backed metadata for
  compile/dump corpus cases. This is a corpus runner header, not Cairn syntax.

Use `corpus/pass` for programs that should compile and run cleanly. Use
`corpus/fail` for programs that intentionally exercise diagnostics.
