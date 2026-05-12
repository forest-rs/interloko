# Cairn EOP Workbench

## Overview

`cairn_lab` can inspect the structured Environment Object Protocol records
emitted by the compiler session. The workbench answers three questions:

- what environment object is this?
- what affordances does it expose?
- which read-only view can this affordance produce?

This is a workbench surface over Rust structs, not a persistent image, database,
UI contract, Portolan bridge, or authorization layer.

## Object IDs

Run `eop` to discover object IDs:

```sh
cargo run -p cairn_lab -- eop tools/cairn_lab/fixtures/generic_area.cairn
```

Typical IDs include:

- `source:<source-file-id>`
- `class:c0`
- `slot:c0.s0`
- `accessor:c0.s0`
- `generic:g0`
- `method:m0`
- `function:f0`
- `runtime-class:c0`
- `runtime-slot:c0.s0`
- `artifact:<artifact-key>`
- `diagnostic:d0`

## Inspect

Inspect prints one structured EOP record:

```sh
cargo run -p cairn_lab -- inspect tools/cairn_lab/fixtures/generic_area.cairn class:c0
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
cargo run -p cairn_lab -- inspect tools/cairn_lab/fixtures/diagnostic_unknown_local.cairn diagnostic:d0
```

## Affordances

List actions exposed by one object:

```sh
cargo run -p cairn_lab -- affordances tools/cairn_lab/fixtures/generic_area.cairn generic:g0
```

Unknown object IDs fail with a list of available IDs:

```sh
cargo run -p cairn_lab -- affordances tools/cairn_lab/fixtures/generic_area.cairn missing:object
```

## Read-Only Invocation

`invoke-affordance` executes only read-only workbench affordances that map to
existing compiler/session views:

```sh
cargo run -p cairn_lab -- invoke-affordance tools/cairn_lab/fixtures/generic_area.cairn slot:c0.s0 show-accessor
cargo run -p cairn_lab -- invoke-affordance tools/cairn_lab/fixtures/generic_area.cairn accessor:c0.s0 show-slot
cargo run -p cairn_lab -- invoke-affordance tools/cairn_lab/fixtures/generic_area.cairn generic:g0 show-dispatch
cargo run -p cairn_lab -- invoke-affordance tools/cairn_lab/fixtures/generic_area.cairn class:c0 show-runtime-layout
cargo run -p cairn_lab -- invoke-affordance tools/cairn_lab/fixtures/generic_area.cairn method:m0 show-dependencies
cargo run -p cairn_lab -- invoke-affordance tools/cairn_lab/fixtures/generic_area.cairn method:m0 dump-sem
cargo run -p cairn_lab -- invoke-affordance tools/cairn_lab/fixtures/generic_area.cairn function:f0 run
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
in records but are intentionally not executable by `cairn_lab` in this slice.

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
cargo run -p cairn_lab -- eop tools/cairn_lab/fixtures/host_projected_environment.cairn
cargo run -p cairn_lab -- inspect tools/cairn_lab/fixtures/host_projected_environment.cairn host-class:rust-shape
cargo run -p cairn_lab -- inspect tools/cairn_lab/fixtures/host_projected_environment.cairn host-instance:circle-10
```

These records are host-provided environment objects. They do not make
`CairnHost` enumerate runtime heap values, do not reflect Rust automatically,
and do not create Cairn language semantics.

## Glossary

- **Environment object:** a compiler, runtime-layout, diagnostic, query, or
  host-projected fact that tools can identify.
- **Affordance:** a stable action name exposed by an environment object.
- **Projection:** converting existing compiler or host facts into EOP records.
- **Host provider:** explicit Rust adapter that contributes host-owned records.
- **Workbench:** the `cairn_lab` command surface for inspecting records.

