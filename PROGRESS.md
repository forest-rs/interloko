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

## Implemented: Lowered Executable IR and Dumps

- Backend-neutral lowered IR in `cairn_compiler::lower`.
- Lowered function IDs, registers, labels, parameters, local-to-register bindings, and typed values.
- Lowered instructions for:
  - integer constants and runtime metadata constants
  - direct calls
  - generated generic dispatch control flow
  - object construction
  - slot reads
  - integer arithmetic
  - runtime class lookup, equality, branches, returns, and traps
- Generic function calls lower to ordinary calls into generated dispatcher functions.
- Dispatcher functions lower to ordinary class lookup, class-id comparison, branch, method call, return, and trap instructions.
- `execution_tape` emission now consumes lowered IR rather than walking semantic expressions directly.
- Object construction and slot reads are still encoded as ordinary `execution_tape` host calls.
- `CompiledModule::dump_lowered()` returns the lowered IR dump.
- `CompiledModule::dump_tape()` returns the `execution_tape` disassembly.
- Public API migration: callers that previously invoked `tape::emit(&sem)` should now call `lower::lower(&sem)` and then `tape::emit(&lowered)`.

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
- Lowered IR optimization passes, canonical basic blocks, and richer type checking.

## Follow-Up Tickets

- `cai-8lcw`: define stable compiler ID strategy.
- `cai-f4sj`: split lowered IR from tape emission (implemented by `cai-uv89`).

## Validation

- `cargo fmt --all`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features`
- `cargo doc --workspace --all-features --no-deps`
- `cargo run -p cairn_forty_two_example` prints `42`.
- `cargo run -p cairn_generic_area_example` prints `100`.
