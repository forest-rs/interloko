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

## Implemented: Stable-ish IDs and Artifact Keys

- `SourceFileId` identifies logical source files, with `syntax::parse_file` for callers that have stable paths.
- CST nodes carry `SyntaxNodeId` values.
- AST nodes and definitions carry `AstNodeId` and `DefinitionId` values.
- Semantic definitions carry stable `DefinitionId` values alongside dense semantic IDs.
- Incremental query artifacts expose stable `ArtifactKey` values based on source file, artifact kind, and definition identity.
- `QueryInvalidation` maps stable artifact keys to dense `invalidation 0.2` keys internally.
- Tests cover syntax/AST definition ID preservation across whitespace/comment edits and method artifact key preservation across insertion-driven method renumbering.
- Stability guarantees and non-guarantees are documented in `crates/cairn_compiler/ADR-stable-ids.md`.

## Implemented: Compiler Session and Workbench CLI

- `cairn_compiler::session` provides a no-std-friendly compiler session API over named source files.
- Session artifacts retain:
  - source file identity and text metadata
  - syntax tree
  - AST
  - semantic IR
  - lowered IR
  - tape module
  - diagnostics
  - optional runtime result
- Session dumps cover:
  - CST/syntax
  - AST
  - semantic IR
  - lowered IR
  - `execution_tape` disassembly
  - stable artifact graph with dependencies
- Session diff reports preserved definitions, added/removed/modified definitions, and affected query artifacts.
- `tools/cairn_lab` provides:
  - `run <file>`
  - `dump <file> --stage syntax|ast|sem|lowered|tape|all`
  - `artifacts <file>`
  - `diff <before> <after>`
- CLI fixtures cover the generic-area program, harmless trivia edits, and a focused method body edit.

## Implemented: Small-Program Corpus Runner

- `corpus/pass` contains runnable small programs for:
  - arithmetic and precedence;
  - local bindings;
  - direct function calls;
  - class definitions, object construction, slot declarations, and slot reads;
  - single-method and multi-class generic dispatch.
- `corpus/fail` contains diagnostic examples for:
  - unresolved locals;
  - unresolved functions/generics;
  - arity mismatches;
  - missing slot initializers;
  - unknown slot initializers.
- Each `.cairn` corpus file declares expectations in leading comments:
  - `// expect: run N`;
  - `// expect: diagnostics N`;
  - `// expect: diagnostic contains TEXT`;
  - `// expect: dump STAGE contains TEXT`.
- Dump expectations are intentionally substring-based so they act like small
  semantic landmarks rather than full brittle snapshots.
- `tools/cairn_corpus` provides:
  - `check [corpus-root]` to compile every corpus program through
    `cairn_compiler::session`;
  - `list [corpus-root]` to inspect discovered cases;
  - `dump <file.cairn> --stage syntax|ast|sem|lowered|tape|all` for
    snapshot-like stage output without committing generated dumps.
- Add a new case by placing a `.cairn` file under `corpus/pass` or
  `corpus/fail`, adding at least one `// expect:` header line, and running
  `cargo run -p cairn_corpus`.

## Implemented: In-Memory Incremental Compiler Engine

- `cairn_compiler::session::IncrementalCompilerSession` provides an in-memory
  edit/compile API over named sources with stable `SourceFileId` values.
- The incremental session retains the latest `SessionArtifacts` snapshot and a
  cache entry for every stable `ArtifactKey` in the current artifact graph.
- Cache reports cover:
  - parse;
  - AST;
  - semantic binding;
  - diagnostics;
  - method lookup;
  - method lowering;
  - method tape;
  - generic dispatcher;
  - lowered module;
  - tape program;
  - runtime result.
- Each artifact report includes:
  - stable artifact key;
  - `Reused` or `Recomputed` action;
  - invalidated flag;
  - affected flag from semantic edit analysis;
  - deterministic `work_units` and `elapsed_ticks` counters.
- The core compiler crate remains `no_std + alloc`. Timing is represented as
  logical ticks in the core cache report; wall-clock collection is deferred to a
  later std scheduler or workbench wrapper.
- `cairn_lab diff <before> <after>` now runs through the incremental session
  and prints cache status, counters, dependency edges, and the semantic diff.
- `cairn_corpus check` now compiles each program through
  `IncrementalCompilerSession`.
- Tests cover:
  - initial cache population;
  - repeat compile reusing all artifacts;
  - harmless trivia edits reusing most artifacts;
  - focused method-body edits recomputing the edited method's lowered/tape
    artifacts while reusing unrelated method artifacts;
  - definition-changing edits marking broad dependents affected.

### Migration Notes

- `ArtifactKind` and `QueryArtifact` gained public variants for diagnostics,
  whole lowered-module artifacts, and runtime-result artifacts.
- `cairn_lab diff` output is now an incremental cache report followed by the
  existing preserved/changed/affected definition diff.

## Implemented: Control Flow, Booleans, and Comparisons

- Syntax now recognizes boolean literals `#t` and `#f`, integer comparison
  operators `=`, `<`, `<=`, `>`, `>=`, and expression-level
  `if ... then ... else ... end`.
- AST now has explicit boolean, comparison, and conditional expression nodes.
- Semantic IR now has a `Bool` value category alongside `I64`, nominal objects,
  and errors.
- Integer comparisons require `I64` operands and produce `Bool`.
- `if` conditions must be `Bool`, and both branches must produce the same
  semantic value category.
- Diagnostics cover non-boolean `if` conditions, mismatched branch result
  kinds, non-`I64` arithmetic operands, and non-`I64` comparison operands.
- Lowered IR now includes:
  - boolean constants;
  - signed integer comparisons;
  - conditional branches;
  - unconditional jumps;
  - register moves used as explicit join values.
- `execution_tape` emission maps those lowered instructions to ordinary tape
  operations. There are no special Cairn control-flow opcodes.
- Session dumps show control flow in CST, AST, semantic IR, lowered IR, and tape
  disassembly.
- The corpus now includes small programs for boolean conditionals,
  comparisons, `max`, `abs`, nested conditionals, generic dispatch guarded by a
  conditional, and expected control-flow diagnostics.
- Incremental tests cover edits inside one conditional branch and verify that
  the edited method's lowered/tape artifacts recompute while unrelated method
  artifacts are reused.

### Deferred Control-Flow Design

- Loops are deferred until lowered IR has an explicit block model that can
  represent backedges and loop-carried values cleanly.
- Recursion-specific analysis is deferred. Direct calls may use simple
  source-order result categories, but this is not a full fixed-point return-kind
  inference system.
- Short-circuit boolean operators are deferred so they can be specified as
  control-flow forms rather than eager arithmetic operators.

## Implemented: Recoverable Condition Restart Slice

- Condition classes are ordinary nominal classes. There is no separate
  condition declaration form in this slice.
- `signal(condition)` is recognized as a special call-shaped expression where
  `condition` must be a statically known object.
- `handle BODY with restart <condition-class> HANDLER end` establishes one
  lexical restart for `BODY`.
- `with restart e :: <condition-class> HANDLER` additionally binds the signaled
  condition object as a handler-local name, so handlers can inspect ordinary
  slots such as `e.fallback`.
- A `signal` must appear inside an active lexical handler, and the signaled
  condition class must match the nearest active restart's condition class.
- `signal` resumes with the active restart's result category.
- The handled body and restart handler must produce the same semantic value
  category.
- Semantic IR now has explicit `Signal`, `Handle`, and `Restart` nodes.
- Lowered IR uses an internal lexical restart frame in `BodyLowering`:
  - normal body completion moves into the handle result register;
  - `signal` constructs/evaluates the condition object, moves it into the
    optional handler-local register, and jumps to the restart handler label;
  - the handler computes the replacement value and jumps to the shared join.
- `execution_tape` still receives only ordinary object construction, branches,
  jumps, moves, and arithmetic. There are no condition-specific tape opcodes.
- Session dumps and fingerprints include signal/handle/restart structure, so
  handler body edits affect the expected method-level artifacts.
- The corpus includes:
  - invalid-radius area recovery;
  - checked arithmetic fallback;
  - uncaught signal diagnostics;
  - body/restart result-kind mismatch diagnostics;
  - restart class mismatch diagnostics.
- Incremental tests cover edits to a restart handler in one method and verify
  that the edited method's lowered/tape artifacts recompute while an unrelated
  method's artifacts are reused.
- Restart condition bindings are handler-scoped; using the bound name after the
  handler expression is a normal unresolved-local diagnostic.

### Deferred Condition Design

- Cross-call condition signaling and dynamic handler lookup are deferred.
- Runtime handler stacks and dynamic extent edge cases are deferred.
- Multiple restart parameters and condition payload typing beyond ordinary
  nominal slots are deferred.
- Full effect typing, effect inference, restart selection, stack inspection,
  async interaction, and multiple values are deferred.

### Migration Notes

- Public AST and semantic expression enums gained `Signal` and `Handle`
  variants.
- Semantic IR gained the public `Restart` type.
- `ast::Restart` now records an optional condition binding name, and
  `sem::Restart` records the optional bound condition local.

## Implemented: Richer Runtime Slot Type Model

- Slot declarations may now include an explicit type:
  - `slot radius :: <integer>;`
  - `slot valid? :: <boolean>;`
  - `slot cause :: <some-class>;`
- Untyped `slot name;` declarations remain accepted. They now default to
  Dylan-shaped `<object>` in the current slot-initialization model below.
- The implemented slot value categories are intentionally small:
  - primitive signed integer;
  - primitive boolean;
  - nominal Cairn runtime object with an exact class id.
- Object construction checks each keyword initializer against the declared slot
  kind. Mismatches produce diagnostics such as `slot valid? expects <boolean>
  but initializer has <integer>`.
- Slot reads now return the declared slot kind in semantic IR. `e.fallback`
  returns `<integer>` because the `fallback` slot says so, while `flag.valid?`
  returns `<boolean>` and object-reference slots return the referenced nominal
  object kind.
- Lowered IR still uses runtime object construction and `slot.read`
  instructions, but `slot.read` now records and dumps the returned value kind.
- `execution_tape` emission still uses host calls, not native aggregate opcodes:
  - `cairn.make_object` has per-class host signatures with mixed slot argument
    types;
  - `cairn.slot_get` has per-slot host signatures whose return type matches the
    declared slot kind;
  - `cairn.class_of` remains the runtime support used by generic dispatch.
- The runtime object heap now stores slot payloads as `execution_tape::Value`
  values, so integers, booleans, and Cairn object handles round-trip through the
  host boundary.
- AST, semantic, lowered, and tape dumps expose enough of the model to see
  typed slots and typed slot reads in the workbench.
- Corpus coverage now includes mixed typed slots, object-reference slots, typed
  condition payloads, initializer type mismatches, and unresolved slot types.
- Incremental tests cover slot type edits and verify dependent method/lowered/
  tape artifacts recompute.

### Runtime Object ABI Notes

- Cairn objects are still runtime-managed host objects identified by
  `CAIRN_OBJECT_TYPE`.
- Object construction passes a class id followed by slot payload registers in
  semantic slot order.
- Slot reads pass an object handle and slot id to the host and receive a value
  whose tape type is determined by the slot declaration.
- This ABI is deliberately compatible with Rust/Cairn interop and dynamic
  language features, but it is not a tape-native struct representation.

### Deferred Native-Aggregate Design

- Tape-native structs, sealed/value classes, aggregate layout selection, and
  object representation optimization remain deferred.
- There is still no inheritance-aware slot overriding, parametric slot types,
  union/intersection types, nullable/object optionality, or full type inference.
- Generic function declarations still do not carry explicit return types; this
  slice only makes slot reads and object construction typed.

## Implemented: Dylan-Shaped Slot Initialization

- Untyped slot declarations now default to `<object>`:
  - `slot value;`
  - `slot value :: <object>;`
- Semantic IR distinguishes the top `<object>` value category from nominal
  object handles:
  - `ValueKind::Any` means any first-slice Cairn value;
  - `ValueKind::Object(ClassId)` means a runtime-managed object with a known
    nominal class.
- Existing integer-like examples and corpus programs now spell integer slots
  explicitly, for example `slot radius :: <integer>;`.
- Slot declarations support the first narrow Dylan-style construction keyword
  options:
  - `init-keyword: external-name:`
  - `required-init-keyword: external-name:`
- A slot's construction keyword defaults to `slot-name:`. Renaming the init
  keyword changes object construction, not the slot read name.
- Object construction now diagnoses:
  - missing ordinary slot initializers;
  - missing required init keywords;
  - unknown construction keywords;
  - duplicate construction keywords;
  - initializer type mismatches.
- AST and semantic dumps show declared type, effective init keyword,
  requiredness, and runtime representation. Semantic fingerprints include slot
  type, keyword, and requiredness so the session diff can report metadata
  changes.
- Lowering uses a boxed top-value representation for `<object>` slots rather
  than treating them as nominal object handles:
  - concrete integers, booleans, and nominal object handles are boxed with
    `cairn.box_value`;
  - boxed top values use the host object type `CAIRN_VALUE_TYPE`;
  - nominal instances still use `CAIRN_OBJECT_TYPE`;
  - `cairn.make_object` and `cairn.slot_get` use boxed-any signatures for
    `<object>` slots.
- Incremental coverage now distinguishes two slot metadata cases:
  - slot type edits affect runtime layout and recompute lowered/tape artifacts;
  - init-keyword-only edits change bind-level metadata but can reuse lowered IR
    and tape when the runtime layout is unchanged.
- Corpus coverage includes untyped `<object>` slots, required init keywords,
  renamed init keywords, duplicate keyword diagnostics, missing required keyword
  diagnostics, and the existing primitive/nominal typed slot cases.

### Deferred Slot Initialization Design

- There is no unboxing operation for top `<object>` values yet, so a read from an
  untyped slot has kind `<object>` and cannot be used as an integer without
  future type tests, casts, or flow typing.
- Default values, `init-value:`, `init-function:`, setters, `constant slot`,
  allocation variants, inherited slot specs, and the full Dylan initialization
  protocol remain deferred.
- The boxed-any representation is a first runtime ABI, not a claim about future
  object identity, primitive boxing policy, or optimized layout.

## Implemented: Slot Accessors As Calls

- Each semantic slot now defines a getter accessor whose identity is the slot's
  stable definition ID. This keeps accessor identity stable across harmless
  edits and ties accessor metadata to the slot declaration that created it.
- Surface `receiver.slot` now lowers to semantic `AccessorCall` IR. The semantic
  layer no longer treats slot access as a primitive slot-read expression.
- The explicit accessor spelling `slot-name(receiver)` is supported when no
  direct function or generic with that name takes precedence. `receiver.slot`
  and `slot-name(receiver)` resolve through the same accessor metadata.
- Lowering recognizes known semantic accessor calls and still emits the existing
  lowered `slot.read` instruction. The tape/runtime path is unchanged: this is a
  semantic cleanup plus an inspectable lowering boundary, not a new bytecode
  feature.
- Semantic dumps show `accessor-call` and accessor IDs; lowered dumps continue
  to show optimized `slot.read`.
- Corpus coverage includes both `c.radius` and `radius(c)`, and the incremental
  slot metadata tests now fingerprint accessor metadata through the slot.

### Deferred Accessor Design

- Setters, mutation, inherited accessors, generic function objects, reflective
  accessor enumeration, and optimizer choices beyond direct slot-read lowering
  remain deferred.

## Intentionally Deferred

- Sealed dispatch optimization.
- Parametric types.
- Effects and restarts.
- Refinements.
- Macros.
- Cranelift.
- Full module system.
- General type checking beyond the current primitive, top object, nominal
  object, and slot value categories.
- Multi-argument/multi-method dispatch beyond one nominal object argument.
- Runtime mutation.
- Persistent caches or scheduling; the invalidation shell models dependencies only.
- Watch mode, LSP, and persistent on-disk workbench caches.
- Full multi-file module sessions beyond compiling named source files independently.
- Cross-file/module-stable identity beyond caller-provided `SourceFileId` values.
- Lowered IR optimization passes, canonical basic blocks, and richer type checking.
- Wall-clock artifact timing in the no-std core; the current engine reports
  deterministic logical ticks and work units.
- Query scheduling that avoids recomputing prerequisite fingerprints when
  validating cache reuse.
- Cross-call condition signaling, dynamic handler stacks, and effect typing.

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
- `cargo run -p cairn_corpus` checks the small-program corpus.
