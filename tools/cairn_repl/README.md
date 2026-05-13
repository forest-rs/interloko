# cairn_repl

`cairn_repl` is a small plain-stdio REPL over the existing Cairn compiler
session pipeline. It keeps one durable in-memory source buffer for definitions,
then evaluates expression input by compiling that durable source plus one
generated zero-argument scratch function.

Run it with:

```sh
cargo run -p cairn_repl
```

Definitions are entered as multiline `define ... end;` forms:

```cairn
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
`cairn_diagnostics`, using the same `CAIRN_DIAGNOSTIC_LOCALE` and
`CAIRN_DIAGNOSTIC_CATALOG` environment variables as `cairn_lab`.

Commands:

```text
:help
:quit
:source
:clear
:dump [syntax|ast|sem|mop|eop|lowered|tape|all]
:mop
:eop
:artifacts
:dispatch [dispatch-site-id]
:inspect <object-id>
:affordances <object-id>
```

Current limits:

- no top-level mutation;
- no modules, macros, persistent image, history, or completion;
- expression evaluation currently prints integer results and reports
  non-integer runtime results as runtime errors;
- host-backed declarations are not parsed from REPL comments yet; use
  `cairn_lab` for file-based host registry examples.

The REPL is also a natural future integration point for Portolan-style
affordances and intents. This crate currently exposes Cairn EOP/MOP objects and
their affordance names directly; a Portolan bridge should consume those records
from the compiler/session layer rather than making the REPL own the action
model.
