# Interloko Boundary

## Overview

Interloko is the provisional name for the core technical substrate underneath
the current Cairn prototype. The name is being introduced as an architecture
boundary before any project-wide rename.

The split is:

```text
Interloko  core substrate for programmable object spaces
Vivarium   later collaborative environment and product layer
Cairn      current prototype language/compiler name
Loko       possible future language/notation name, not used yet
```

In this repo, the first concrete Interloko artifacts are
`crates/interloko_model` and `crates/interloko_runtime`. The model crate holds
small shared facts. The runtime crate owns executable host support for the
current object model without depending on Cairn semantic IR.

## Boundaries

Interloko owns substrate concepts that are useful across compiler, runtime, and
future environment layers:

- durable model identifiers;
- small value-class facts;
- future protocol records that describe objects, host boundaries, and
  computations without owning a compiler pipeline.

Cairn owns the current language implementation:

- syntax and parsing;
- AST and semantic IR;
- type checking and diagnostics;
- lowering and tape emission;
- compiler sessions and incremental artifacts.

Vivarium is reserved for the later collaborative environment:

- people and agents;
- transcripts and shared workspaces;
- presence and collaboration;
- product-level affordances;
- permission and capability policy.

## Current Slices

The first extraction is deliberately small. `interloko_model` owns:

- `ClassId`;
- `SlotId`;
- `GenericFunctionId`;
- `MethodId`;
- `BuiltinValueClass`.

`interloko_compiler::sem` re-exports those names so existing code can keep using the
same semantic module path while the dependency direction improves.

`interloko_runtime` owns:

- runtime-representable `ValueKind` for host signatures and slot IO;
- `RuntimeLayout`, `RuntimeClass`, `RuntimeSlot`, `RuntimeGeneric`, and
  `RuntimeMethod`;
- host-call symbols and `execution_tape` host signatures;
- `CairnHost`, native object storage, boxed `<object>` values, strings,
  symbols, vectors, metaobjects, and host-backed object adapters.

`interloko_compiler` still owns semantic `ValueKind`, because it contains
compiler-only states such as known callables and errors. The compiler also owns
the semantic-to-runtime adapter: `interloko_compiler::runtime::runtime_layout_from_sem`.
That function translates finalized semantic classes, slots, generics, methods,
and setter metadata into runtime layout values consumed by `interloko_runtime`.

`interloko_compiler::runtime` remains as a compatibility module that re-exports the
runtime crate plus the semantic layout adapter.

## Glossary

- **Substrate:** shared Rust types and protocols that compiler/runtime layers
  can both depend on without depending on each other.
- **Compiler-owned:** facts tied to parsing, semantic analysis, lowering, or
  artifact scheduling.
- **Runtime-owned:** host execution, object heap behavior, boxed values, and
  host calls.
- **Product layer:** the future user-facing Vivarium environment built on the
  substrate and language/runtime pieces.
