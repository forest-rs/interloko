# Cairn Milestone 0 Progress

## Implemented

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

## Intentionally Deferred

- Generic functions, classes, effects, refinements, macros, Cranelift, and a full module system.
- Incremental query/caching shell around the compiler stages.
- Stable edit-preserving ID strategy beyond the current per-module semantic IDs.
- A separate backend-neutral lowered IR layer; milestone 0 emits tape directly from semantic IR.
- Runtime object model and host-call language semantics.

## Follow-Up Tickets

- `cai-ki3x`: add query invalidation shell after milestone 0.
- `cai-8lcw`: define stable compiler ID strategy.
- `cai-f4sj`: split lowered IR from tape emission.

## Validation

- `cargo fmt --all`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features`
- `cargo run -p cairn_forty_two_example` prints `42`.
