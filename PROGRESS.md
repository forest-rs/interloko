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
  - `dump <file> --stage syntax|ast|sem|mop|eop|lowered|tape|all`
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
  - `dump <file.cairn> --stage syntax|ast|sem|mop|eop|lowered|tape|all` for
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
- `init-function:`, setters, allocation variants beyond instance slots,
  inherited slot specs, and the full Dylan initialization protocol remain
  deferred. `init-value:` and `constant slot` metadata are now implemented in
  the richer slot slice below.
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

## Implemented: Class Names As Class Objects

- Source class names such as `<circle>` now lower to semantic class-object
  values instead of being only raw syntax consumed by `make`.
- Object construction consumes a class-object expression and keeps the known
  class ID for validation and runtime layout. This supports local and direct
  call flow such as `let c = <circle>; make(c, radius: 10)`.
- `class-of(x)` is a semantic expression returning the known class object for a
  known runtime object. Lowering emits the existing class-id operation, and
  generic dispatch continues to use runtime class IDs internally.
- Lowered IR represents known class objects as class IDs with a distinct
  `class(<name>)` value kind. Dumps show semantic `class-object` /
  `class-of` expressions and lowered `const.class` / `class_of` representation.
- Corpus coverage includes direct class-object construction, class objects
  returned through direct calls, and `class-of` feeding `make`.
- Incremental coverage includes a class-reference edit that recomputes the
  dependent lowered module and tape program while reusing unrelated method
  lookup/lowering/tape artifacts.

### Deferred Class Object Design

- Metaclasses, user-defined classes of classes, dynamic unknown class objects,
  reflective class/slot enumeration, class-object mutation, and a full Dylan
  class hierarchy remain deferred.

## Implemented: Typed Direct Function Surface

- Direct function parameters may now carry explicit value-kind annotations:
  - `define function keep?(flag :: <boolean>)`
  - `define function pass(c :: <circle>)`
  - `define function build(klass :: class <circle>)`
- Direct functions may now carry explicit result annotations:
  - `define function keep?() => <boolean>`
  - `define function build() => <circle>`
  - `define function circle-class() => class <circle>`
- Unannotated direct function parameters still default to `<integer>`, keeping
  the existing arithmetic examples source-compatible.
- Unannotated direct function results still use the existing inferred result
  category. Explicit result annotations are checked against the lowered body
  result.
- The first direct-function annotation model supports:
  - `<integer>`;
  - `<boolean>`;
  - `<object>` as boxed top value;
  - exact nominal runtime objects such as `<circle>`;
  - exact class-object values spelled `class <circle>`.
- Semantic declaration pre-pass now records direct function parameter kinds and
  explicit result kinds before lowering function bodies, so direct calls can be
  checked against known signatures.
- Direct function calls are checked at each call site. Mismatches produce
  diagnostics such as `function keep? argument 1 expects <boolean> but got
  <integer>`.
- Lowered IR function signatures now consume the checked semantic kinds.
  Concrete values flowing into `<object>` parameters or returns are boxed with
  the existing `box.any` / `cairn.box_value` runtime path.
- AST, semantic, lowered, and corpus dumps expose annotated parameter and
  result meaning. Stable semantic fingerprints include parameter and explicit
  result categories so signature edits participate in session diffs.
- Incremental coverage includes a signature-only edit from an untyped direct
  function to an `<object>` boundary. The edit recomputes the changed
  whole-module lowering and tape artifacts while reusing unrelated generic
  method lowering/tape artifacts.
- Corpus coverage includes boolean parameters/returns, nominal object
  parameters/returns, class-object returns flowing into `make`, `<object>`
  function boundaries, and argument/return mismatch diagnostics.

### Deferred Direct Function Type Design

- Generic function signature annotations, typed method return annotations,
  optional/rest/keyword parameters, overloads, union types, parametric types,
  full local type inference, dynamic unknown class objects, and dynamic casts or
  unboxing from `<object>` remain deferred.
- Direct function type checking is intentionally exact and local. There is no
  subtyping beyond accepting concrete first-slice values at `<object>` boxing
  boundaries.
- Runtime does not own type checking. It receives already checked lowered IR and
  performs only the existing object/boxed-value host operations.

## Implemented: Typed Generic Contracts

- Generic declarations may now carry explicit parameter and result contracts:
  - `define generic area(x :: <object>) => <integer>;`
  - `define generic keep(x :: <object>) => <object>;`
- Method definitions may now carry explicit result annotations:
  - `define method area(c :: <circle>) => <integer>`
  - `define method keep(c :: <circle>) => <circle>`
- Unannotated generic declarations keep the existing generic-dispatch behavior.
  Their result category remains the first-slice legacy `<integer>` dispatcher
  result.
- The implemented generic parameter contract model is deliberately narrow:
  explicit contracts must be `<object>` or an exact nominal class, because the
  current dispatcher still dispatches over one runtime object argument.
- Method specializers are checked against explicit generic parameter
  contracts. With no inheritance model yet, a nominal generic parameter
  contract accepts only the exact same class; `<object>` accepts any known
  nominal object method specializer.
- Generic call sites are checked against explicit generic parameter contracts.
  A contracted generic call requires a known nominal object argument; boxed top
  `<object>` values must be refined or cast in a later slice before dispatch.
- Method bodies are checked against explicit method result annotations, and
  method results are checked against explicit generic result contracts.
- Lowered dispatcher signatures now use the declared generic result category.
  When a generic returns `<object>` and a method returns a more precise nominal
  object, the dispatcher boxes through the existing `box.any` /
  `cairn.box_value` runtime path.
- AST, semantic, lowered, and corpus dumps expose generic and method contracts.
  Definition and dispatcher fingerprints include contract kinds so signature
  edits participate in session diffs and cache reports.
- Corpus coverage includes typed generic area dispatch, typed generic
  `<object>` results, bad generic call arguments, bad method result contracts,
  and bad method specializers.

### Deferred Generic Contract Design

- Generic dispatch is still one-argument nominal dispatch. Multi-argument
  dispatch, primitive specializers, sealed dispatch optimization, method
  combination, overload resolution, and full generic function type objects
  remain deferred.
- `<object>` generic parameters mean "known runtime object accepted by this
  dispatcher" in this slice, not "any boxed first-slice value". Boxed top values
  require explicit refinement/cast support before they can feed dispatch.

## Implemented: Object Tests, Casts, and Narrow Refinement

- `instance?(value, <class>)` is now recognized as a runtime class-membership
  test expression.
- `as(<class>, value)` is now recognized as a checked object cast/narrowing
  expression.
- Both forms accept semantic `<object>` top values and known nominal object
  values. Primitive values are rejected during semantic lowering.
- Statically impossible nominal checks are diagnosed:
  - `instance?(make(<square>, ...), <circle>)`;
  - `as(<circle>, make(<square>, ...))`.
- The semantic IR records local true-branch refinement for:
  - `if instance?(x, <circle>) then ... else ... end`
- Refined local uses carry their narrowed semantic value category, so
  `<object>` parameters can feed slot reads after an `instance?` guard.
- Lowered IR represents tests and casts with ordinary instructions that emit
  ordinary runtime host calls:
  - `cairn.instance_of`;
  - `cairn.cast_object`.
- No new `execution_tape` generic, cast, or class-test opcodes were added.
- AST, semantic, lowered, and tape dumps expose tests, casts, and refinement.
  Semantic fingerprints include use-site local result kinds and refinements so
  cache reports can see edits that affect narrowing.
- Corpus coverage includes guarded slot access through `<object>`, explicit
  checked casts, impossible nominal tests/casts, and primitive test
  diagnostics.

### Deferred Object Refinement Design

- Refinement is deliberately local and positive only. Else-branch narrowing,
  negative tests, compound boolean conditions, path-sensitive propagation
  across function calls, and generalized local type inference remain deferred.
- `as` is a runtime checked cast from top or nominal object values. It is not a
  general unboxing mechanism for primitive values stored in `<object>`.
- Runtime class tests now follow the single nominal superclass chain.
  Generalized refinement across function calls and compound control flow remains
  deferred.

## Implemented: First-Class Known Callable Objects

- Bare references to direct functions, generic functions, and unambiguous slot
  accessors now lower to semantic known-callable values when no local shadows
  the name.
- Known callables are represented in semantic value categories, so `let`
  bindings can carry identities such as:
  - `function f0`;
  - `generic g0`;
  - `accessor c0.s0`.
- `apply(callable, ...)` is recognized as a narrow built-in call form.
- Applying a known direct function value resolves back to the existing
  semantic `DirectCall` path, including arity and argument-kind checks.
- Applying a known generic function value resolves back to the existing
  semantic `GenericCall` path, including generic contract checks.
- Slot accessor identities are exposed as known callable values, but applying
  them is intentionally diagnosed for now.
- Known callable locals are compile-time-only in lowering. They do not allocate
  lowered registers and do not create runtime `execution_tape` values.
- Lowered IR and tape emission remain unchanged for successful `apply` forms:
  the emitted code is the ordinary direct call or generated generic dispatcher
  call that Cairn already used.
- Semantic dumps and fingerprints expose known-callable identities, including
  local bindings. Incremental coverage shows that changing a callable target in
  one function recomputes the function-level lowered/tape artifacts while
  reusing unrelated method artifacts.
- Corpus coverage includes direct-function `apply`, generic-function `apply`,
  accessor identity as a value, non-callable apply diagnostics, and accessor
  apply diagnostics.

### Deferred Callable Object Design

- There is still no runtime representation for unknown function objects,
  closures, captured environments, dynamic invocation, or accessor invocation
  through `apply`.
- Known callable values cannot cross an ABI boundary or be returned as runtime
  values. They are local compile-time facts consumed by semantic lowering.
- Multi-argument direct-function `apply` is structurally supported by the
  semantic path, but generic dispatch remains limited by the current
  one-argument dispatcher model.

## Implemented: Host-Backed Metaobject Foundation

- `HostMetaRegistry` lets an embedding Rust host declare host-backed classes,
  slots, and functions explicitly. This is metadata only; the registry does not
  inspect Rust values or depend on reflection.
- Host-backed classes and Cairn-native classes share the same nominal
  `ClassId` space. Methods can specialize on either kind, and generic dispatch
  uses the same class-id comparison path for both.
- Semantic classes and slots now carry origin metadata:
  - `native` for Cairn source definitions;
  - `host-backed(hcN)` and `host-backed(hcN.hsN)` for registry declarations.
- Semantic dumps show class and slot origin, so it is visible when a program is
  using a Rust-backed metaobject instead of a Cairn-native definition.
- Host-backed slots have Cairn `ValueKind` declarations and are read-only in
  this slice.
- `make(<host-backed-class>, ...)` is rejected during semantic lowering.
  Host-backed object handles must be supplied by the embedding runtime.
- `CairnHost` can install host-backed object handles through a
  `HostObjectAdapter`. The adapter owns Rust value access and exposes
  read-only slot reads back to the Cairn runtime.
- Host-backed object handles participate in:
  - `<object>` boxing through the host value representation;
  - `instance?`;
  - `as(<host-backed-class>, value)`;
  - slot reads;
  - generic method dispatch.
- Lowered IR continues to expose ordinary model operations such as class tests,
  casts, generic dispatch calls, and `slot.read` before tape emission.
- Tape emission still uses ordinary runtime host calls such as
  `cairn.slot_get`, `cairn.instance_of`, and `cairn.cast_object`. There are no
  host-backed-specific `execution_tape` opcodes.
- Tests include hand-written Rust adapters for small Rust-backed shapes and
  cover slot reads, generic dispatch, `<object>` boxing, casts, invalid slot
  reads, and invalid casts.
- `examples/host_backed_area` is a runnable Rust embedding example. It creates
  Rust-backed circle and square values, installs them into `CairnHost`, and
  runs Cairn generic dispatch over those host-backed object handles.
- `cairn_corpus` supports `// host-class:` headers for compile/dump fixtures,
  and the corpus now includes host-backed slot, dispatch, construction, slot,
  and cast cases.

### Host-Backed Glossary

- **Host-backed class:** a nominal Cairn class declared by the embedding host
  instead of by Cairn source.
- **Host-backed slot:** a read-only Cairn slot whose value is provided by a
  host adapter.
- **Host object handle:** a runtime object handle stored in `CairnHost` that
  points at adapter-owned Rust data.
- **Adapter:** Rust code that maps a Cairn `SlotId` and expected `ValueKind` to
  a runtime value.
- **Registry:** explicit metadata describing host-backed metaobjects before
  semantic lowering.

### Deferred Host-Backed Design

- `facet` integration is deferred. It should later populate
  `HostMetaRegistry`; it should not replace Cairn's object-model semantics.
- Automatic Rust reflection, mutation, Cairn-side construction of host-backed
  values, ownership/lifetime generality, dynamic unknown host classes,
  host-backed generic Rust types, enum/variant modeling, trait discovery,
  dynamic method installation, persistence, async/effects integration, and
  Cranelift remain out of scope.
- Host-backed functions are metadata scaffolding in this slice. Runtime calls
  still go through the existing Cairn lowering and host-call mechanisms.

## Implemented: MOP Inspector Dumps

- `SessionArtifacts::dump_stage(DumpStage::Mop)` renders a read-only
  metaobject protocol summary from the existing semantic module and runtime
  layout.
- The MOP dump exposes:
  - native and host-backed class origins;
  - slots, declared slot kinds, storage ownership, and runtime representation;
  - accessor identities and the slots they read;
  - generic functions, method specializers, and method result categories;
  - generic dispatch rows from nominal class to method;
  - runtime layout class/slot storage kind.
- `cairn_lab dump <file> --stage mop` and `cairn_lab mop <file>` print the MOP
  summary.
- `cairn_corpus` supports `dump mop contains ...` expectations.
- `cairn_lab` compile-only paths understand leading `// host-class:` headers
  as workbench metadata, matching the corpus runner. This remains a tooling
  convention, not Cairn source syntax.

### Migration Notes

- `DumpStage` gained a public `Mop` variant. Exhaustive downstream matches on
  `DumpStage` must handle it.
- CLI stage lists now include `mop`.

## Implemented: Structured Environment Object Protocol Records

- `DESIGN.md` now frames Cairn as a Dylan-shaped semantic kernel for
  executable object systems inside a larger human/agent workspace, with clear
  boundaries around persistence, search, UI, and scheduling.
- `SessionArtifacts::environment_objects()` returns typed EOP records for the
  current compilation artifacts.
- `SessionArtifacts::project_environment()` emits those records through an
  `EnvironmentObjectSink` so tools do not need to parse text dumps.
- `SessionArtifacts::dump_stage(DumpStage::Eop)` renders its text from the
  typed records, preserving the existing workbench/corpus dump behavior.
- The EOP dump is intentionally descriptive. It does not add persistence,
  permissions, UI rendering, search, scheduling, mutation, or a new runtime.
- EOP objects currently cover:
  - source files;
  - classes, slots, accessors, generics, methods, and direct functions;
  - diagnostics;
  - runtime layout rows;
  - query artifacts.
- Each object reports a small protocol vocabulary:
  - `kind`;
  - meta-level `class`;
  - `origin`;
  - `storage`;
  - `presentation`;
  - `affordances`;
  - `execution` when applicable;
  - `inspection`.
- Stable affordance names are modeled by `EnvironmentAffordance`, including
  `inspect`, `dump-syntax`, `dump-ast`, `dump-sem`, `dump-mop`, `dump-eop`,
  `dump-lowered`, `dump-tape`, `show-dispatch`, `show-runtime-layout`,
  `show-dependencies`, `show-accessor`, `show-slot`, `run`, `edit-body`, and
  `add-method`.
- `HostEnvironmentProvider` is an explicit host projection seam. Host adapters
  can contribute host-owned environment records into the same sink without
  making `CairnHost` a world registry or enumerating the runtime heap.
- `cairn_lab dump <file> --stage eop` and `cairn_lab eop <file>` print the EOP
  summary. The workbench still understands leading `// host-class:` metadata so
  host-backed objects are visible in the environment view.
- `cairn_corpus` supports `dump eop contains ...` expectations.

### EOP Glossary

- **Environment object:** anything the workbench can identify and act on, such
  as a method, diagnostic, runtime layout row, or query artifact.
- **Origin:** where the object came from: source, host metadata, compiler
  generation, and eventually agent/import sources.
- **Storage:** who owns the current representation: source text, host adapter,
  registry metadata, or cached compiler artifact.
- **Presentation:** stable text hints that a future inspector or transcript can
  render.
- **Affordance:** a named action that a human, agent, or Portolan subject can
  discover and request.
- **Host projection:** an opt-in Rust adapter hook that contributes host-owned
  environment records. It is not runtime heap discovery.

### Deferred EOP Design

- EOP records are Rust structs, not a stable serialized schema. There is no
  Portolan subject bridge, Understory inspector, persistent image, capability
  policy, transaction model, or agent provenance model yet.
- Future Cairn-level generic functions may mirror these projection protocols,
  but this slice keeps projection at the Rust/session API boundary.

### Migration Notes

- `DumpStage` gained a public `Eop` variant. Exhaustive downstream matches on
  `DumpStage` must handle it.
- The session API now exposes `EnvironmentObject`, `EnvironmentObjectId`,
  `EnvironmentKind`, `EnvironmentOrigin`, `EnvironmentStorage`,
  `PresentationFacet`, `AffordanceFacet`, `ExecutionFacet`, `InspectionFacet`,
  `EnvironmentObjectSink`, and `HostEnvironmentProvider`.
- CLI stage lists still include `eop`.

## Implemented: EOP Workbench Commands

- `cairn_lab inspect <file> <object-id>` reads structured EOP records and prints
  one object with kind, class, origin, storage, presentation, affordances,
  execution, and inspection facets.
- `cairn_lab affordances <file> <object-id>` lists the stable affordance names
  exposed by one object and reports unknown object IDs with available IDs.
- `cairn_lab invoke-affordance <file> <object-id> <affordance>` executes a
  read-only workbench action when the object exposes it.
- Supported read-only invocations map to existing compiler/session views:
  `show-slot`, `show-accessor`, `show-dispatch`, `show-runtime-layout`,
  `show-dependencies`, `dump-syntax`, `dump-ast`, `dump-sem`, `dump-mop`,
  `dump-eop`, `dump-lowered`, `dump-tape`, and `run`.
- `tools/cairn_lab/fixtures/host_projected_environment.cairn` demonstrates an
  explicit host projection provider with a class-like host object and an
  instance-like host object. The provider is installed only from explicit
  `// host-eop-*` fixture metadata.
- `docs/eop_workbench.md` documents how to discover object IDs, inspect records,
  list affordances, invoke read-only views, and inspect host-projected records.

### Workbench Boundary

- `cairn_lab` is a workbench surface, not the long-term UI.
- Host projection is adapter-owned and explicit. `CairnHost` does not enumerate
  runtime heap objects.
- EOP remains structured protocol vocabulary. There is still no Portolan bridge,
  Understory inspector, persistent image, capability policy, mutation
  transaction model, or Cairn-level EOP generic function protocol.

## Implemented: Richer Dylan-Shaped Slots

- Slot declarations now support:
  - untyped slots defaulting to `<object>`;
  - typed slots such as `slot radius :: <integer>;`;
  - renamed init keywords such as `init-keyword: r:`;
  - required init keywords such as `required-init-keyword: radius:`;
  - default initializer expressions with `init-value: <expr>`;
  - `constant slot` metadata.
- Semantic direct slots now record declared type, normalized value category,
  init keyword, required flag, optional default initializer expression, instance
  allocation metadata, mutability metadata, origin, and stable accessor
  identity.
- Object construction now fills omitted slots from `init-value:` expressions.
  Explicit keyword values override defaults. Required init keywords may be
  omitted only when a default exists.
- Construction diagnostics cover unknown init keywords, duplicate init
  keywords, missing required keywords, missing non-defaulted slot initializers,
  explicit initializer type mismatches, and `init-value:` type mismatches.
- `constant slot` is metadata only in this slice. It records construction-time
  assignment intent, but reads lower through the same accessor/slot-read path
  as mutable slots because setters and mutation are not implemented yet.
- MOP dumps, EOP records, and `cairn_lab inspect ... slot:*` now show slot
  type, init keyword, required/default status, allocation, mutability, runtime
  representation, and accessor relationship.
- Incremental fingerprints include slot defaults and slot metadata. Tests cover
  harmless slot declaration trivia preserving definition IDs, init keyword edits
  reusing executable artifacts when layout is unchanged, default-value edits
  recomputing lowered/tape artifacts, and mutability metadata edits reusing
  lowered/tape artifacts.
- The corpus includes pass cases for renamed keywords, `init-value:`, default
  override, required keywords with defaults, object-top defaults, and constant
  slots, plus a failing `init-value:` type mismatch case.
- `docs/slot_model.md` documents supported slot syntax, construction behavior,
  workbench inspection, and deferred full initialization/mutation design.

### Deferred Slot Design

- Slot writes/setters, inherited slot specifications, class allocation, each-subclass
  allocation, virtual/computed slots, full Dylan initialization protocol hooks,
  before/after/around init methods, sealed slot optimization, persistence/image
  storage, host-backed construction, and generalized mutability enforcement are
  still deferred.

## Implemented: AMOP-Informed Class Finalization And Inherited Slots

- AST lowering now preserves the explicit superclass written in
  `define class <child> (<parent>)`. Omitting the superclass list still
  normalizes to the `<object>` root.
- Semantic classes now distinguish:
  - `direct_slots`, which are source-authored facts;
  - effective slots, which are computed finalization facts.
- Class finalization computes effective slots in deterministic layout order:
  parent effective slots first, then the class's direct slots.
- Inherited effective slots preserve the original direct slot/accessor
  definition identity while receiving a subclass-local layout slot. This keeps
  definition IDs stable while allowing the runtime to read the correct child
  object field.
- `make(<child>, ...)` initializes all effective slots. Inherited init keywords,
  required keywords, and `init-value:` defaults work the same way as direct
  slots, and explicit keyword values still override defaults.
- Slot reads and explicit accessor calls resolve against the receiver class's
  effective slots, so `child.parent_slot` and `parent_slot(child)` work for
  inherited slots.
- Finalization diagnostics reject unknown superclasses, primitive
  superclasses, host-backed superclasses, duplicate effective slot names,
  duplicate effective init keywords, and duplicate effective accessors.
- Runtime layout is derived from effective slots. `execution_tape` still sees
  ordinary construction/read host calls; no inheritance-specific tape opcodes
  were added.
- Semantic/MOP dumps and EOP inspection now show superclass, finalization
  status, direct slots, effective slots, direct vs inherited source, source
  slot identity, and effective layout order.
- Incremental tests cover harmless superclass/slot trivia, parent slot edits
  affecting child finalization and executable artifacts, child-only slot edits
  leaving parent definitions unchanged, and adding a subclass while preserving
  parent definition IDs.
- Corpus coverage includes inherited construction/read cases, inherited
  `init-value:`, inherited required keywords, parent+child slots, inherited
  accessor calls, constant inherited slots, and expected superclass/conflict
  diagnostics.
- `docs/mop_class_finalization.md` explains the AMOP-inspired vocabulary and
  the boundary between source facts, computed MOP facts, runtime layout, and
  exposed workbench/EOP views.

### Deferred Class-Finalization Design

- AMOP is vocabulary and architecture guidance here, not CLOS compatibility.
- Dylan inherited slot specifications, user-defined `initialize` methods, and
  full initialization protocol hooks remain deferred.
- Multiple inheritance, class precedence lists, public metaclass protocols,
  user-defined metaclasses, class redefinition, instance obsolescence, method
  combination, setters, sealed slot optimization, persistence, host-backed
  construction, and generalized mutability enforcement remain deferred.

### Migration Notes

- `sem::Class::slots` was renamed to `sem::Class::direct_slots`. Code that
  wants source-authored slot declarations should use `direct_slots`; code that
  wants construction/read/runtime layout should use `effective_slots`.

## Implemented: Nominal Inheritance Semantics And Inspection

- Single inheritance now acts as a nominal subtype relation. Semantic checking
  accepts subclass instances where a parent object is expected for slot
  initialization, direct function parameters/returns, generic call arguments,
  generic method specializers, casts to a parent, and `instance?` tests against
  a parent.
- Restart matching now uses the same relation when a signaled condition is a
  subclass of the active restart condition class. The restart condition binding
  keeps the restart's parent-specializer type.
- Runtime `instance?`, `as`, and object slot storage checks use the emitted
  runtime superclass chain, so parent-typed values work after lowering to tape.
- Generic dispatch now emits ordinary lowered control flow that checks runtime
  class IDs and selects the most-specific applicable method in the single
  superclass chain. There are still no inheritance-specific execution_tape
  opcodes.
- Disjoint sibling classes remain incompatible for statically known
  `instance?` and `as` operations.
- Incremental fingerprints for generic dispatchers include dispatch order and
  method applicability, so adding or editing subtype-relevant definitions
  affects the right artifacts.
- Corpus coverage now includes parent-typed slots with child values, direct
  calls that upcast child values, parent-specialized methods handling child
  instances, most-specific subclass dispatch, subclass condition restarts, and
  sibling impossible-test/cast diagnostics.
- MOP, semantic, EOP, and runtime-layout inspection now report superclass
  chains, direct/effective/runtime slot source, and dispatch applicability. The
  workbench fixtures `inherited_point.cairn`, `inherited_dispatch.cairn`, and
  the inherited parent-slot diff fixtures show how to inspect inherited
  behavior and incremental propagation.
- `docs/mop_class_finalization.md` and `docs/slot_model.md` describe the
  subtype semantics, inspection commands, artifact propagation, and the
  remaining deferred design.

## Implemented: Explainable Generic Dispatch-Site Inspection

- `SessionArtifacts::dispatch_sites()` now walks semantic functions and methods
  and records every `GenericCall` as an inspectable dispatch site.
- Dispatch sites have friendly IDs such as `dispatch:d0` plus stable backing
  keys derived from the source file, enclosing definition, owner-local call ID,
  and generic definition. The record also carries the current source span for
  navigation.
- `SessionArtifacts::explain_dispatch_site("dispatch:d0")` reports the
  enclosing function/method, span, generic contract, first argument's static
  kind, exact class when known, applicable methods in dispatcher specificity
  order, rejected methods with reasons, selected method when statically known,
  the lowered dispatcher name, and related artifact keys.
- Parent-typed and top-object calls are reported as runtime-needed rather than
  pretending the selected method is statically knowable.
- `cairn_lab dispatch <file>` lists dispatch sites, and
  `cairn_lab dispatch <file> dispatch:d0` explains one site.
- EOP projection now includes `dispatch:dN` objects. `inspect <file>
  dispatch:d0` shows the structured record, and `invoke-affordance <file>
  dispatch:d0 show-dispatch` prints the same dispatch explanation.
- Tests cover exact class selection, child-over-parent selection,
  parent-typed runtime dispatch, rejected sibling methods, stable dispatch-site
  IDs across harmless trivia edits, and method/class fact changes affecting the
  expected explanation and artifacts.
- `docs/eop_workbench.md` and `docs/mop_class_finalization.md` document how to
  list and inspect dispatch sites.

### Dispatch-Site Boundaries

- Dispatch-site inspection is read-only and explains the existing semantic and
  lowered model.
- Runtime dispatch is still ordinary lowered control flow plus ordinary calls;
  no execution_tape dispatch opcode was added.
- Multi-argument dispatch, method combination, sealed dispatch optimization,
  parametric dispatch, and a public AMOP-level dispatch protocol remain
  deferred.

### Migration Notes

- `EnvironmentKind` has a new `DispatchSite` variant with stable text label
  `dispatch-site`.

## Implemented: Initialization Plans For Dylan-Style Initialize Substrate

- Each finalized semantic class now has an `InitializationPlan` derived from its
  effective slots. The plan records deterministic construction order, source
  slot identity, accepted init keyword, value kind/type name, inherited/direct
  source, requiredness, default presence, and explicit-argument override
  behavior.
- Semantic `make(<class>, key: value, ...)` now records explicit `InitArg`
  entries at the call site before choosing planned field initializers. The
  chosen field initializers distinguish explicit values from default
  `init-value:` expressions.
- Construction validation now separates unknown keywords, duplicate keywords,
  missing required keywords, explicit initializer type mismatches, and default
  `init-value:` type mismatches.
- Initialization order is inherited effective slots in layout order, followed
  by direct effective slots in layout order. Explicit init args override
  defaults, and required slots without explicit/default values diagnose.
- Lowered IR object construction consumes the class initialization plan rather
  than iterating effective slots ad hoc. The execution_tape path remains
  ordinary runtime object construction and host calls; no initialization or
  object-aggregate tape opcode was added.
- Semantic dumps, MOP dumps, EOP class records, and `cairn_lab inspect <file>
  class:cN` expose initialization plans, accepted keywords, required/default
  status, slot order, and explicit-argument override behavior.
- Inspection explicitly reports `initialize-methods=deferred`. This slice is
  not avoiding Dylan-style initialization; it is the substrate future
  `make`/`initialize` semantics can consume.
- Tests cover inherited initialization order, explicit overrides, required
  inherited keywords, unknown/duplicate keywords, explicit and default
  initializer type mismatches, stable init facts across trivia edits, and
  expected artifact changes after slot init metadata edits.
- `docs/mop_class_finalization.md`, `docs/slot_model.md`, and
  `docs/eop_workbench.md` document the current init-plan model, how to inspect
  it, how it maps toward Dylan `make`/`initialize`, and why user-defined
  `initialize` remains deferred.

### Deferred Initialization Design

- User-defined `initialize` methods, `make` as an overridable generic function,
  custom allocation protocols, keyword forwarding beyond slot initialization,
  `next-method`, method combination, reinitialization, class redefinition,
  obsolete instances, slot setters, multiple inheritance, and host-backed
  construction remain deferred.
- Before user-defined initialization is enabled, Cairn still needs decisions
  about method ordering, keyword consumption/forwarding rules, allocation hooks,
  interaction with setters/mutation, and how initialization participates in MOP
  inspection and incremental invalidation.

## Implemented: Catalog-First Diagnostic Model

- `cairn_compiler::diagnostic::Diagnostic` now carries stable diagnostic facts:
  code, severity, span, labels, structured args, and notes. It does not carry a
  rendered English message.
- `DiagnosticCode` provides stable code strings such as
  `sem.unresolved-local`, `sem.missing-required-init-keyword`,
  `sem.slot-initializer-type-mismatch`, `syntax.expected-expression`,
  `lower.invalid-semantic`, `tape.build-error`, and `runtime.trap`.
- Lowering, tape emission, driver/runtime, and session runtime outcomes expose
  stable error codes for non-source diagnostic failures.
- Diagnostic fingerprints and logical work counters now include structured
  code/arg/label facts instead of rendered prose.
- EOP diagnostic records expose code, severity, span, labels, and args, without
  rendered text. `cairn_lab inspect <file> diagnostic:dN` shows those fields,
  and CLI diagnostic printing includes the code beside catalog-rendered text.
- `tools/cairn_diagnostics` owns MessageFormat catalog loading and rendering.
  Rendering is strict: missing catalog entries, missing required args, and
  MessageFormat failures are presentation errors rather than silent fallbacks.
- Built-in English and pseudo catalogs cover every stable diagnostic code.
  `cairn_lab` selects them with `CAIRN_DIAGNOSTIC_LOCALE=en|pseudo`, or loads an
  override TOML resource through `CAIRN_DIAGNOSTIC_CATALOG`.
- `cairn_corpus` supports `// expect: diagnostic code ...` while keeping
  `diagnostic contains ...` expectations for catalog-rendered English checks.
- Corpus coverage now includes representative code expectations for syntax,
  AST, semantic, control-flow, restart, and initialization diagnostics.
- `docs/diagnostics.md` explains the boundary between diagnostic facts and
  presentation, how to add/override catalogs, and why artifact identity is
  language-neutral.

### Deferred Diagnostic Design

- Full localization beyond English and pseudo, a `message-format` dependency in
  `cairn_compiler`, terminal snippet rendering, LSP adaptation, fix-it edits,
  warning severities, and generated diagnostic schemas remain deferred.

## Implemented: First Richer Value Types

- Cairn now parses and lowers string, symbol, keyword, and immutable vector
  literals:
  - strings use `"..."` with `\n`, `\t`, `\"`, and `\\` escapes;
  - symbols use `#name`;
  - keyword values use `#:name`, keeping them distinct from call/init keyword
    arguments such as `name:`;
  - vectors use `[a, b, c]`.
- Semantic IR has explicit `ValueKind` coverage for `<string>`, `<symbol>`,
  `<keyword>`, and `<vector>`, backed by built-in value classes so generic
  dispatch and `instance?` can treat them as object values.
- Symbols and keywords are interned by the runtime host for stable per-session
  identity. Strings are runtime objects compared by content. Vectors are
  immutable ordered runtime objects; general sequence operations remain
  deferred.
- Lowered IR contains inspectable literal instructions and runtime value
  equality. Tape emission still uses ordinary constants, calls, and host calls;
  there are no special value-type opcodes.
- Slots, direct functions, generic contracts, and method specializers can use
  `<string>`, `<symbol>`, `<keyword>`, and `<vector>` annotations.
- Session, MOP, EOP, lowered, and tape dumps expose the new built-in value
  classes and literal forms.
- `examples/rich_values`, `tools/cairn_lab/fixtures/rich_values.cairn`, REPL
  transcript coverage, and corpus programs demonstrate strings, symbols,
  keywords, vectors, typed slots/functions, and generic dispatch.
- Diagnostics cover malformed string literals and unsupported vector equality
  through structured diagnostic codes.

### Deferred Value-Type Design

- Hash tables, a sequence library, mutable vectors, string interpolation,
  Unicode normalization, locale-sensitive string behavior, reader macros,
  package-qualified symbols, persistent cross-session interning, and optimizer
  passes remain deferred.

## Intentionally Deferred

- Sealed dispatch optimization.
- Parametric types.
- Effects and restarts.
- Generalized refinements beyond local positive `instance?` true branches.
- Macros.
- Cranelift.
- Full module system.
- General type checking beyond the current primitive, top object, nominal
  object, and slot value categories.
- Multi-argument/multi-method dispatch beyond one nominal object argument.
- Multiple inheritance, method combination, sealed dispatch optimization, and
  parametric types.
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
- Automatic host reflection through `facet`.
- Host-backed mutation, construction, generic Rust types, enum/variant
  modeling, trait discovery, and dynamic host class installation.

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
- `cargo run -p cairn_host_backed_area_example` prints `74`.
- `cargo run -p cairn_corpus` checks the small-program corpus.
