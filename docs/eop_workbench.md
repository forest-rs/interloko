# Cairn EOP Workbench

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
- `class:c0`
- `slot:c0.s0`
- `accessor:c0.s0`
- `generic:g0`
- `method:m0`
- `function:f0`
- `dispatch:d0`
- `runtime-class:c0`
- `runtime-slot:c0.s0`
- `artifact:<artifact-key>`
- `diagnostic:d0`

## Inspect

Inspect prints one structured EOP record:

```sh
cargo run -p interloko_lab -- inspect tools/interloko_lab/fixtures/generic_area.loko class:c0
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
cargo run -p interloko_lab -- inspect tools/interloko_lab/fixtures/inherited_point.loko class:c1
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
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/generic_area.loko slot:c0.s0 show-accessor
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/generic_area.loko accessor:c0.s0 show-slot
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/generic_area.loko generic:g0 show-dispatch
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/generic_area.loko dispatch:d0 show-dispatch
cargo run -p interloko_lab -- invoke-affordance tools/interloko_lab/fixtures/generic_area.loko class:c0 show-runtime-layout
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
- `run`

Mutation-oriented affordances such as `edit-body` and `add-method` can appear
in records but are intentionally not executable by `interloko_lab` in this slice.

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

The explanation is read-only. It describes the current one-argument nominal
dispatch model; it does not imply sealed optimization, method combination,
multi-argument dispatch, or new tape opcodes.

## Explicit Host Projection

The workbench can install an explicit host projection provider from fixture
metadata:

```cairn
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
and do not create Cairn language semantics.

## Glossary

- **Environment object:** a compiler, runtime-layout, diagnostic, query, or
  host-projected fact that tools can identify.
- **Affordance:** a stable action name exposed by an environment object.
- **Projection:** converting existing compiler or host facts into EOP records.
- **Host provider:** explicit Rust adapter that contributes host-owned records.
- **Dispatch site:** a compiler-generated environment object for one semantic
  generic call.
- **Workbench:** the `interloko_lab` command surface for inspecting records.
