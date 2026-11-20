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
`sealed`, `open` and `strict`, and functions, methods and constants
`strict`), and any other placement is a syntax error.

## Strict Typing

Soft typing's findings are advice: `typing.*-runtime-checked` codes are
silent by default and the checks stay in the code. A definition declared
`strict` (or a method of a `strict` generic, or every definition under the
`strict` typing policy) must instead prove the checks the compiler inserts
into it, and each one typing cannot prove is an error:

| Code | When |
| --- | --- |
| `typing.unproved-dispatch` | a send may find no method for its arguments' runtime classes |
| `typing.unproved-cast` | a cast the compiler inserted may fail, such as the check of a value returned through an untyped `<callable>` |
| `typing.unproved-range` | a value reaching a `limited(<integer>, ...)` declaration (an argument, binding, slot, constant or result) may be out of range |
| `typing.unproved-overflow` | integer arithmetic may overflow, only under `strict.overflow=proved` |

A check that always fails is an error in strict code too, with its soft
code (`typing.cast-always-fails`, `typing.dispatch-no-applicable-method`,
...); in soft code it stays a warning. An explicit `as`, an `instance?`
test, and the check an indirect entry makes on what it is passed are not
strict obligations: the first is deliberate, the second never fails, and
the third guards the body against callers typing cannot see and blames
them.

Every typing finding carries the `decision` arg, the EOP id
`typing:<definition>:s<n>` of the decision it reports, beside `expected`
and `actual` (or `generic` and `arguments`, and `operation`) and the
`rule`. A strict error adds `definition` (such as `function twice`) and
`strict`, why it is strict (`declared`, `generic area is strict`, or
`typing policy`); secondary labels pointing at where the operand types
came from; and notes:

- `because <reason chain>`, one per operand, in the format of the typing
  dump's `because=`;
- `fix: <edit>`, the edits that could prove the check: `annotate
  parameter n as limited(<integer>, min: 0, max: 9)`, `test s with
  instance? against <circle> or <square> first`, `declare <shape> sealed
  abstract`, or `write as(<circle>, ...) to check deliberately`.

Notes are facts for renderers, not catalog messages; `interloko_lab` and
the REPL print them under the diagnostic. The EOP offers the fixes an
annotation makes as `preview-fix` and `apply-fix` (see
[`eop_workbench.md`](eop_workbench.md#strict-typing)).

Strict errors are errors like any other, so they block running (see
[Errors Block Running](#errors-block-running)).

## Errors Block Running

Every error-severity diagnostic keeps the program from running or being
installed, whatever stage reported it: a semantic error, an internal
lowering failure, or an error-severity typing finding. Warnings never do.
Typing errors are the case where code exists to run: strictness and the
typing policies change only how verdicts are reported, so a program with a
strict error still lowers and emits tape, keeping the checks it could not
prove. That code is not run:

- `SessionArtifacts::check_runnable` is the one rule. Calls
  (`call_function` and its variants), `runtime()`, a `SessionRuntime` call,
  and `code_unit()` (the unit a chooser installs) refuse with
  `FunctionCallError::CompilationFailed`, which lists each error with its
  index in `SessionArtifacts::diagnostics` (its `diagnostic:d<n>` object);
  `RuntimeMode::RunMain` leaves `runtime_result` empty.
- The refusal reads `compilation reported 2 errors: diagnostic:d0
  typing.unproved-range at 302..303 (typing:def…:s0); ...`: each error's
  object, code, span and typing decision. Tools print the diagnostics
  themselves first, with their `because` and `fix:` notes.
- `interloko_lab run`, `trace`, `report` and the `run` affordance, the
  corpus runner, and the REPL's `:run`, expressions and `run` affordance
  all apply it. `interloko_session::Session` never accepts a source or
  typing configuration with diagnostics, and mutations and the REPL's
  forms and break-loop edits reject any that add errors, so an edit with a
  strict error is never installed either.

This covers every error-severity typing finding, not only strict ones: a
policy such as `cast.refuted=error` asks for that finding to fail the
compilation, and severity is the one notion of failure every tool gates
on. Two kinds of error, one that blocks and one that does not, would make
`has_errors()` and an accepted run disagree. Soft typing's defaults report
no errors (refuted checks are warnings), so code written without opting in
to strictness or an error-severity policy is unaffected.

## Runtime Type Errors

A runtime check that fails does not trap. It signals a `<type-error>`
condition, and its stable code follows the same `area.name` scheme, with
the condition's class as the area. The code names what kind of check
failed:

| Code | Check |
| --- | --- |
| `type-error.cast` | a value is not an instance of the class an `as` (or a boxed `<object>` read as a number, such as a callable value's result) expects |
| `type-error.entry` | an argument passed through a callable value is not of its parameter's class, number kind or `<boolean>`, a callable from untyped code returns a value its typed binding's signature does not allow, an exit procedure's value is not of its block's class, or a `use-value` replacing a failed send's result is not of its generic's result class (see [`eop_workbench.md`](eop_workbench.md#runtime-type-errors)) |
| `type-error.range` | a number is outside a limited type's range: a conversion to `<i32>`/`<u32>`, a float converted to an integer it does not equal, a shift count outside `0..=31`, or an argument, binding, slot value, constant or result outside its `limited(<integer>, ...)` type; or a text operation rejects its argument: `as(<character>, i)` an integer that is not a Unicode scalar value, `digit-value` a character that is not an ASCII digit, or a string operation a `<string-position>` (or `position-at` an index) that is not a boundary of its string where it needs one. A text operation's `use-value` replaces the rejected argument, and the operation checks the replacement again |
| `type-error.overflow` | integer arithmetic produced a result its type cannot represent |

`interloko_model::condition::TypeErrorKind` owns the codes, and
`DiagnosticCode` mirrors them (`DiagnosticCode::TypeErrorCast`, ...). A
condition carries its code in its `type-error-code` slot, beside the
rejected value, the expected type (a type object a handler can test values
against, printed as Loko writes the type; see
[`eop_workbench.md`](eop_workbench.md#runtime-type-errors)), and the
check's site:

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

Some sends fail for every value of an argument's type, and those are
compile errors. A parameter whose methods all specialize on object classes
carries runtime objects, which a number, boolean or callable is not (only a
boxed `<object>` parameter, one some method specializes on `<object>`, a
number class, `<boolean>` or `<function>`, carries those), so passing one
names the language-level reason rather than how the parameter is carried:

| Code | When |
| --- | --- |
| `sem.no-applicable-method` | no method of the generic accepts the argument's type there (args `generic`, `argument_index`, `actual`, `specializers`, the classes its methods specialize that argument on, and `class`, the one a new method would need: the number's class, `<boolean>` or `<function>`) |
| `sem.generic-without-methods` | the same, for a generic with no methods yet (no `specializers`) |
| `sem.generic-argument-type-mismatch` | the argument is not of the parameter's declared class, such as a `<square>` for `<circle>` |

There is no code for an ambiguous send: methods that could be equally
specific for some arguments are rejected when the module compiles
(`sem.ambiguous-generic-method-dispatch`). The tape trap code a failed send
used, 1, is retired with those of the failed type checks; `make` on a class
value still traps (`runtime.trap`) when the class needs an init keyword the
call cannot pass, which is not a dispatch failure.

## Calls Through `<callable>`

An untyped `<callable>` value says nothing about what it returns, so a
call through it (`f(...)`) takes its result types from context: a
declared binding (`let r :: <integer> = f(4)`), the enclosing
function's declared result, or an argument's parameter type. With no such
context it reports `sem.untyped-callable-result`, which suggests declaring
one or giving the callable a signature such as `callable(<integer>) =>
<integer>`. A result the call site relies on but cannot check is
`sem.unchecked-callable-result`, and an `apply` target that is not
callable at all is `sem.apply-target-type-mismatch`.

`apply` and a call through an untyped value whose callee takes `#rest`
pass a number of arguments only the run knows. When it is not one the
callee takes, the call signals `<argument-count-error>` as a call of the
wrong arity through any untyped value does; statically, a call
passing too few arguments to a known callee taking `#rest` is
`sem.function-too-few-arguments`, and an `apply` whose last argument
cannot be a `<sequence>` is `sem.apply-sequence-type-mismatch`.

## Corpus

The corpus runner supports both forms:

```loko
// expect: diagnostic code sem.unresolved-local
// expect: diagnostic contains unresolved local `missing`
```

Code expectations are preferred for behavior. String expectations remain useful
as compatibility checks for the default English catalog. A run's outcome is
matched by its code too, as in `// expect: runtime code type-error.overflow`,
and `// expect: run blocked` says compile errors keep `main` from running.

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
- Fix-it edits beyond strict typing's annotation fixes, and machine-applicable
  suggestions for other codes.
- Warning levels beyond the current error-only severity model.
