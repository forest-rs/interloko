# interloko_repl

`interloko_repl` is a small REPL over the existing Interloko compiler session pipeline.
Interactive terminal sessions use `reedline` for line editing, local in-memory
history, Ctrl-C/Ctrl-D handling, and completions. Piped input keeps the plain
stdio transcript mode used by tests and scripts.

The REPL keeps an in-memory source buffer of accepted definitions. Aggregate
source is rendered from those entries for compilation, `:source`, and expression
evaluation with one generated zero-argument scratch function.

Run it with:

```sh
cargo run -p interloko_repl
```

Definitions are entered as multiline `define ... end;` forms:

```loko
define function square(x)
  x * x
end;
```

Expression input is evaluated through the normal source -> syntax -> AST ->
semantic IR -> lowered IR -> tape pipeline:

```text
square(5)
```

Integer results print directly. Diagnostics are rendered through
`interloko_diagnostics`, using the same `INTERLOKO_DIAGNOSTIC_LOCALE` and
`INTERLOKO_DIAGNOSTIC_CATALOG` environment variables as `interloko_lab`.

Interactive controls:

- Up/Down browse the local history for this process.
- Tab completes colon commands, `:dump` stages, dispatch IDs, and object IDs
  from the most recent artifacts.
- Input is highlighted in the editor: commands, Interloko keywords, class names,
  literals, comments, punctuation, and unknown tokens get distinct styles.
- Ctrl-C cancels the current line, and clears a pending multiline definition.
- Ctrl-D exits.
- ANSI color is used only for interactive TTY output.

Commands:

```text
:help
:quit
:source
:forms
:run <zero-arg-function>
:reset
:clear
:cancel
:clear-pending
:pending
:dump [syntax|ast|sem|mop|eop|lowered|tape|all]
:mop
:eop
:artifacts
:dispatch [dispatch-site-id]
:inspect <object-id>
:affordances <object-id>
```

`:source` prints the rendered accepted source with line numbers. `:forms` lists
the top-level forms that have been accepted into the source buffer. `:reset`
clears accepted definitions and cached artifacts; `:clear` is kept as an alias.
`:pending` shows an unfinished multiline definition. `:cancel` and
`:clear-pending` discard it without changing accepted source.

Current limits:

- no top-level mutation;
- no modules, macros, persistent image, or persistent history;
- expression evaluation currently prints integer results and reports
  non-integer runtime results as runtime errors;
- host-backed declarations are not parsed from REPL comments yet; use
  `interloko_lab` for file-based host registry examples.

The REPL is also a natural future integration point for Portolan-style
affordances and intents. This crate currently exposes Interloko EOP/MOP objects and
their affordance names directly; a Portolan bridge should consume those records
from the compiler/session layer rather than making the REPL own the action
model.
