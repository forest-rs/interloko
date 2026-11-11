# Interloko Small-Program Corpus

This directory contains small `.loko` programs used by `tools/interloko_corpus`.
Each program declares its expectations in leading comments:

```text
// expect: run 42
// expect: diagnostics 0
// expect: diagnostic contains unresolved local
// expect: dump lowered contains i64.add
```

Supported expectations:

- `run VALUE`: compile and run zero-argument `main`, expecting the scalar
  `VALUE` written as Loko literal text: `42` (integer), `42.0` or `4.2e1`
  (float; also `inf`, `-inf`, `nan`), `#t`/`#f` (boolean), or `"text"`
  (string, with `\n`, `\t`, `\"`, and `\\` escapes). The literal form names
  the type, so `run 42` does not match a string `"42"` or a float `42.0`.
  Floats match by exact value: `run nan` matches any NaN and `run -0.0`
  does not match `0.0`.
- `diagnostics N`: expect exactly `N` diagnostics.
- `diagnostic contains TEXT`: expect at least one diagnostic whose message
  contains `TEXT`.
- `dump STAGE contains TEXT`: render `STAGE` and expect it to contain `TEXT`.
  Stages are `syntax`, `ast`, `sem`, `mop`, `eop`, `lowered`, `tape`, `typing`, and
  `all`.
- `// host-class: <name> slot:<type>`: declare host-backed metadata for
  compile/dump corpus cases. This is a corpus runner header, not Loko syntax.
- `// grammar: base-loko|derived-host`: select the grammar environment used by
  the session. `derived-host` is a trusted fixture that extends the base grammar
  for MOP/trace inspection while delegating parsing to the base parser.

Use `corpus/pass` for programs that should compile and run cleanly. Use
`corpus/fail` for programs that intentionally exercise diagnostics.
