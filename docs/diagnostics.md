# Structured Diagnostics

## First Read

Interloko diagnostics are facts first. Prose belongs to catalog-backed
presentation tools.

A diagnostic has a stable code, severity, primary span, source labels,
structured arguments, and optional notes. `interloko_compiler` does not store a
rendered English message on `Diagnostic`.

Example:

```text
code=sem.missing-required-init-keyword
severity=Error
span=...
args=[keyword=r:,slot=radius]
```

The workbench exposes those facts through EOP records:

```sh
cargo run -p interloko_lab -- inspect tools/interloko_lab/fixtures/diagnostic_unknown_local.loko diagnostic:d0
```

The output includes the diagnostic code, labels, and args. It deliberately does
not include rendered text, so EOP records and artifact fingerprints do not
change when the selected language or catalog changes.

`interloko_lab` renders command-line diagnostics through the built-in
MessageFormat catalogs in `tools/interloko_diagnostics/diagnostics/`. The catalog
entry ID is the diagnostic code:

```toml
[messages]
"sem.unresolved-local" = "unresolved local { $local }"
```

Rendering is strict: if a catalog has no entry for a code, if an entry refers
to an argument the diagnostic did not provide, or if MessageFormat formatting
fails, the renderer returns an error. `interloko_lab` reports that error instead of
silently falling back to English.

Select a built-in diagnostic locale with:

```sh
INTERLOKO_DIAGNOSTIC_LOCALE=pseudo cargo run -p interloko_lab -- run program.loko
```

Load an override catalog from a MessageFormat resource TOML file with:

```sh
INTERLOKO_DIAGNOSTIC_CATALOG=path/to/catalog.mf2.toml \
  cargo run -p interloko_lab -- run program.loko
```

## Second Read

`interloko_compiler::diagnostic` owns the core diagnostic model:

- `DiagnosticCode` is the stable identity for tests, corpus expectations, EOP
  records, and localized renderers.
- `DiagnosticArg` carries named values such as `class`, `slot`, `keyword`,
  `expected`, `actual`, and counts.
- `DiagnosticLabel` carries primary or secondary source spans.

This slice intentionally keeps `message-format` out of `interloko_compiler`.
Diagnostic production does not know about locale selection, catalog loading,
UI rendering, terminal color, or LSP adaptation.

The MF2 integration point is a renderer/catalog layer outside the compiler
core:

```text
interloko_compiler
  Diagnostic { code, args, labels, notes }

interloko_diagnostics
  MessageFormat catalog loading
  DiagnosticCode + DiagnosticArg -> localized text

workspace UI / LSP / EOP tools
  choose locale, load catalogs, render snippets, add presentation
```

In that model, a catalog key is the stable diagnostic code, and diagnostic
args become MF2 variables. The current catalog source uses MessageFormat
resource TOML:

```toml
[messages]
"sem.missing-required-init-keyword" =
  "missing required init keyword { $keyword } for slot { $slot }"
```

Tools can add or override a diagnostic catalog by compiling their own resource
TOML with `interloko_diagnostics::DiagnosticCatalog::from_toml_source` and passing
it to `DiagnosticRenderer::new`. Override catalogs do not change diagnostics,
artifact identity, or cache behavior; they only change presentation text. A
partial override can omit entries only if the caller is prepared for rendering
to fail when an omitted diagnostic is emitted.

Artifact identity must stay language-neutral. Fingerprints and cache keys mix
diagnostic codes, spans, labels, and args, but not rendered catalog output.
That keeps incremental reuse stable when a user switches from `en` to
`pseudo`, or from a built-in catalog to a project-specific override catalog.

## Sealing

Sealing has diagnostics where a definition contradicts itself, and refusals
where something from outside the defining source tries to extend it:

| Code or refusal | When |
| --- | --- |
| `ast.duplicate-adjective` | a definition repeats an adjective, as in `define sealed sealed class` |
| `ast.conflicting-adjectives` | a definition has two that contradict each other: `sealed` and `open`, or `abstract` and `concrete` (args `adjective`, `other`) |
| `sem.abstract-class-make` | `make` names an abstract class, which has no direct instances (arg `class`); a class value naming one at run time traps instead |
| `MutationError::SealedGeneric` | an `add-method` session mutation targets a sealed generic, or a replayed one does after the new source sealed it |
| `CodeMismatch::Assumption` | code installed under live frames would break a closed-world assumption the running code relies on: a sealed class gained or lost a direct subclass, changed abstractness or was unsealed, or an assumed method set changed |

There is one compilation unit, so a source cannot violate its own sealing:
every subclass and method it defines is part of the sealed set. Adjectives
belong to the definitions that take them (classes take all four, generics
`sealed` and `open`), and any other placement is a syntax error.

## Runtime Type Errors

A runtime check that fails does not trap. It signals a `<type-error>`
condition, and its stable code follows the same `area.name` scheme, with
the condition's class as the area. The code names what kind of check
failed:

| Code | Check |
| --- | --- |
| `type-error.cast` | a value is not an instance of the class an `as` (or a boxed `<object>` read as a number) expects |
| `type-error.entry` | an object argument passed through an untyped `<callable>` is not of its parameter's class, an exit procedure's value is not of its block's class, or a `use-value` replacing a failed send's result is not of its generic's result class (a scalar of the wrong representation still traps; see [`eop_workbench.md`](eop_workbench.md#runtime-type-errors)) |
| `type-error.range` | a number is outside a limited type's range: a conversion to `<i32>`/`<u32>`, a float converted to an integer it does not equal, a shift count outside `0..=31`, or an argument, binding, slot value, constant or result outside its `limited(<integer>, ...)` type |
| `type-error.overflow` | integer arithmetic produced a result its type cannot represent |

`interloko_model::condition::TypeErrorKind` owns the codes, and
`DiagnosticCode` mirrors them (`DiagnosticCode::TypeErrorCast`, ...). A
condition carries its code in its `type-error-code` slot, beside the
rejected value, the printed expected type, and the check's site:

- `typing:<definition>:s<n>`, the EOP id of the typing decision that
  classified the check, which `interloko_lab inspect` and the workbench
  resolve;
- `entry:<function>:<index>` for an entry check typing does not classify:
  source parameter `index` (from zero) of the lowered function, such as
  `entry:exit#3:0` for a shared exit procedure's value, `entry:closure#2:0`,
  or a callable thunk such as `entry:radius#closure:0`;
- `check:<definition>@<start>..<end>` for a check typing does not
  classify, by the definition whose code makes it and the check's span,
  such as a dispatcher coercing an argument it has already selected a
  method for, or checking a `use-value` that replaces a failed send's
  results (at the generic's span);
- `check@<start>..<end>` for the same in generated code no definition
  owns.

When nothing handles a failed check's `<type-error>`, the run ends with
the type error's own code rather than `runtime.unhandled-condition`, and
the rendered outcome adds what it reports (a `<type-error>` the program
signals itself is an ordinary unhandled condition):

```text
unhandled condition <type-error>: type-error.cast: {<square>} is not of type <circle> at typing:def…:s0
```

## Failed Sends

A generic send whose arguments no method accepts does not trap either. It
signals a `<no-applicable-method>` whose code is
`dispatch.no-applicable-method` (`interloko_model::condition::NO_APPLICABLE_METHOD_CODE`,
mirrored as `DiagnosticCode::DispatchNoApplicableMethod`), in its
`no-applicable-method-code` slot beside the generic and the arguments. It
is the runtime counterpart of the advisory
`typing.dispatch-no-applicable-method`, which soft typing reports where it
can tell statically that a send always fails. Unhandled, the run ends with
that code and the rendered outcome names the generic and the classes and
values of the arguments:

```text
unhandled condition <no-applicable-method>: dispatch.no-applicable-method: no method of area applies to (<square>), called with ({<square>})
```

There is no code for an ambiguous send: methods that could be equally
specific for some arguments are rejected when the module compiles
(`sem.ambiguous-generic-method-dispatch`). The tape trap code a failed send
used, 1, is retired with those of the failed type checks; `make` on a class
value still traps (`runtime.trap`) when the class needs an init keyword the
call cannot pass, which is not a dispatch failure.

## Corpus

The corpus runner supports both forms:

```loko
// expect: diagnostic code sem.unresolved-local
// expect: diagnostic contains unresolved local `missing`
```

Code expectations are preferred for behavior. String expectations remain useful
as compatibility checks for the default English catalog. A run's outcome is
matched by its code too, as in `// expect: runtime code type-error.overflow`.

## Glossary

- **Diagnostic code:** stable identity such as
  `sem.slot-initializer-type-mismatch`.
- **Diagnostic arg:** named structured value used by tools and renderers.
- **Label:** source span attached to a diagnostic.
- **Rendered message:** catalog-produced text for humans.
- **MF2 renderer:** adapter that formats diagnostics through
  MessageFormat 2 catalogs.
- **Catalog:** MessageFormat resource whose message IDs are diagnostic codes.

## Deferred

- Full localization catalogs beyond the built-in English and pseudo catalogs.
- A `message-format` dependency in `interloko_compiler`; it should remain out of
  the compiler core.
- Terminal snippet and color rendering.
- LSP diagnostic adaptation.
- Fix-it edits and suggestions.
- Warning levels beyond the current error-only severity model.
