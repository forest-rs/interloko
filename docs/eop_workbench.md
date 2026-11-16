# Interloko EOP Workbench

## Overview

`interloko_lab` can inspect the structured Environment Object Protocol records
emitted by the compiler session. The workbench answers three questions:

- what environment object is this?
- what affordances does it expose?
- which read-only view can this affordance produce?

This is a workbench surface over Rust structs, not a persistent image, database,
UI contract, Portolan bridge, or authorization layer.

## Object IDs

Run `eop` to discover object IDs:

```sh
cargo run -p interloko_lab -- eop tools/interloko_lab/fixtures/generic_area.loko
```

Typical IDs include:

- `source:<source-file-id>`
- `class:c0` for the compiler-generated `<object>` root
- `class:c1` for the first source or host class
- `slot:c1.s0`
- `accessor:c1.s0`
- `generic:g0`
- `method:m0`
- `function:f0`
- `constant:k0`
- `dispatch:d0`
- `typing-config:session`
- `typing:<definition-id>:s0` for the typing decision at check site `s0` of
  a definition
- `contract:<definition-id>` for the type contract of a function, generic,
  or method
- `binding:<definition-id>:<name>` for a parameter or `let` binding that can
  carry a type annotation, with `#<n>` when the name is bound more than once
  (see [Annotation Targets](#annotation-targets))
- `result:<definition-id>` for a function's or method's result, or a
  constant's value
- `runtime-class:c0` for the root runtime layout row
- `runtime-class:c1`
- `runtime-slot:c1.s0`
- `artifact:<artifact-key>`
- `diagnostic:d0`

## Inspect

Inspect prints one structured EOP record:

```sh
cargo run -p interloko_lab -- inspect tools/interloko_lab/fixtures/generic_area.loko class:c1
```

The output includes:

- `kind`
- `class`
- `origin`
- `storage`
- `presentation`
- `affordances`
- `execution`, when available
- `inspection`

Diagnostics are environment objects too:

```sh
cargo run -p interloko_lab -- inspect tools/interloko_lab/fixtures/diagnostic_unknown_local.loko diagnostic:d0
```

Diagnostic inspection reports structured facts, not only prose: stable code,
severity, source span, labels, args, and the default English rendering. Tools
should use the code and args for behavior; the rendered message is presentation.

Class inspection includes initialization facts derived from the class's
finalized effective slots:

```sh
cargo run -p interloko_lab -- inspect tools/interloko_lab/fixtures/inherited_point.loko class:c2
```

The `inspection` text names the accepted init keywords, inherited/direct slot
order, required/default status, explicit-argument override behavior, and
`initialize-methods=deferred`. That last field is intentional: the workbench
shows the substrate that future Dylan-style `initialize` methods can consume,
not user-defined initialization methods themselves.

## Affordances

List actions exposed by one object:

```sh
cargo run -p interloko_lab -- affordances tools/interloko_lab/fixtures/generic_area.loko generic:g0
```

Unknown object IDs fail with a list of available IDs:

```sh
cargo run -p interloko_lab -- affordances tools/interloko_lab/fixtures/generic_area.loko missing:object
```

## Read-Only Invocation

`invoke-affordance` executes only read-only workbench affordances that map to
existing compiler/session views:

```sh
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/generic_area.loko slot:c1.s0 show-accessor
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/generic_area.loko accessor:c1.s0 show-slot
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/generic_area.loko generic:g0 show-dispatch
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/generic_area.loko dispatch:d0 show-dispatch
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/generic_area.loko class:c1 show-runtime-layout
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/generic_area.loko method:m0 show-dependencies
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/generic_area.loko method:m0 dump-sem
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/generic_area.loko function:f0 run
```

Supported read-only invocations are:

- `show-slot`
- `show-accessor`
- `show-dispatch`
- `show-runtime-layout`
- `show-dependencies`
- `dump-syntax`
- `dump-ast`
- `dump-sem`
- `dump-mop`
- `dump-eop`
- `dump-lowered`
- `dump-tape`
- `show-typing`
- `show-types`
- `show-dependents`
- `preview-strict`
- `run`

`run` runs `main` and, like `interloko_lab run`, refuses when compilation
reported errors, strict typing's included, naming them; the affordance is
still listed, since the errors are facts of this compilation, not of the
function.

The `add-method` mutation is now executable through `invoke-affordance` on
`generic:gN` objects; see [`eop_mutation.md`](eop_mutation.md) for the
contract. So are the typing adjustments `annotate`, `preview-annotation`,
`preview-fix`, `apply-fix`, `set-typing-policy`, and `set-codegen-policy`;
see [Adjusting Typing](#adjusting-typing). Other mutation-oriented affordances
such as `edit-body` can appear in records but are intentionally not
executable by `interloko_lab` in this slice.

## Dispatch Sites

`dispatch:dN` records are compiler-generated EOP objects for semantic generic
call sites. They connect a source call to the generic contract, static argument
facts, applicable and rejected methods, the lowered dispatcher, and related
artifact keys.

List dispatch sites:

```sh
cargo run -p interloko_lab -- dispatch tools/interloko_lab/fixtures/inherited_dispatch.loko
```

Explain one dispatch site:

```sh
cargo run -p interloko_lab -- dispatch tools/interloko_lab/fixtures/inherited_dispatch.loko dispatch:d0
```

The same explanation is available through EOP:

```sh
cargo run -p interloko_lab -- inspect tools/interloko_lab/fixtures/inherited_dispatch.loko dispatch:d0
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/inherited_dispatch.loko dispatch:d0 show-dispatch
```

Each record names its call by owner definition and check site, as in
`owner: function main definition=def… site=s1`. Semantic lowering numbers
every expression that leaves a runtime check (generic calls, casts, instance
tests, numeric conversions, and integer arithmetic) densely within its
function, method, or constant, in evaluation order; closures share their
enclosing definition's numbering, and `dump sem` shows each as `site=sN`. A
site depends only on its own definition's body, so editing another
definition never renumbers it, while `dispatch:dN` is a module-wide index
that can shift. Typing decisions are keyed the same way
(`typing:<definition-id>:sN`), and a dispatch decision links to the record
with the same owner and site, so two calls that share a source span still
link correctly. Generic calls inside closures get records under their
enclosing definition, as their typing decisions do. A typing decision's
explanation also lists each operand's reason chain, such as
`because #0: narrowed(s0,then)@… <circle> <- declared@… <shape>`, and the
closed-world assumptions its verdict relies on.

A record's exact argument classes come from the typing decision at the same
owner and site: an argument has an exact class only when its flow type does,
so a local bound to `make(<circle>)` is exactly `<circle>` while the result of
a callable declared to return `<shape>` is not. When typing is blocked by
semantic errors, no class is known statically.

The explanation is read-only. It describes the current nominal dispatch
model; it does not imply method combination or new tape opcodes. A send
over a sealed class's cone is proved when every piece has a method, and is
linked when every piece is an exact class selecting the same one; a send
whose pieces select different methods still asks the runtime.

## Types and Contracts

Function, method, and constant objects expose `show-types`, which lists the
flow type of each parameter and local where it is bound, every read a test
narrowed further, and the definition's result, each with its reason chain:

```sh
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/typed_contracts.loko function:f1 show-types
```

```text
  definition def… function measure
    param s@221..222 <shape> because=declared@221..222 <shape>
      use @273..274 <circle> because=narrowed(s0,then)@240..262 <circle> <- declared@221..222 <shape>
    result <integer> because=joined@…
```

A binding whose flow type is narrower than sem's also shows `declared=`, as
in `local c@… exactly <circle> declared=<shape>`. Locals of closures and
captures are labelled `closure#N`. `dump --stage types` renders the same
listing for every definition. Embedders ask the same question directly with
`SessionArtifacts::type_of`, selecting a binding, a check site, or a source
span; site and span queries re-analyze one definition rather than storing a
fact per expression.

Each typing decision object carries, besides its verdict:

- `reasons`: the rendered reason chain of each operand type;
- `depends-on`: what the verdict relies on outside its own body, as EOP ids:
  `contract:<definition-id>` for a cited function, generic, or method
  contract (`constant:kN` or `accessor:cN.sM` for a constant's value or a
  slot read), `generic:gN` for the method set it assumes, `class:cN` for a
  closed built-in or host class it assumes gains no subclasses, and the
  sealed class's `contract:<definition-id>` for a sealed class whose pieces
  it relies on;
- `assumes`: the closed-world assumptions themselves, as sentences such as
  `sealed abstract class <shape> has exactly direct subclasses {<circle>,
  <square>}`, `sealed class <circle> has no subclasses`, or `generic area
  has exactly methods {area(<circle>), area(<square>)}`. They include those
  the operands' facts cite, so a cast proved in an `instance?` test's else
  branch assumes the sealing that narrowed it;
- `codegen`: what generated code does with the check, compared with lowering
  that ignores typing: `kept` (the same code), `elided` (the check is gone:
  a constant `instance?` test, possibly with its dead branch, a cast that
  reuses its operand, an untested shift count), `specialized` (the operation
  without its check: wrapping arithmetic, a reinterpreting conversion) or
  `inlined` (a numeric dispatch replaced by its method's operation), or
  `linked` (a dispatch whose method typing selected calls it through a
  relinkable link: no method selection or indirect call). Only
  `CodegenPolicy::Proved`, the default, acts on typing; under
  `CodegenPolicy::Off` every check is `kept`. `explain` names the policy,
  and says whether the generated code still performs the check or relies
  on the decision. See [Codegen Policy](#codegen-policy).

`contract:<definition-id>` objects show a definition's contract, the
parameter and result types callers were compiled to rely on. Their
`show-dependents` affordance lists the typing decisions whose reasons cite
that contract:

```sh
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/typed_contracts.loko contract:<definition-id> show-dependents
```

Dependents are read from the recorded reasons, so they are what the current
facts rely on rather than a dependency graph; a method's contract is cited by
nothing, since sends rely on their generic's contract.

A generic's contract object also says whether the generic is sealed, how
many methods it has, and how many decisions assume its method set
(`sealing=sealed methods=2 assumed-by=3`). Each sealed class has a contract
object too: its sealing is the promise code compiled against it relies on.
Its inspection lists the direct subclasses and abstractness
(`sealing=sealed abstract=yes subclasses=[<circle>,<square>] dependents=2`),
and `show-dependents` renders the closed world as a sentence and lists the
decisions relying on it. Class and generic objects show `sealing=` (and
classes `abstract=`) in their inspection, and a sealed generic offers no
`add-method`.

## Adjusting Typing

The objects above answer what typing decided. Four affordances change the
inputs it decides from, and each reports a *verdict delta*: which decisions
changed verdict, severity, or codegen. That is how a user or agent sees
whether an adjustment pays off.

- `annotate` and `preview-annotation` on `binding:` and `result:` objects
  write a type annotation into the source, or report what writing it would
  change without writing it.
- `set-typing-policy` and `set-codegen-policy` on `typing-config:session`
  change the session's policies.

[Strict Typing](#strict-typing) adds `preview-strict`, and `preview-fix`
and `apply-fix`, which go through the same annotation path.

### Annotation Targets

A binding id names a place source can declare a type by definition and name,
never by sem's local numbering, which shifts when the body changes:

- `binding:<definition-id>:<name>` is the first parameter, closure
  parameter, or `let` binding named `<name>` in the function, method, or
  constant `<definition-id>`, in source order;
- `binding:<definition-id>:<name>#<n>` is the n-th one (from zero), when the
  name is bound more than once, as in a closure parameter that shadows a
  local; `#0` is accepted and written without the suffix;
- `result:<definition-id>` is a function's or method's result, or a
  constant's value.

Definition ids hash the source file, the definition's kind, and its name (a
method's also its specializers), so editing another definition, adding a
definition, or adding a binding of another name leaves a target's id alone.
Renaming the binding or its definition changes it, and so does annotating a
method parameter, since that changes the method's specializers. Restart
condition names and block exits bind names but take no annotation, so they
are not targets and do not count toward `#<n>`; neither are bindings a
staged grammar emitted, which have no source of their own to edit.

`eop` lists every target as a `binding` or `result` object. Its inspection
says whether the type is written or inferred and what is written:

```text
object binding:def…:s
  inspection definition=def… role=param declared=written annotation=<shape> span=245..246
object binding:def…:bonus
  inspection definition=def… role=local declared=inferred annotation=none span=265..270
```

Roles are `param`, `method-param`, `closure-param`, `local`, `result`, and
`constant-value`. `show-types` on a target renders its type, its declared
type with `written` or `inferred`, and its narrowed uses. `show-types` on a
definition and `dump --stage types` end each binding and result line with
`annotation=written|inferred id=<target>`, and
`SessionArtifacts::type_of` sets `TypeReport::declaration` the same way.
`SessionArtifacts::annotation_sites` lists the targets for embedders.

### Annotate

```sh
cargo run -p interloko_lab -- annotate tools/interloko_lab/fixtures/annotate_shapes.loko binding:def…:s '<circle>' --dry-run
cargo run -p interloko_lab -- annotate tools/interloko_lab/fixtures/annotate_shapes.loko binding:def…:s '<circle>'
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/annotate_shapes.loko binding:def…:s annotate --type '<circle>'
```

`annotate` is the `SessionMutation::Annotate` mutation: it replaces the
written annotation, or inserts ` :: <type>` after the name (` => <type>`
after a parameter list, for a result), compiles the edited source as a
candidate, and swaps it in only when the candidate has no new errors, the
same definitions, and the target reads back with exactly the requested type.
A type that callers violate, a class that does not exist, or text that is
not a type is rejected and the session is left alone. Annotating a method
parameter changes the method's specializer, and so which calls it handles;
the outcome names the method's new target id. See
[`eop_mutation.md`](eop_mutation.md#annotate) for the log, rewind, and replay.

`preview-annotation` (`annotate --dry-run`) runs the same candidate and
checks and reports the same delta, without swapping, logging, or using a
mutation id. It fails exactly when `annotate` would.

The delta matches each decision across the edit by owner, check kind, and
span, not by site number, since an annotation can add a check (a typed
`let` casts its value) and renumber the sites after it:

```text
preview annotate target=binding:def…:s annotation=<circle> previous=<shape> (not applied)
typing delta: changed=2 added=0 removed=0 unchanged=1
  verdicts proved 0->2 checked 3->1 refuted 0->0
  codegen kept 3->2 elided 0->1 specialized 0->0 inlined 0->0 linked 0->0
  errors 0->0
  typing:def…:s0 instance verdict=checked->proved codegen=kept->elided severity=silent->warning
  typing:def…:s1 dispatch verdict=checked->proved codegen=kept severity=silent
```

The second to fourth lines count every decision before and after, the
fourth those reported as errors, such as strict errors. Each
change line names the decision after the change, then `old->new` for what
changed; a decision the change introduced says `added`, one it removed says
`removed`, with its values. An empty delta (`changed=0 added=0 removed=0`)
means the annotation buys nothing. `TypingDelta` carries the same facts
typed, as `DecisionChange { before, after }` pairs and tallies.

### Policies

```sh
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/annotate_shapes.loko typing-config:session set-typing-policy strict
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/annotate_shapes.loko typing-config:session set-typing-policy soft dispatch.checked=warning
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/inlined_overflow.loko typing-config:session set-codegen-policy off
```

`set-typing-policy` takes words applied in order: a preset (`soft`,
`silent`, `strict`) replaces the configuration, `check.verdict=severity`
(such as `cast.checked=warning`) changes one setting, and
`strict.scope=declared|all` and `strict.overflow=runtime|proved` change
strictness (see [Strict Typing](#strict-typing)). It re-derives
diagnostics without re-checking, so verdicts and code do not change, only
severities. `set-codegen-policy off|proved` lowers again, so codegen facets
change and verdicts do not. Both print the change, the delta, and any new
diagnostics. A policy that reports a finding at `error`, the strict
preset's errors included, makes the code unrunnable until it is fixed or
the policy changes back. The lab is stateless, so the policy lasts for that invocation;
the REPL keeps it (`:typing` and `:codegen`), as do `Session` and the
compiler sessions.

Policies are session inputs, not source mutations, so neither affordance
adds a mutation log entry. The log is a transcript of source edits that
rewind and replay rebuild from; a policy is applied to every compile the
session makes, like its grammar and host registries. Rewind and replay keep
the session's current policies, `:reset` keeps them, and a policy change in
the log would make rewinding past it silently change how the session
reports and lowers code.

In the REPL:

```text
:annotate [--dry-run] <binding:...|result:...> <type>
:typing [preset|check.verdict=severity ...]
:invoke <binding:...> annotate|preview-annotation <type>
:invoke typing-config:session set-typing-policy|set-codegen-policy <args>
```

`:annotate` edits the accepted buffer through the same candidate path as
any other change; `:typing` with no argument shows the policy.

### Strict Typing

A strict definition must prove the checks the compiler inserts into it;
see the README and `typing.rs` for which checks those are. Each one typing
leaves to the runtime is a `typing.unproved-*` error, and any check that
always fails is an error too (see
[`diagnostics.md`](diagnostics.md#strict-typing)). The EOP ties each error
to what it reports and to what could fix it:

- A typing finding's `diagnostic:d<n>` object names its decision
  (`decision=typing:def…:s<n>` in its inspection, `TypingFacet::Finding`
  typed) and offers `show-typing`, which explains that decision. The
  decision object's inspection ends `origin=<implicit|explicit|test|entry>
  strict=<unproved|refuted|none>`, and its `show-typing` says whether the
  check must be proved and lists the fixes.
- A strict error lists its fixes, in order, as `fix<n>="..."` in the
  inspection and `StrictFixOffer`s in the facet: annotate a parameter, test
  it with `instance?`, seal a class, or write the check as an `as`. A fix
  that an annotation makes names its target and type
  (`@binding:def…:n:limited(<integer>, min: 0, max: 9)`), and such an error
  offers `preview-fix [n]` and `apply-fix [n]` (fix `0` by default). They
  are `preview-annotation` and `annotate` on that target, so the delta
  shows which errors the fix resolves (`errors 2->0`, `severity=error->silent`)
  and applying adds an ordinary `Annotate` entry to the log.
- `preview-strict` on a function, method, generic, or constant reports
  what would fail if it were strict, without changing anything: each
  would-be error with its decision, reasons, and fixes. On a generic it
  previews each method.

```sh
cargo run -p interloko_lab -- run tools/interloko_lab/fixtures/strict_digits.loko
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/strict_digits.loko diagnostic:d0 preview-fix
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/strict_digits.loko diagnostic:d0 apply-fix 0
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/strict_digits.loko function:f0 preview-strict
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/annotate_shapes.loko typing-config:session set-typing-policy strict
```

```text
diagnostics:
  Error typing.unproved-range 302..303 function twice is strict (declared), but typing cannot prove that <integer> is within limited(<integer>, min: 0, max: 9) (typing:def…:s0)
    because declared@265..266 <integer>
    fix: annotate parameter n as limited(<integer>, min: 0, max: 9)
    fix: write as(limited(<integer>, min: 0, max: 9), ...) to check deliberately
  ...
main did not run: compilation reported 2 errors: diagnostic:d0 typing.unproved-range at 302..303 (typing:def…:s0); diagnostic:d1 typing.unproved-range at 313..314 (typing:def…:s1)
```

Strictness is reporting only, and an input of its own: toggling a
definition's `strict` re-derives the typing diagnostics from the recorded
decisions (the incremental graph's `strictness` artifact), and so does the
policy, so nothing is re-typed or lowered again. A strict definition with
errors still lowers, keeping the checks it could not prove, but its errors
block running like any compile error
([`diagnostics.md`](diagnostics.md#errors-block-running)): `run`, `trace`
and the `run` affordance print the diagnostics, then the refusal naming
each error's `diagnostic:d<n>` object, span and decision, and exit
nonzero. So do the REPL's `:run`, expressions and `run` affordance, for
instance after `:typing strict` leaves an error in the accepted buffer.
Edits never install one: mutations (`add-method`, `annotate`, `apply-fix`)
reject an edit that adds errors and print them, the REPL rejects a form
that has them, and a break-loop edit with errors is not installed under
the suspended run. In the REPL:

```text
:invoke <function:...|method:...|generic:...|constant:...> preview-strict
:invoke <diagnostic:dN> preview-fix|apply-fix [fix-index]
```

## Codegen Policy

The codegen policy decides whether lowering acts on what soft typing
proved. It is a session input of its own, beside the typing configuration,
and `typing-config:session` shows both (`config=soft codegen=proved ...`):

- `proved` (the default) drops or specializes each check typing proved, as
  the `codegen` facet of its decision records;
- `off` ignores typing facts, so the generated code keeps every runtime
  check and is what lowering emits without typing. Use it when debugging
  the dispatch and check machinery itself, or to compare against `proved`.

Switch it with `--codegen off|proved` on any `interloko_lab` command (before
the command or among its arguments), with `:codegen [off|proved]` in the
REPL (with no argument it shows the policy; it is kept across `:reset` and
cannot change while a break waits), with `Session::with_codegen_policy` and
`Session::set_codegen_policy` in `interloko_session`, with
`set_codegen_policy` on the compiler sessions, or with a corpus case's
`// codegen:` header:

```sh
cargo run -p interloko_lab -- trace --codegen off tools/interloko_lab/fixtures/inlined_overflow.loko
cargo run -p interloko_lab -- trace tools/interloko_lab/fixtures/inlined_overflow.loko
```

Both policies compute the same results and end in the same runtime outcome
codes; the corpus runs every case under both and fails if they differ. What
differs is what the generated code does on the way, and so what tools
observe:

- **Fewer sends.** A numeric send whose method typing selected runs that
  method's operation inline, and a proved cast or narrowed read makes no
  `interloko.cast_object` call. Execution traces show fewer host calls and
  activations (no `interloko.code.dispatch`, `+#dispatch` or `+#method0`
  for an inlined `+`), and the driver's `ControlStats::sends` and
  `send_cache_hits` count fewer sends. Dispatch-site records (`dispatch:dN`)
  still describe every send in the source; the decision's `codegen=inlined`
  says the run makes no dispatch there. Typing gives an inlined send's
  result its method's class, so in a chain such as `(x + 1) + 2` on a boxed
  `x` every send is inlined and the sums stay unboxed until the chain ends.
- **Linked sends.** Any other send whose method typing selected from its
  arguments' exact classes calls that method through an `execution_tape`
  link (`call.link`) instead of asking the runtime (`select-method`, the
  `interloko.code.dispatch` host call) and calling indirectly. A program
  declares one link per method, named after its generic and specializers
  (`area(<circle>)`), so a link's id is its method's and no edit renumbers
  it. The decision's codegen is `linked`, and the dispatch site record shows
  `send=linked:area(<circle>)` where a dynamic send shows `send=dynamic`;
  `dump-tape` lists the program's links. The send stays late-bound:
  installing code repoints every link at the new code, and each run points
  them back at its entry unit, so a linked send goes where a dynamic one
  would (see [`eop_mutation.md`](eop_mutation.md)); the break loop's
  `:links` shows where each points now. Execution traces show `call.link`
  instead of the host call and `call.indirect`, and the driver's
  `ControlStats::sends` counts only dynamic sends. A linked send costs about
  what a direct function call does; the `typed` bench's `generic send` case
  compares it with a dynamic one.
- **Failure locations.** A check inside inlined numeric code, such as the
  overflow check the inlined `+` keeps, signals its `<type-error>` from the
  caller's code at the call's span. Under `off` the same overflow signals
  from inside the built-in method, whose code has no source of its own:

  ```text
  off:    host-call interloko.condition.type_error args=(9223372036854775807, 1, 0u) ... at +#method0@20 (1:1) from +#dispatch@298 (1:1) from main@44 (8:17)
  proved: host-call interloko.condition.type_error args=(9223372036854775807, 1, 1u) ... at main@156 (8:17)
  ```

  Either way the condition names the same site, the `+` method's typing
  decision, and the run ends with the same outcome
  (`type-error.overflow`); `trace` names the signalling function and
  resolves the span to a line and column.
- **Lowered and tape dumps** lack the dropped checks: `i64.add` instead of
  `i64.add_ovf` and its signalling branch, no `select-method` for an inlined send,
  a constant instead of an `instance?` test.

Code lowered under either policy hot-swaps the same way: the install and
retry checks in [`eop_mutation.md`](eop_mutation.md) protect what proved code
relies on.

## Runtime Type Errors

A runtime check that fails signals a `<type-error>`
([`diagnostics.md`](diagnostics.md) lists its codes). The condition is an
ordinary object of the kernel class `<type-error>`, under `<error>` and
`<condition>`, and a program handles it like any other:

```loko
handle
  as(<circle>, s).radius
with restart e :: <type-error>
  if e.type-error-code = "type-error.cast" then 0 else 1 end
end
```

Its slots are `type-error-value` (the rejected value, as an `<object>`;
for an overflow, a vector of the operation's operands),
`type-error-expected-type` (the printed type, such as `<circle>`, `<i32>`,
`limited(<integer>, min: 0, max: 255)` or `subclass(<shape>)`),
`type-error-site` and `type-error-code`.

A `limited(<integer>, min: m, max: n)` type is checked where a value
reaches its declaration, so code reading the declared value may assume the
range. The caller checks an argument for a limited parameter, where typing
usually knows the argument's range and can prove the check away; a `let`
binding, a slot write and a constant check their values, and a function or
method checks a limited result before it returns (a method against its
generic's range, widening an `<i32>` or `<u32>` result to do so). Classes
that declare a slot of the same name share its setter generic, whose send
checks a range only when every such slot declares the same one; otherwise
each class's setter method checks the value against its own slot's range. A call that cannot see
the parameter's range is checked at the callee instead: a function entered
through a callable value, a closure, and a first-class generic check on
entry, and a host call through `call_function` refuses an argument out of
range (`FunctionCallError::ArgumentOutOfRange`). Each failure is a
`type-error.range` whose expected type is the limited type and whose site
is the check's, such as the call site.

The site ties a failure at run time to the static decision: for a check
typing classified it is the decision's EOP id, so an agent can inspect it
and see the verdict, the rule, why typing could not prove the check, and
what codegen did with it:

```sh
cargo run -p interloko_lab -- run tools/interloko_lab/fixtures/inlined_overflow.loko
cargo run -p interloko_lab -- inspect tools/interloko_lab/fixtures/inlined_overflow.loko typing:def38917abbcdc0a678:s0
```

The lowered dump names the same site on every check that can fail
(`%r5 = as <circle>, %r4: any else type-error.cast <circle> at
typing:def…:s0`).

An unhandled `<type-error>` suspends like any unhandled condition, with
every frame live, and goes to the condition chooser
(`interloko_runtime::chooser`). The driver knows which signals come from a
failed check (`UnhandledCondition::failed_check`); for those,
`UnhandledCondition::type_error` reads what the condition reports, and the
REPL break loop prints it. A `<type-error>` the program makes and signals
itself is an ordinary condition: no check continues after it, its
`use-value` is what its `signal` returns, and unhandled it ends the run as
`runtime.unhandled-condition`.

```text
break [1]: no handler for <type-error>; 2 frames suspended, nothing unwound
type error: code=type-error.range expected=<i32> value=5000000000 site=typing:def34e752e9bedb7b3c:s0
```

The `use-value` restart (`Choice::UseValue`) continues the failed check
with a replacement. A check whose result is a number takes a replacement
of that number type, which is all the check establishes (an `<i32>` for a
conversion to `<i32>`, the result for an overflow or a shift); a value of
another type is an invalid choice that ends the run, which the break loop
refuses before choosing it. A failed cast takes an
object, which the cast checks again: one of the wrong class signals a new
`<type-error>` at the same site, so code after a check only ever sees a
value that passed it. A failed `limited(<integer>, ...)` check takes an
`<integer>`, whose register type does not establish the range, so it too
tests the replacement again and signals at the same site until one is in
range. `retry` and `abort` work as for any condition. The
break loop parses literals only, so it can resume a numeric check; a cast
needs an object, which a programmatic chooser can take from a frame's
registers (`InterlokoHost::value_class` finds one of the right class).

One mismatch still traps rather than signalling. A call through an
untyped `<callable>` passes each argument in the register type the call
site has, and expects its results in the type its context needs. Objects
and class objects share one register type, so their classes are checked at
the callee's entry (`type-error.entry`) or on the result (`type-error.cast`).
Scalars and boxed `<object>`s do not: applying a function that takes a
`<double-float>` or an `<i32>` to an `<integer>`, one that returns an
`<object>` where a `<double-float>` is expected, or one that takes a
`<circle>` (or a generic that takes runtime objects) to an `<object>`
value, fails the VM's own signature check at the call (`runtime.trap`,
"type mismatch") before any Loko code runs. Making
those signal needs every untyped callable call to box its scalars and
every callable entry and result to check and unbox them, a calling
convention change for all callable values, so it waits for its own slice.

## Failed Sends

A generic send whose arguments no method accepts signals a
`<no-applicable-method>`, the kernel class beside `<type-error>` under
`<error>`, rather than trapping. A program handles it like any condition:

```loko
handle
  area(s)
with restart e :: <no-applicable-method>
  if generic-function-name(e.no-applicable-method-generic) = "area" then 0 else 1 end
end
```

Its slots are `no-applicable-method-generic` (the generic's
`<generic-function>` metaobject), `no-applicable-method-arguments` (a
`<vector>` of the arguments in order; a callable argument, which is not an
`<object>` and never selects a method, appears as the symbol `#<callable>`)
and `no-applicable-method-code` (`dispatch.no-applicable-method`).

Every send that can fail reaches the generic's dispatcher, which signals:
a dynamic send asks the runtime, which answers with the dispatcher when its
table selects nothing; a generic used as a value or through `apply` sends
the same way; a numeric send that is not inlined is a dynamic send. A
linked send exists only where typing proved a method applies to the
arguments' exact classes, and the layout, which no install may change,
keeps that method applicable, so it cannot fail; an inlined numeric send
runs a method typing selected. The dispatcher signals after its last method
fails to match, so a send that finds a method costs what it did. The driver
counts the sends that fail (`ControlStats::failed_sends`).

A parameter whose methods all specialize on object classes is passed as a
runtime object, while an `<object>` value (a parameter or binding declared
`<object>`, or `as(<object>, ...)`) is boxed and may hold a number. A
send, including `apply` of a known generic, evaluates its arguments and
then unboxes each such one; when one holds a number, no method can apply,
and the send itself signals the condition, so the sender is the innermost
frame. A number written directly is rejected when the program compiles
(`sem.generic-argument-type-mismatch`), since no method can ever take it.

An unhandled one suspends with every frame live, the dispatcher innermost
(`area#dispatch`), and the driver marks it as a failed check's
(`UnhandledCondition::failed_check`). `UnhandledCondition::no_applicable_method`
reads the generic's name, the arguments, their classes and the code; the
REPL break loop prints them and labels `use-value` with the generic's
declared result types (`CodeUnit::generic_result_types`). Unhandled, it
ends the run as `dispatch.no-applicable-method`. One the program signals
again from a handler is ordinary, as a re-signalled `<type-error>` is.

```text
break [1]: no handler for <no-applicable-method>; 3 frames suspended, nothing unwound
no applicable method: code=dispatch.no-applicable-method generic=area classes=(<square>) arguments=({<square>})
restarts:
  restart:0 use-value(limited(<integer>, min: 0, max: 100))  the send returns this value instead, checked against its generic's result type
```

`use-value` supplies the send's results, one per generic result, in the
generic's result kinds; another kind is an invalid choice. The dispatcher
then checks each one against the generic's declared result type before it
returns it, as an indirect entry checks an argument: a class or class
object signals `type-error.entry` and a `limited(<integer>, ...)` range
`type-error.range`, at the dispatcher's site
(`check:<generic>@<start>..<end>`), until a replacement passes. A result
sem knows to be one particular function, generic or accessor (an open
generic whose methods all return it) has that callable as its only value,
which no check of a closure could establish, so `use-value` supplies
nothing for it and the dispatcher returns the callable. Code after the
send therefore sees results that keep the generic's contract, as it does
when a method returns.

`retry` re-runs a sending frame (the dispatcher is not retryable: with
the same arguments and the same methods it would fail again). An edit that
changes how the sender sends installs while the send waits, and retrying
the sender's frame runs it. A new method cannot install under a waiting
send: methods are part of the runtime layout, which every unit of a run
shares and which linked sends and typing's closed-world facts rely on, so
the break loop refuses it for this run and the next run finds it.

A send never finds several equally specific methods: a module whose
methods could be ambiguous for some arguments does not compile
(`sem.ambiguous-generic-method-dispatch`), so there is no ambiguity
condition.

## Explicit Host Projection

The workbench can install an explicit host projection provider from fixture
metadata:

```loko
// host-eop-class: rust-shape <rust-shape>
// host-eop-instance: circle-10 <rust-shape> radius=10
define function main()
  42
end;
```

Run:

```sh
cargo run -p interloko_lab -- eop tools/interloko_lab/fixtures/host_projected_environment.loko
cargo run -p interloko_lab -- inspect tools/interloko_lab/fixtures/host_projected_environment.loko host-class:rust-shape
cargo run -p interloko_lab -- inspect tools/interloko_lab/fixtures/host_projected_environment.loko host-instance:circle-10
```

These records are host-provided environment objects. They do not make
`InterlokoHost` enumerate runtime heap values, do not reflect Rust automatically,
and do not create Loko language semantics.

## Glossary

- **Environment object:** a compiler, runtime-layout, diagnostic, query, or
  host-projected fact that tools can identify.
- **Affordance:** a stable action name exposed by an environment object.
- **Projection:** converting existing compiler or host facts into EOP records.
- **Host provider:** explicit Rust adapter that contributes host-owned records.
- **Dispatch site:** a compiler-generated environment object for one semantic
  generic call.
- **Workbench:** the `interloko_lab` command surface for inspecting records.
