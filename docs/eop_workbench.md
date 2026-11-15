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
- `run`

The `add-method` mutation is now executable through `invoke-affordance` on
`generic:gN` objects; see [`eop_mutation.md`](eop_mutation.md) for the
contract. Other mutation-oriented affordances such as `edit-body` can appear
in records but are intentionally not executable by `interloko_lab` in this
slice.

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

The explanation is read-only. It describes the current one-argument nominal
dispatch model; it does not imply sealed optimization, method combination,
multi-argument dispatch, or new tape opcodes.

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
  slot read), `generic:gN` for the method set it assumes, and `class:cN` for
  a closed class it assumes gains no subclasses;
- `codegen`: what generated code does with the check, compared with lowering
  that ignores typing: `kept` (the same code), `elided` (the check is gone:
  a constant `instance?` test, possibly with its dead branch, a cast that
  reuses its operand, an untested shift count), `specialized` (the operation
  without its check: wrapping arithmetic, a reinterpreting conversion) or
  `inlined` (a numeric dispatch replaced by its method's operation). Only
  the session's `CodegenPolicy::Proved` acts on typing; under the default
  `CodegenPolicy::Off` every check is `kept`. `explain` names the policy,
  and says whether the generated code still performs the check or relies
  on the decision. `typing-config:session` shows the policy beside the
  typing configuration (`config=soft codegen=off ...`). `interloko_lab`
  takes `--codegen off|proved` on every command, and the REPL's
  `:codegen [off|proved]` shows or sets it.

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
