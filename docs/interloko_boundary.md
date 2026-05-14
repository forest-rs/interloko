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

In this repo, the first concrete Interloko artifact is
`crates/interloko_model`. It holds small shared model facts that should not be
owned by the compiler forever.

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

## First Slice

The first extraction is deliberately small. `interloko_model` owns:

- `ClassId`;
- `SlotId`;
- `GenericFunctionId`;
- `MethodId`;
- `BuiltinValueClass`.

`cairn_compiler::sem` re-exports those names so existing code can keep using the
same semantic module path while the dependency direction improves.

`ValueKind` stays in `cairn_compiler` for now because it still contains
compiler-only states such as known callables and errors. Runtime layout records
also stay in `cairn_compiler` until `cairn_runtime` can be extracted without
pulling semantic IR ownership across the crate boundary.

## Glossary

- **Substrate:** shared Rust types and protocols that compiler/runtime layers
  can both depend on without depending on each other.
- **Compiler-owned:** facts tied to parsing, semantic analysis, lowering, or
  artifact scheduling.
- **Runtime-owned:** host execution, object heap behavior, boxed values, and
  host calls.
- **Product layer:** the future user-facing Vivarium environment built on the
  substrate and language/runtime pieces.

