# Cairn Progress

## Implemented: Milestone 0

- Workspace scaffold with a `no_std + alloc` compiler crate and a separate runnable example crate.
- Tiny Dylan-like parser for:
  - `define function ... end;`
  - parameters
  - `let` bindings
  - integer literals
  - `+`, `-`, and `*`
  - direct function calls
- Lossless-enough syntax layer:
  - original source retained
  - all tokens retained, including whitespace and comments
  - CST nodes carry source spans
- AST lowering from CST.
- Semantic lowering with:
  - `FunctionId`
  - `LocalId`
  - direct-call resolution
  - unresolved-name and arity diagnostics
- `execution_tape` emission for direct calls and `i64` arithmetic.
- End-to-end driver that compiles and runs zero-argument `main`.
- Example program in `examples/forty_two` that returns and prints `42`.

## Implemented: Generic Dispatch Slice

- Class definitions with nominal `ClassId` values and slot declarations with `SlotId` values.
- Generic function declarations with `GenericFunctionId` values.
- Method definitions with `MethodId` values and one nominal class specializer.
- AST and semantic support for:
  - `make(<class>, slot: value)` object construction
  - `receiver.slot` reads
  - `GenericCall` for generic calls
  - `DirectCall` for ordinary function calls
- Runtime object host support for:
  - object construction
  - runtime class lookup
  - integer slot reads
- `execution_tape` emission using only ordinary calls, branches, and host calls.
- Generated dispatcher functions for generic calls; no generic-specific tape opcode.
- Thin invalidation shell using `invalidation 0.2` for source, parse, AST, bind, method lookup, method lowering, method tape, generic dispatcher, and program artifacts.
- Targeted invalidation tests showing that changing one method affects its lookup/lowering/tape artifacts without affecting unrelated method artifacts.
- Example program in `examples/generic_area` that compiles and runs `area(make(<circle>, radius: 10))` and prints `100`.

## Intentionally Deferred

- Sealed dispatch optimization.
- Parametric types.
- Effects and restarts.
- Refinements.
- Macros.
- Cranelift.
- Full module system.
- General type checking beyond the current `I64` and nominal object value categories.
- Multi-argument/multi-method dispatch beyond one nominal object argument.
- Runtime mutation and non-integer slots.
- Persistent caches or scheduling; the invalidation shell models dependencies only.
- Stable edit-preserving ID strategy beyond the current per-module semantic IDs.
- A separate backend-neutral lowered IR layer; the tape adapter still emits directly from semantic IR.

## Follow-Up Tickets

- `cai-8lcw`: define stable compiler ID strategy.
- `cai-f4sj`: split lowered IR from tape emission.

## Validation

- `cargo fmt --all`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features`
- `cargo doc --workspace --all-features --no-deps`
- `cargo run -p cairn_forty_two_example` prints `42`.
- `cargo run -p cairn_generic_area_example` prints `100`.
