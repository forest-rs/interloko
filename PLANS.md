# Plans

## Add Dylan-Style Writable Slot Assignment

### Goal

Add `obj.slot := value` as Dylan-style setter sugar backed by compiler-generated
slot setter generics and methods. Mutable native slots become writable through
the same generic-call surface users can inspect and call explicitly.

### Fence

Syntax owns recognition of the `:=` surface form. Semantic lowering owns setter
protocol facts and diagnostics. Lowered IR, tape emission, and runtime host
support own mutation of native object storage. This slice explicitly does not
own generalized setter places, user-defined setter methods, writable
host-backed slots, transactions, persistence, or optimizer removal of setter
generic overhead.

### Invariants

- Getter accessors keep their current behavior.
- A mutable native slot synthesizes a visible setter generic named `<slot>-setter` with
  `(new-value, object)` argument order and a setter method returning
  `new-value`.
- Constant slots and host-backed slots remain read-only.
- Assignment and explicit setter-call spelling share the same semantic path.
- Tape uses ordinary host calls for slot writes; no special tape opcode is
  introduced.

### Steps

1. Extend syntax/AST for `obj.slot := value` while leaving generalized setter
   syntax deferred.
2. Synthesize setter metaobjects during semantic collection/finalization and
   route assignment/calls through them.
3. Add semantic diagnostics for read-only, unknown, non-object, host-backed, and
   value-type mismatch writes.
4. Lower native slot writes, emit a runtime `slot_set` host call, and mutate
   native object storage.
5. Expose setter protocol facts in dumps/EOP enough to inspect them.
6. Add corpus and unit/CLI coverage, update progress notes, then run final
   gates and commit.

### Risks

- Existing generic dispatch is first-argument dispatch only; setter lowering
  must preserve the user-visible generic shape without pretending full
  multi-argument dispatch exists.
- Mutable runtime storage must reject boxed/top values and typed values with the
  same value-kind checks used for construction.
- MOP visibility should be honest about compiler-synthesized setters and
  deferred user-defined setter methods.

## Add Richer Value Types

### Goal

Add Cairn's first non-numeric language values: strings, symbols, keywords, and
simple immutable vectors. These values must flow through syntax, AST, semantic
IR, lowered IR, tape/runtime execution, dumps, examples, corpus, and
diagnostics.

### Fence

`cairn_compiler` owns source syntax, semantic value kinds, lowered value
operations, runtime representation, and tape emission for these values. It
explicitly does not own general collection libraries, mutable sequence
operations, persistent cross-session interning, package-qualified symbols,
string interpolation, locale semantics, or optimizer work.

### Invariants

- String escape decoding is explicit and diagnosed for malformed literals.
- Symbols and keyword values have stable per-session identity and are distinct
  from function/generic keyword arguments.
- Vectors are immutable ordered values in this slice.
- Slot storage and function/generic contracts use the same `ValueKind`
  machinery as existing integers, booleans, objects, and classes.
- Tape lowering uses ordinary constants/runtime calls/host calls, not new
  special-purpose generic opcodes.

### Steps

1. Extend lexer/parser/AST for strings, symbols, keywords, and vector literals
   with focused diagnostics.
2. Extend semantic value kinds, type annotations, slot/function/generic
   checking, equality rules, and dumps.
3. Extend lowered IR and tape/runtime representation for executable values.
4. Add corpus, examples, session/REPL/workbench coverage, and progress notes.
5. Keep commits coherent: syntax/AST, sem/diagnostics, lower/runtime, docs/tests.

### Risks

- Vectors can easily become a general sequence library; this pass only needs
  literals, storage, passing, returning, and display/dumps.
- Equality should be conservative: only define cases with clear identity/value
  semantics.
- Keep interning deterministic inside a compiled module/session, not globally
  persistent.

## Add Initial Cairn REPL Tool

### Goal

Add a plain-stdio `cairn_repl` tool that drives the existing incremental
session API, supports durable definitions plus generated scratch expressions,
and exposes the same stage/artifact/MOP/EOP inspection surfaces as the
workbench.

### Fence

`cairn_repl` owns terminal interaction, input buffering, scratch-source
generation, and command routing. It explicitly does not own parsing, binding,
runtime dispatch, diagnostics rendering policy, MOP/EOP construction, artifact
graph logic, persistent image state, or language semantics.

### Invariants

- Definitions append to one durable in-memory source buffer with a stable
  `SourceFileId`.
- Expression input compiles durable source plus one generated scratch function;
  scratch definitions are replaced each time.
- All compilation still flows through source -> syntax -> AST -> semantic IR ->
  lowered IR -> tape.
- Named scratch execution uses a small compiler/session runtime API, not a
  fake `main`.
- REPL diagnostics render through `cairn_diagnostics`.
- The first slice uses plain stdin/stdout, with no line-editing dependency.

### Steps

1. Add a compiler/session API for running a named zero-argument function from
   existing `SessionArtifacts`.
2. Add `tools/cairn_repl` to the workspace with a small state machine for
   durable source, pending multiline definitions, scratch expressions, and
   colon commands.
3. Reuse `SessionArtifacts` dumps, dispatch inspection, artifact reports, and
   environment objects for commands.
4. Add transcript-style CLI coverage for definitions, expression evaluation,
   source/dump/artifact commands, and quit/clear behavior.
5. Document the workflow and current limits, then run fmt, clippy, and tests.

### Risks

- Do not accidentally treat expression evaluation as top-level mutation.
- Do not duplicate `cairn_lab` rendering logic beyond small text formatting
  helpers.
- Keep result printing honest: integer results print directly; non-integer or
  missing-function cases report runtime errors.

## Make Diagnostics Catalog-First

### Goal

Finish the diagnostic localization boundary by making compiler diagnostics
language-neutral facts only. User-facing wording moves to MessageFormat
catalogs loaded by presentation tools.

### Fence

`cairn_compiler` owns diagnostic codes, arguments, spans, labels, notes, and
artifact identity. It explicitly does not own MessageFormat compilation, TOML
resource loading, selected language/catalog state, or fallback English text.
`cairn_diagnostics` owns catalog-backed rendering for tools such as
`cairn_lab` and `cairn_corpus`.

### Invariants

- `Diagnostic` has no embedded rendered message.
- Producer sites emit a stable `DiagnosticCode` plus structured args.
- Rendered wording is never part of artifact fingerprints.
- Missing catalog entries, missing args, and formatting failures are hard
  presentation errors.
- English and pseudo catalogs both cover every stable diagnostic code.
- EOP diagnostic records expose facts, not pre-rendered text.

### Steps

1. Remove `Diagnostic::message` and migrate producer callsites to code/arg
   constructors.
2. Update session fingerprints, work counters, EOP diagnostic projection, and
   corpus checks so they use diagnostic facts and catalog rendering where text
   is intentionally tested.
3. Make `cairn_diagnostics` strict, add pseudo catalog coverage, and expose
   explicit catalog/locale selection for `cairn_lab`.
4. Add tests for code/arg-only rendering, loud missing catalog/arg failures,
   pseudo rendering, and language-independent artifact identity.
5. Update docs, then run fmt, clippy, workspace tests, and docs.

### Risks

- Some existing tests assert English strings directly; migrate them to codes
  and args unless the test is specifically about presentation.
- Syntax diagnostics may still carry syntactic fragments as args, but not full
  fallback sentences.
- Keep `message-format` out of compiler crates.

## Build Structured Diagnostics And MF2 Localization Boundary

### Goal

Replace string-only compiler diagnostics with stable diagnostic facts that can
still render the current English messages. Prepare a clean boundary for a later
MessageFormat 2 renderer without adding `message-format` as a core compiler
dependency.

### Fence

`cairn_compiler::diagnostic` owns diagnostic identity, spans, labels, arguments,
and the default English rendering contract. It explicitly does not own localized
catalog loading, terminal snippet styling, LSP adaptation, fix-it edits, or
MessageFormat 2 runtime integration.

### Invariants

- Diagnostic codes are stable enough for tests, corpus expectations, EOP
  records, and tools.
- This earlier migration kept `Diagnostic::message` temporarily; the later
  catalog-first plan above removes it.
- Structured arguments use named values rather than parsing prose.
- Labels describe source spans; the primary label remains the diagnostic span.
- EOP diagnostic objects expose both facts and rendered text.
- `cairn_compiler` must remain free of a production dependency on
  `../message-format`.

### Steps

1. Add `DiagnosticCode`, diagnostic args, labels, notes, and render helpers in
   `cairn_compiler::diagnostic`.
2. Provide constructors/builders so new diagnostics can be structured while old
   string callers still compile.
3. Migrate syntax, AST, semantic, lowering, tape, runtime/session diagnostic
   creation to stable codes and structured args.
4. Teach diagnostic fingerprints, work counters, EOP projection, dumps, and
   `cairn_lab` output to include codes and structured facts.
5. Extend `cairn_corpus` to support `// expect: diagnostic code ...` while
   keeping `diagnostic contains ...`.
6. Add focused unit/CLI/corpus coverage for code expectations, args, labels,
   rendered compatibility text, and EOP diagnostic records.
7. Document diagnostic facts vs presentation and the future MF2 renderer/catalog
   boundary, then run all final gates and examples.

### Risks

- Migrating every diagnostic in one pass can become noisy; keep helper
  constructors small and mechanical.
- Codes must be named semantically, not after current English wording.
- The MF2 boundary should stay documented, not implemented, in this slice.

## Build Initialization-Plan Substrate For Dylan-Style Initialize

### Goal

Make object construction use explicit class initialization plans and init-argument
records. Preserve today's slot-initialization behavior, but represent it as a
computed MOP fact that future Dylan-style `make`/`initialize` can consume.

### Fence

Semantic lowering owns computed initialization plans as MOP facts derived from
finalized effective slots. Lowering and runtime construction consume those
plans. This slice explicitly does not own user-defined `initialize`, overridable
`make`, custom allocation, init-arg forwarding, `next-method`, method
combination, reinitialization, class redefinition, slot setters, multiple
inheritance, sealed optimization, or new execution_tape opcodes.

### Invariants

- Direct slots are source facts.
- Effective slots are finalized class facts.
- Initialization plans are computed class/MOP facts.
- Init-argument records are semantic call-site facts for `make`.
- Runtime construction follows the initialization plan.
- `execution_tape` still sees ordinary object construction/runtime calls.
- Inspection must say user-defined `initialize` is deliberately deferred, not
  rejected as a direction.

### Steps

1. Add semantic `InitializationPlan`, plan-step, and init-argument records.
2. Refactor `make` validation so explicit args, defaults, required checks, and
   diagnostics are classified through the plan.
3. Add semantic tests for inherited order, explicit overrides, required
   inherited keywords, unknown/duplicate keywords, explicit/default type
   mismatch, and stable plan facts across trivia edits.
4. Commit the compiler/session model and validation cleanup.
5. Change lowered object construction to consume initialization-plan field order
   instead of iterating class effective slots ad hoc.
6. Add lowering/runtime tests for construction order and artifact changes after
   init metadata edits, then commit the lowering slice.
7. Add MOP/EOP/class inspection output for initialization plans, accepted
   keywords, defaults, required args, and override behavior.
8. Update docs and progress notes with the Dylan `make`/`initialize` boundary,
   then run final gates and examples.

### Risks

- The plan should not duplicate source slot facts so heavily that future
  metaclass work has two authorities.
- Diagnostics must stay specific enough for existing corpus expectations while
  separating unknown, duplicate, missing, explicit type, and default type
  failures.
- Dynamic class-object construction must still work when the class is statically
  known through `class <T>` values.

## Build Explainable Generic Dispatch-Site Inspection

### Goal

Make each semantic generic call inspectable as a dispatch-site artifact that
connects the call site, generic contract, argument facts, applicable/rejected
methods, lowered dispatcher order, and related query artifacts.

### Fence

The session/workbench layer owns dispatch-site inspection records and text
explanations. Semantic lowering still owns `GenericCall` resolution, and lowering
still owns executable dispatcher control flow. This slice does not add language
semantics, optimizer behavior, sealed dispatch, method combination,
multi-argument dispatch, or new execution_tape opcodes.

### Invariants

- A generic function is a metaobject.
- A method is a metaobject.
- A dispatch site is a call-site artifact, not a new language object.
- Runtime dispatch remains ordinary lowered control flow plus calls.
- Dispatch-site inspection must explain the current one-argument nominal model
  without implying broader dispatch features.
- Friendly IDs such as `dispatch:d0` are stable for unchanged call ordering;
  stable backing keys include source file, enclosing definition, owner-local
  call id, and generic definition, while records still report the current source
  span for navigation.

### Steps

1. Add public session dispatch-site records and explanation data by walking
   semantic functions/methods for `GenericCall` expressions.
2. Add specificity/applicability/rejection classification using existing
   nominal subclass helpers.
3. Add unit tests for exact selection, child-over-parent selection, parent/top
   runtime dispatch, sibling rejection, stable IDs across trivia, and changed
   method/class facts affecting explanations/artifacts.
4. Commit the compiler/session model slice.
5. Add `cairn_lab dispatch <file>` and `cairn_lab dispatch <file> <id>`, plus
   EOP `dispatch:dN` records and `inspect` support.
6. Add workbench fixtures/tests and commit the CLI/workbench slice.
7. Document how to list/inspect dispatch sites in MOP/EOP docs and progress
   notes, then run all final gates and examples.

### Risks

- IDs must be stable enough for harmless edits without overpromising stability
  after reordered or newly inserted call sites.
- Static selection must clearly distinguish exact/child-known calls from
  parent/top-object calls that need runtime dispatch.
- Explanations should name rejected methods without implying multi-method or
  method-combination semantics.

## Make Inheritance Semantically Real And Inspectable

### Goal

Make Cairn's existing single-inheritance class finalization affect semantic type
checks, runtime predicates/casts, generic dispatch, and workbench inspection,
without changing effective slot layout or adding inheritance-specific tape
operations.

### Fence

Semantic lowering owns nominal subtype facts and static compatibility checks.
Runtime layout and lowering consume those facts for existing predicates, casts,
slot access, calls, and dispatch. This slice does not own multiple inheritance,
method combination, sealed dispatch optimization, parametric types, host-backed
inheritance, or new execution_tape opcodes.

### Invariants

- `ast::Class::slots` remains source syntax.
- `sem::Class::direct_slots` remains resolved direct source facts.
- `sem::Class::effective_slots` remains finalized layout facts.
- Runtime layout still comes from effective slots.
- Subtyping is nominal and follows one superclass chain.
- Generic dispatch uses ordinary lowered control flow and calls.
- Host-backed classes remain non-constructible and not inheritance parents.

### Steps

1. Add semantic same-or-subclass helpers and regression tests for parent/child
   compatibility.
2. Teach static checks for slots, direct calls, returns, generics, casts,
   `instance?`, and restarts to use the helper where it is semantically valid.
3. Teach runtime predicates/casts/slot checks and lowered generic dispatch to
   use most-specific single-inheritance applicability.
4. Add unit, driver, corpus, and incremental coverage for the new subtype
   behavior and impossible sibling cases.
5. Commit the semantic/runtime/lowering slice.
6. Improve dumps for superclass chains, direct/effective/runtime slots, dispatch
   applicability, and inheritance-related artifact propagation.
7. Add workbench fixtures/tests and update docs/progress notes, then commit the
   inspection slice.
8. Run fmt, clippy, workspace tests, docs, corpus, and examples.

### Risks

- Static subtype acceptance must not accidentally allow arbitrary downcasts or
  unrelated classes.
- The dispatcher must pick the child method before a parent method without
  implying method combination.
- Dumps should expose useful propagation facts without creating a new CLI shape
  unless the existing commands cannot carry the information.

## Build AMOP-Informed Class Finalization And Inherited Slots

### Goal

Add a narrow, AMOP-informed class finalization slice: source classes may name a
single superclass, semantic lowering computes direct and effective slots, and
construction/reads/runtime layout consume the effective slot layout.

### Fence

AST and semantic lowering own class-source facts and computed finalization
facts. Runtime layout, lowered IR, tape emission, and EOP/MOP dumps are views or
consumers of those finalized facts. This slice does not implement a public AMOP
API, metaclasses, class redefinition, multiple inheritance, setters, host-backed
construction, persistence, or new tape opcodes.

### Invariants

- `<object>` remains the root/default superclass.
- Direct slots are source-authored facts with stable direct IDs.
- Effective slots are computed facts: parent effective slots first, then direct
  slots.
- Inherited effective slots preserve the original direct slot/accessor
  definition identity while receiving a deterministic layout position in the
  subclass.
- Construction and slot reads operate on effective slots.
- Dumps and EOP inspection make source facts vs computed facts explicit.
- Host-backed classes remain non-constructible and are not inheritance parents
  in this slice.

### Steps

1. Thread superclass syntax through AST and semantic class metadata.
2. Resolve and finalize native classes, computing effective slots and duplicate
   effective slot/init-keyword/accessor diagnostics.
3. Teach construction, accessor lookup, lowered IR, runtime layout, dumps, and
   EOP inspection to use finalized effective slots.
4. Add corpus, unit, incremental, and CLI coverage for inherited construction,
   inherited reads/accessors/defaults/required keywords, and error cases.
5. Add AMOP/finalization docs, update slot/progress notes, run all gates, and
   commit in coherent slices.

### Risks

- Effective slot IDs must be layout IDs, while definition/accessor identity
  stays tied to the source slot.
- Parent changes must affect child finalization without making child-only edits
  appear to mutate the parent.
- AMOP vocabulary must clarify architecture without implying CLOS/Dylan
  compatibility or an exposed metaclass protocol.

## Build Richer Dylan-Shaped Slots

### Goal

Extend slots from simple typed fields into explicit object-model facts with
construction keywords, default initializer expressions, and constant-slot
metadata, while keeping the existing construction and slot-read lowering path.

### Fence

Syntax/AST/semantic lowering own slot declaration meaning. Lowering/runtime own
the existing representation path for construction and reads. This slice
explicitly does not own setters, inherited slots, full Dylan initialization,
allocation variants beyond instance slots, persistence, or new tape opcodes.

### Invariants

- Untyped slots still normalize to `<object>`.
- Slot options are checked semantically before lowered IR is emitted.
- `constant slot` is metadata plus construction-time assignment only.
- Missing slot values are supplied only by explicit `init-value`; there is no
  implicit unbound value.
- Text dumps, EOP records, and workbench inspection are views over semantic
  slot facts.

### Steps

1. Extend syntax and AST for `constant slot` and `init-value: <expr>`.
2. Add semantic allocation/mutability/default metadata to slots.
3. Fill omitted construction fields from slot `init-value` expressions and
   type-check explicit/default initializers.
4. Update semantic, MOP, EOP, fingerprints, and workbench-visible inspection
   details.
5. Add corpus, unit, incremental, and CLI coverage for renamed keywords,
   defaults, required slots, object-top defaults, constant slots, and errors.
6. Update progress notes and run fmt, clippy, workspace tests, docs, examples,
   and corpus.

### Risks

- Default expressions should not imply a full init protocol or method hooks.
- Constant-slot metadata should not imply setter/mutation enforcement before
  setters exist.
- Slot fingerprinting must include defaults and metadata so incremental reports
  do not claim reuse after semantic slot changes.

## Make EOP Workbench Commands Usable

### Goal

Add a small read-only environment workbench surface to `cairn_lab`: inspect one
EOP object, list its affordances, invoke safe view-producing affordances, and
demonstrate explicit host projection through a tool fixture.

### Fence

`cairn_lab` owns interactive/workbench command glue over structured EOP records.
The compiler owns the records. Host adapters own explicit host projection.
This slice explicitly does not own Portolan subjects, Understory presentation,
persistence, automatic host reflection, runtime heap enumeration, mutation, or
authorization policy.

### Invariants

- Inspect/affordance commands read `EnvironmentObject` records, not text dumps.
- Affordance execution is read-only and maps to existing dumps/views.
- Host-projected records appear only when an explicit workbench provider is
  installed by fixture metadata.
- `CairnHost` remains an execution host, not an environment registry.
- Existing `eop` and corpus behavior stays intact.

### Steps

1. Add a `cairn_lab` workbench wrapper that compiles source and gathers
   compiler records plus optional host-projected records.
2. Add `inspect`, `affordances`, and read-only `invoke-affordance` commands.
3. Add explicit fixture metadata and a provider for host class-like and
   instance-like records.
4. Add CLI tests covering object kinds, affordance listing/execution, unknown
   IDs, and opt-in host projection.
5. Add usage docs and progress notes, then run fmt, clippy, tests, docs,
   examples, and corpus.

### Risks

- If command output becomes too clever, it will look like a UI contract instead
  of a workbench inspection tool.
- If fixture metadata is confused with Cairn syntax, the language boundary gets
  muddy.
- If invoke support includes mutation affordances, this slice overreaches.

## Build Structured EOP Records and Host Projection Seam

### Goal

Turn the Environment Object Protocol from a text-only dump into typed compiler
records that tools can inspect directly. Keep the existing `cairn_lab eop` and
`cairn_corpus` behavior by rendering the text dump from those records.

### Fence

The compiler/session layer owns structured projection of compiler-known
environment facts. It explicitly does not own persistent world state, automatic
host discovery, runtime heap enumeration, Portolan subjects, Understory views,
or authorization policy.

### Invariants

- EOP records describe facts that already exist in the compiler, runtime layout,
  host metadata, diagnostics, and query graph.
- Text dumps are views over typed records; tests should not need to parse text
  to verify the protocol.
- `CairnHost` remains an execution host, not an environment registry.
- Host projection is explicit and adapter-owned.
- MOP answers object-system questions. EOP answers environment/tooling
  questions.

### Steps

1. Add public EOP record types, stable affordance names, a sink API, and session
   projection entry points.
2. Refactor the existing EOP dump to render from structured records while
   preserving CLI/corpus output.
3. Add an explicit host projection trait and a test provider that contributes
   host-owned environment objects without heap enumeration.
4. Add direct structured-record tests for native compiler records, host-backed
   compiler records, and host-projected records.
5. Update progress/design notes and run fmt, clippy, workspace tests, docs,
   `cairn_lab`, and `cairn_corpus`.

### Risks

- If host projection reaches into `CairnHost`, EOP becomes runtime enumeration
  rather than adapter-owned projection.
- If records imply a stable serialized schema, this slice will overpromise.
- If affordance names are just ad hoc strings, downstream tools will have to
  parse fragile text.

## Build Host-Backed Metaobject Foundation

### Goal

Introduce a narrow host-backed metaobject path so Rust-side adapters can
declare nominal Cairn classes and expose read-only slots without making Rust
objects Cairn-native heap objects. Host-backed classes should participate in
the same semantic class IDs, slot reads, `instance?`, `as`, and generic method
dispatch as native Cairn classes.

### Fence

Semantic IR owns nominal metaobject facts, class/slot origin, type categories,
and dispatch meaning. Runtime owns object handles, host-backed object identity,
and host-call execution. Adapter code owns Rust value access. Facet is deferred:
it may later populate the same registry, but it does not replace Cairn's MOP.

### Invariants

- The user-facing concept is host-backed, not foreign.
- Native and host-backed classes share the same `ClassId` space.
- Host-backed classes cannot be constructed by Cairn `make` in this slice.
- Host-backed object handles are supplied by the embedding Rust host.
- Slot access is read-only and adapter-mediated.
- Lowered IR keeps class tests, casts, and slot reads as explicit model
  operations before tape emission.
- Tape emission uses ordinary host calls; no new tape opcodes.
- No `facet` dependency, automatic reflection, mutation, generic Rust types, or
  ownership/lifetime generality.

### Steps

1. Add semantic host-backed metaobject declarations:
   - `HostMetaRegistry`, `HostClass`, `HostSlot`, and IDs;
   - class/slot origin metadata on semantic classes and slots;
   - `sem::lower_with_host_registry` and compiler/session entry points that
     accept a registry.
2. Teach lowering and runtime layout about class/slot origin:
   - host-backed classes appear in dumps and runtime layout;
   - `make` rejects host-backed classes before lowering;
   - native class behavior stays unchanged.
3. Add runtime host object support:
   - public adapter trait for read-only Rust-side slot access;
   - public API to install a host-backed object handle in `CairnHost`;
   - `class_of`, `instance?`, `as`, slot reads, and `<object>` boxing work for
     host-backed handles.
4. Add execution/testing helpers:
   - compile with registry;
   - run named functions with explicit argument values in tests.
5. Add tests, corpus/fixtures where possible, dumps, fingerprints, and progress
   notes explaining what is real and how `facet` can later feed the registry.

### Risks

- If host-backed values become constructible too early, runtime ownership and
  lifetime policy will leak into the compiler.
- If registry metadata bypasses semantic class IDs, native and host-backed
  dispatch will split into two systems.
- Trait-object adapters must stay runtime-only so the core compiler remains
  no-std-friendly and dependency-light.

## Make Typed Object/Function Model Compose

### Goal

Add three narrow composition features as separate commits: typed generic
contracts, local object refinement, and statically known callable values. The
goal is to make existing class/object/function machinery compose without adding
closures, dynamic callable invocation, overloads, or new tape opcodes.

### Fence

AST owns only narrow surface syntax. Semantic lowering owns contract meaning,
local refinement state, known callable identity, and diagnostics. Lowering and
tape emission consume resolved semantic kinds and known call targets. Runtime
provides ordinary host support only where a dynamic class test/cast needs it; it
does not own static type checking.

### Invariants

- Existing examples and unannotated generic programs still compile.
- Direct-function type behavior remains unchanged.
- Generic contracts are collected before checking methods and call sites.
- Method return annotations are checked, and generic contracts constrain
  method result kinds when declared.
- `instance?` refinement is local to the true branch of the matching `if`.
- `<object>` values become useful through explicit tests/casts, not implicit
  unboxing.
- Known callable values stay statically known; no closures or unknown dynamic
  apply are introduced.
- Lowered IR and tape use existing calls, branches, and runtime calls.

### Steps

1. Typed generic contracts:
   - parse and AST-lower generic parameter/result annotations and method return
     annotations;
   - collect generic contract kinds before lowering methods and bodies;
   - check method specializers/results and generic-call arguments;
   - update dumps, fingerprints, tests, corpus, and progress notes.
2. Object tests, casts, and refinement:
   - add `instance?(x, <class>)` and `as(<class>, x)` semantic forms;
   - refine a matching local in the true branch of `if instance?(x, <class>)`;
   - lower through ordinary class checks, branches, traps, and runtime calls;
   - update dumps, fingerprints, tests, corpus, and progress notes.
3. Known callable objects:
   - add semantic value kinds/expressions for known direct functions, generic
     functions, classes, and accessors where practical;
   - add narrow `apply(f, arg...)` for statically known direct and generic
     function values;
   - lower known apply through existing direct/generic call paths;
   - update dumps, fingerprints, tests, corpus, and progress notes.

### Risks

- Generic contracts can look like a full type system. Keep them exact and
  local, with `<object>` as the only top boundary.
- Refinement needs to be visible in semantic dumps so it does not become hidden
  flow magic.
- Known callable values must not imply closures, captured environments, or
  late-bound invocation.
- `execution_tape` signatures remain static, so any runtime class-test/cast
  support must fit through existing host-call mechanisms.

## Add Typed Direct-Function Surface

### Goal

Add a narrow direct-function signature surface without changing generic or
method signatures. Function parameters may carry type annotations, function
returns may carry explicit result annotations, and direct calls are checked
against those signatures before lowering.

### Fence

The semantic layer owns direct-function signature meaning and call checking; the
AST owns narrow annotation syntax; lowering and tape emission consume resolved
value kinds and representation coercions only.

### Invariants

- Unannotated direct function parameters still default to `<integer>`.
- Unannotated direct function returns may remain inferred when available.
- Explicit parameter annotations are enforced at every direct call.
- Explicit return annotations are checked against the body result.
- Function signatures are collected before lowering function bodies.
- Class-object values stay distinct from runtime object instances.
- Generic declarations and method signatures do not gain new annotation syntax
  in this slice.

### Steps

1. Extend syntax and AST for direct-function parameter annotations and return
   annotations, including `class <name>` as the class-object type spelling.
2. Add semantic signature pre-pass data for parameter kinds and explicit return
   kinds.
3. Check direct-call arguments and annotated direct-function returns.
4. Teach lowering to coerce accepted top `<object>` parameters/returns through
   existing boxed-value representation.
5. Update dumps, fingerprints, corpus, and incremental tests for signature
   edits and signature-driven call invalidation.
6. Update progress notes and run fmt, clippy, workspace tests, docs, examples,
   `cairn_lab`, and `cairn_corpus`.

### Risks

- `<object>` accepts multiple concrete values semantically, so lowering must box
  direct-call arguments and returns at the call boundary.
- Explicit signatures collected before bodies must not erase useful inferred
  return kinds for earlier unannotated helper functions.
- Class-object type spelling should stay narrow so it does not imply
  metaclasses or reflective class protocols.

## Make Object Model More Dylan-Shaped

### Goal

Make two narrow object-model moves without implying a full Dylan object system:
slot reads become semantic accessor calls, and class names become semantic class
object values. Keep runtime lowering pragmatic: known accessors still become the
existing slot-read path, and known class objects still become class IDs.

### Non-Goals

- No metaclasses, user-defined classes of classes, or reflection.
- No setters, slot mutation, slot defaults, or full init protocol.
- No real inheritance semantics or parametric types.
- No first-class generic function values.
- No dynamic `make` for unknown class objects.
- No expanded effects/restarts or Cranelift.

### Fence

The semantic layer owns Dylan meaning. A slot access is a call through accessor
metadata, and a class name is a value. Lowering and runtime own representation:
known accessors become optimized slot reads, known class objects become class
IDs, and generic dispatch may keep using runtime class IDs internally.

### Steps

1. Add accessor metadata to slots with identity tied to the slot definition.
2. Rewrite semantic `c.radius` to accessor-call IR and add the same meaning for
   explicit `radius(c)` when it is unambiguous.
3. Lower accessor calls through the existing lowered `slot.read` instruction,
   update dumps, fingerprints, corpus, and incremental tests.
4. Make `<circle>` an AST and semantic class-object expression.
5. Change `make` to consume a known class-object expression and support simple
   local flow such as `let c = <circle>; make(c, radius: 10)`.
6. Add semantic `class-of(x)` returning a class-object value while lowering
   through existing class-id machinery.
7. Update progress notes and run fmt, clippy, workspace tests, docs, examples,
   `cairn_lab`, and `cairn_corpus`.

### Risks

- Accessor syntax must not shadow existing direct functions or generics.
- Class-object values should stay distinct from runtime instances even though
  known class objects lower to integer class IDs.
- The visible dumps need to show semantic meaning and representation lowering
  separately so this does not look more dynamic than it is.

## Make Slots Dylan-Shaped

Ticket: `cai-lnet`

### Goal

Make Cairn slot declarations follow the useful part of Dylan's slot shape:
untyped slots default to `<object>`, slots can declare required or renamed
construction keywords, and object construction reports focused diagnostics for
bad keyword initialization.

The intended surface shape is:

```text
define class <circle> (<object>)
  slot radius :: <integer>, required-init-keyword: radius:;
  slot cached :: <object>, init-keyword: cached-value:;
end;
```

The important semantic distinction is:

```text
<object>             => top Cairn value category
nominal <some-class> => runtime-managed object handle specialized to a class
```

### Non-Goals

- No default slot values, `init-value:`, or `init-function:`.
- No setters, `constant slot`, or setter naming.
- No `instance`, `class`, `each-subclass`, or `virtual` allocation variants.
- No inherited slot specifications.
- No parametric slot types.
- No full Dylan class initialization protocol.
- No sealed dispatch optimization or Cranelift.

### Fence

The compiler owns slot declaration metadata, top-value slot typing,
construction-keyword validation, semantic/lowered/tape artifact identity, and
dumps. The runtime owns carrying slot payload values through existing
host-call object operations. This slice does not own native aggregate layout,
boxed primitive identity, Dylan initialization methods, or optimization.

### Invariants

- Missing slot types resolve to the top `<object>` value category.
- `<object>` accepts integers, booleans, and nominal runtime object handles.
- Nominal class slot types still require a matching runtime object handle.
- A slot's construction keyword defaults to `slot-name:`.
- `init-keyword:` changes only the external construction keyword, not the slot
  read name.
- `required-init-keyword:` marks the slot as required and, when present,
  supplies the external construction keyword.
- In this slice, every slot still needs an initializer because defaults are not
  implemented; diagnostics should phrase required-keyword failures in keyword
  terms and ordinary missing initializers in slot terms.
- Duplicate, unknown, missing, and type-incompatible construction keywords
  produce diagnostics without panics.
- Lowering and tape emission continue to use ordinary calls and runtime host
  calls, not new generic or aggregate opcodes.

### Steps

1. Add parser and AST support for slot options:
   - `, init-keyword: keyword:`
   - `, required-init-keyword: keyword:`
2. Extend semantic slot metadata with declared type, effective value category,
   construction keyword, and requiredness.
3. Split value categories so top `<object>` is distinct from nominal class
   object handles.
4. Validate object construction by construction keyword, including duplicate
   keyword detection and focused missing/unknown/type diagnostics.
5. Update lowered IR and tape/runtime host-call signatures so top-value slots
   can store and return any first-slice Cairn value.
6. Update CST/AST/semantic/lowered/tape dumps, fingerprints, and artifact
   reporting to include declared type, keyword, requiredness, and runtime
   representation.
7. Add corpus fixtures for:
   - untyped `<object>` slots;
   - explicit primitive and nominal typed slots;
   - required init keywords;
   - renamed init keywords;
   - missing, unknown, duplicate, and type-incompatible construction keywords.
8. Add incremental tests proving slot metadata edits invalidate the affected
   class/layout/construction/lowered/tape artifacts without broad recompute.
9. Update progress notes with implemented semantics and deferred Dylan slot
   features, then run the full gates.

### Risks

- `execution_tape` host signatures are statically typed. Returning top
  `<object>` values requires a deliberate representation, likely a single
  runtime value handle for the first slice rather than pretending the value is a
  nominal object handle.
- Existing examples that relied on untyped integer slots should become explicit
  `:: <integer>` slots when their reads participate in arithmetic.
- The slot option syntax should stay narrow so it does not imply default values,
  setters, allocation variants, or inherited slot behavior.

## Build Richer Slot Type Model

Ticket: `cai-kf0d`

### Goal

Make class slot declarations carry a small explicit value kind and thread that
kind through object construction, slot reads, lowered IR, tape host signatures,
runtime object storage, dumps, corpus, and incremental reporting.

The intended surface shape is:

```text
define class <invalid-radius> (<object>)
  slot fallback :: <integer>;
  slot retry? :: <boolean>;
  slot cause :: <some-class>;
end;
```

This is a runtime-managed object model. It is not a general type system and it
does not add tape-native structs.

### Non-Goals

- No tape-native structs or aggregate opcodes.
- No sealed/value-class representation optimization.
- No inheritance-aware slot overriding.
- No parametric, union, intersection, nullable, or inferred types.
- No cross-function conditions, async, effect handlers, or Cranelift.

### Fence

The compiler owns slot declaration parsing, slot kind resolution, initializer
checking, and typed slot-read IR. The runtime owns storing and returning mixed
slot values for runtime-managed Cairn objects. Neither side owns native aggregate
layout in this slice.

### Invariants

- Untyped legacy `slot name;` declarations remained accepted in this slice.
  The later `cai-lnet` slot-initialization slice changed their default to
  `<object>`.
- Explicit primitive slot types are limited to `<integer>` and `<boolean>`.
- Explicit nominal slot types refer to already-declared or source-declared Cairn
  classes by name.
- `make(<class>, slot: value)` checks each initializer against the slot's
  declared value kind.
- Slot reads return the slot's declared value kind instead of a hard-coded
  integer.
- Tape emission continues to use host/runtime calls; only host signatures and
  runtime value storage become mixed-kind.

### Steps

1. Extend syntax and AST for optional `:: <type>` on slot declarations.
2. Add semantic slot kinds and resolve `<integer>`, `<boolean>`, and nominal
   object slot types while collecting classes.
3. Check object initializer kinds and make slot reads carry/read the declared
   kind.
4. Lower typed slot reads and update tape host signatures for per-class
   construction and per-slot read return types.
5. Store runtime object slots as `execution_tape::value::Value` so integers,
   booleans, and object handles round-trip through host calls.
6. Update AST/semantic/lowered/tape dumps, fingerprints, corpus fixtures, and
   incremental slot-type edit tests.
7. Update progress notes with the runtime-managed object ABI and deferred
   native-aggregate design, then run the full gates.

## Bind Restart Condition Objects

Ticket: `cai-240w`

### Goal

Let a lexical restart handler bind the signaled condition object and inspect its
ordinary nominal slots:

```text
handle
  signal(make(<invalid-radius>, fallback: 0))
with restart e :: <invalid-radius>
  e.fallback
end
```

This lifts the first restart slice's "no condition parameter" restriction
without adding a runtime handler stack or special execution_tape opcodes.

### Non-Goals

- No cross-call signaling.
- No dynamic handler lookup.
- No runtime condition stack.
- No multiple restarts or restart selection.
- No new condition declaration form.
- No new tape opcodes.

### Invariants

- The bound name is visible only inside the restart handler expression.
- The binding's semantic value category is the nominal condition object class.
- `signal` still evaluates the condition object once on the signal edge.
- Lowering moves that object into the handler-local register before jumping to
  the restart handler label.
- The existing unbound syntax remains accepted for simple handlers.

### Steps

1. Extend restart-clause syntax and AST with an optional `name ::` binding.
2. Bind the restart condition as a scoped semantic local for the handler body.
3. Lower the signal edge by moving the condition object into the handler-local
   register before the restart jump.
4. Update AST/semantic dumps, fingerprints, corpus fixtures, and incremental
   tests to show condition-slot access.
5. Record progress notes and run the full workspace gates.

## Build Recoverable Condition Restart Slice

Ticket: `cai-xvew`

### Goal

Add Cairn's first recoverable condition/restart vertical slice. Condition
objects remain ordinary nominal objects. `signal(condition)` inside a lexical
`handle ... with restart <condition-class> ... end` resumes with one handler
value, lowering to existing object construction, branches, jumps, and moves.

### Non-Goals

- No full effect typing.
- No stack inspection.
- No async.
- No dynamic extent edge cases.
- No multiple values.
- No optimizer work.
- No general cross-call unwinding.
- No runtime handler stack.

### Fence

This slice owns lexical condition signaling and one resumable restart value
inside a single function or method body. It explicitly does not own general
dynamic handler lookup, cross-call unwinding, typed effect inference, restart
selection, or runtime stack inspection.

### Syntax

The initial syntax was expression-oriented:

```text
handle BODY
with restart <condition-class>
  HANDLER
end
```

Signals use call-shaped syntax:

```text
signal(make(<condition-class>, fallback: 0))
```

### Plumbing

```text
ordinary class <invalid-radius>
  -> signal(make(<invalid-radius>, ...))
  -> handle body with restart <invalid-radius> handler end
  -> AST Signal / Handle / Restart
  -> semantic Signal / Handle / Restart + kind diagnostics
  -> lowered lexical restart frame:
       body normal path -> join
       signal path      -> restart handler -> join
  -> execution_tape ordinary object calls, branches, jumps, moves
```

### Invariants

- A `signal` must appear inside an active lexical `handle`.
- A `signal` condition must have a statically known nominal object class.
- The nearest active restart must specialize the signaled condition class.
- The handled body and restart handler must produce the same value category.
- The first slice did not bind the condition object. Ticket `cai-240w` adds the
  handler-local binding form.
- The core compiler crate remains `no_std + alloc`.

### Steps

1. Extend syntax for `handle`, `with`, and `restart` plus CST nodes for handle
   expressions and restart clauses.
2. Treat `signal(...)` as special call-shaped AST and add AST `Handle` and
   `Restart`.
3. Add semantic IR for `Signal`, `Handle`, and `Restart` with diagnostics for:
   - signal outside a handle;
   - signal condition that is not a known object;
   - signal class not handled by the active restart;
   - handled body / restart result kind mismatch.
4. Lower handles using an internal lexical restart frame in `BodyLowering`,
   without adding execution_tape opcodes or a runtime handler stack.
5. Update AST, semantic, lowered, and tape dumps/fingerprints.
6. Add corpus cases:
   - recovering from invalid radius;
   - checked arithmetic fallback;
   - diagnostics for uncaught signals and restart/body mismatches.
7. Add incremental tests proving restart handler edits recompute the edited
   method's lowered/tape artifacts while unrelated methods are reused.
8. Update progress notes and migration notes.
9. Run full gates:
   - `cargo fmt --all`;
   - `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
   - `cargo test --workspace --all-features`;
   - `cargo doc --workspace --all-features --no-deps`;
   - existing examples;
   - `cairn_lab` fixture tests;
   - `cairn_corpus`.

### Risks

- Lexical restart frames are intentionally smaller than real conditions. The
  docs must call out that cross-call signaling and handler stacks are deferred.
- `signal` returns the active restart's result category, so semantic lowering
  must lower restart handlers before the handled body.
- Nested handles should target the nearest matching active restart; corpus tests
  can stay small, but the lowering stack should be structured for nesting.

## Add Control Flow, Booleans, and Comparisons

Ticket: `cai-srg5`

### Goal

Add Cairn's first expression-level control-flow slice: boolean literals,
integer comparisons, and `if` expressions that flow through syntax, AST,
semantic IR, lowered IR, `execution_tape`, dumps, corpus fixtures, and
incremental cache reporting.

### Non-Goals

- No loops.
- No recursion-specific analysis.
- No short-circuit `and` / `or`.
- No pattern matching.
- No type inference beyond the current value categories.
- No parametric types.
- No effects or restarts.
- No sealed dispatch optimization.
- No Cranelift.

### Fence

This slice owns boolean/comparison/control-flow syntax and the direct lowering
needed to execute it. It explicitly does not own scheduler behavior, persistent
caches, richer type inference, multi-file module semantics, loop lowering, or
backend-specific control-flow opcodes.

### Plumbing

```text
source #t / #f / comparisons / if
  -> CST BoolExpr / BinaryExpr / IfExpr
  -> AST Bool / Compare / If
  -> semantic Bool value category + diagnostics
  -> lowered bool constants, comparisons, branches, moves, joins
  -> execution_tape ordinary instructions
  -> dumps, corpus, and incremental cache reports
```

### Invariants

- `if` conditions must have semantic kind `Bool`.
- Both `if` branches must produce the same semantic value category.
- Integer comparisons accept only `I64` operands and produce `Bool`.
- Arithmetic stays `I64 -> I64`; boolean support must not widen object slot
  storage or generic dispatch semantics.
- Lowered IR represents joins explicitly with labels, jumps, and register
  moves rather than a special high-level tape opcode.
- The compiler core remains `no_std + alloc`.

### Steps

1. Extend lexer/parser/CST for `#t`, `#f`, comparison tokens, and
   `if ... then ... else ... end`.
2. Extend AST lowering and dumps for bools, comparisons, and conditionals.
3. Extend semantic IR with `Bool`, comparison expressions, `if` expressions,
   and diagnostics for invalid conditions and mismatched branch kinds.
4. Extend lowered IR with bool constants, integer comparisons, moves, jumps,
   conditional branches, and explicit join values.
5. Emit the new lowered instructions into `execution_tape`.
6. Add corpus programs for passing control-flow examples and expected
   diagnostics.
7. Add incremental tests proving a small branch edit has focused lowered/tape
   cache effects.
8. Update progress notes with implemented semantics and deferred loop/recursion
   design.
9. Run full gates:
   - `cargo fmt --all`;
   - `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
   - `cargo test --workspace --all-features`;
   - `cargo doc --workspace --all-features --no-deps`;
   - existing examples;
   - `cairn_lab` fixture tests;
   - `cairn_corpus`.

### Risks

- Reusing `end` for both functions and `if` expressions can expose parser
  recovery bugs; tests should include nested conditionals.
- Direct-call return kinds are still simple and source-order based. This slice
  should document the limit rather than pretend full inference exists.
- Generic dispatch currently returns `I64`; conditionals may guard generic calls
  but should not expand generic return typing in this slice.

## Build Real In-Memory Incremental Compiler Engine

Ticket: `cai-e3zq`

### Goal

Turn the current one-shot session plus invalidation graph into a usable
in-memory incremental compiler engine. A caller should be able to keep named
source files in a session, edit them by stable `SourceFileId`, compile through
cached artifacts, and inspect which artifacts were reused, recomputed,
invalidated, and semantically affected.

### Non-Goals

- No persistent on-disk cache.
- No watch mode.
- No LSP.
- No parallel scheduler.
- No full multi-file module system.
- No new language semantics.
- No sealed dispatch optimization.
- No Cranelift.

### Fence

The incremental engine owns source-file edit state, artifact cache entries,
dependency-aware reuse/recompute reports, and per-artifact counters; it
explicitly does not own parser behavior, semantic rules, lowering rules,
runtime object storage, tape bytecode encoding, scheduling, or persistence.

The CLI tools own file IO, process exit behavior, and presentation; they
explicitly do not own cache dependency policy or artifact identity.

### Plumbing

```text
named source files
  -> IncrementalCompilerSession
  -> one-shot stage builders when a cache entry is missing/stale
  -> per-artifact fingerprints + dependency graph
  -> reuse / recompute / invalidated / affected reports
  -> cairn_lab edit output + cairn_corpus checks
```

### Invariants

- `SourceFileId` is stable for a logical file name or caller-provided id.
- `ArtifactKey` is the public identity for cache entries.
- A cache report must include every artifact known to the current query graph.
- Reuse is keyed by stable artifact fingerprints, not dense semantic indices.
- Dependency edges stay explicit and inspectable.
- The core compiler crate remains `no_std + alloc`.

### Metrics

Core cache reports use deterministic counters:

- `work_units`: estimated amount of artifact work performed for a recompute;
- `elapsed_ticks`: logical timing ticks for the artifact action.

This keeps the core no-std. A later scheduler or std wrapper can map the same
artifact reports to wall-clock timing without changing cache keys or dependency
semantics.

### Steps

1. Extend query artifact identity for diagnostics, lowered module, and runtime
   result artifacts.
2. Add an in-memory incremental session/cache API:
   - add named source files;
   - edit source by `SourceFileId`;
   - compile through cached artifact reports;
   - retain current `SessionArtifacts`.
3. Add artifact reports with:
   - action: reused or recomputed;
   - invalidated flag;
   - affected flag;
   - `work_units` and `elapsed_ticks`;
   - dependency edges.
4. Add tests for:
   - first compile populating cache;
   - repeat compile reusing all artifacts;
   - harmless trivia edits reusing most artifacts;
   - focused method body edits avoiding unrelated method artifacts;
   - definition-changing edits invalidating broad dependents.
5. Add `cairn_lab` output for edit reports.
6. Update `cairn_corpus` so corpus checks compile through the incremental
   engine.
7. Update progress notes and migration notes.
8. Run full gates:
   - `cargo fmt --all`;
   - `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
   - `cargo test --workspace --all-features`;
   - `cargo doc --workspace --all-features --no-deps`;
   - existing examples;
   - `cairn_lab` fixture commands;
   - `cairn_corpus` runner.

### Risks

- A whole-module compiler limits true partial artifact reuse. The first slice
  should make cache validity and reports explicit, while deferring a scheduler
  that can avoid all prerequisite recomputation.
- Dump-based or text-based fingerprints can be brittle. Prefer semantic
  fingerprints for method-level artifacts and normalize source-span-only text
  where possible.
- Public artifact enums will gain variants. Document this as a migration note
  in progress notes.

## Build Small-Program Corpus Runner

Ticket: `cai-5zew`

### Goal

Build a small, durable corpus of `.cairn` programs and a workspace runner that
validates each program through the session API. The corpus should make it easy
to add new semantic examples, expected diagnostics, and lightweight dump
stability checks without committing large snapshot files.

### Non-Goals

- No new language semantics.
- No new production dependencies.
- No persistent snapshot store.
- No watch mode or editor integration.
- No replacement for `tools/cairn_lab`; this runner is batch validation, not an
  interactive workbench.

### Fence

The `corpus/` tree owns source fixtures and declarative expectations; it
explicitly does not own compiler behavior or dump formatting.

The `tools/cairn_corpus` crate owns filesystem traversal, expectation parsing,
session invocation, and human-readable check output; it explicitly does not own
syntax, semantic, lowering, tape, or runtime rules.

### Plumbing

```text
corpus/**/*.cairn
  -> header expectations
  -> tools/cairn_corpus
  -> cairn_compiler::session
  -> runtime result / diagnostics / stage dumps
  -> pass/fail report
```

### Header Format

Each corpus file can declare expectations with leading comments:

```text
// expect: run 42
// expect: diagnostics 0
// expect: diagnostic contains unresolved local
// expect: dump lowered contains call
```

The runner treats these as the stable manifest for the file.

### Steps

1. Add `corpus/pass` and `corpus/fail` fixtures covering:
   - arithmetic;
   - locals;
   - direct calls;
   - classes, slots, and object construction;
   - generic dispatch;
   - expected diagnostics.
2. Add `tools/cairn_corpus` with:
   - recursive corpus discovery;
   - header expectation parsing;
   - session compilation;
   - runtime, diagnostic, and dump containment checks;
   - concise check and list commands.
3. Add tests for the runner and fixture set.
4. Update progress notes with corpus categories and how to add cases.
5. Run full gates:
   - `cargo fmt --all`;
   - `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
   - `cargo test --workspace --all-features`;
   - `cargo doc --workspace --all-features --no-deps`;
   - existing examples;
   - `cairn_lab` fixture commands;
   - `cairn_corpus` runner.
6. Close ticket and commit coherent changes.

### Risks

- Dump checks can become brittle. Use substring expectations that assert
  semantic landmarks rather than full dumps.
- Corpus expectations should not hide compiler regressions. Treat missing
  expectations as a runner failure.
- The core compiler remains `no_std + alloc`; keep all filesystem and process
  behavior in the tool crate.

## Build Compiler Session and Workbench CLI

Ticket: `cai-lk7f`

### Goal

Build a usable incremental compiler workbench around the existing Cairn
pipeline. The workbench should let a caller compile a named source file, inspect
every stage, list stable artifact keys, run `main`, and compare two file
versions for preserved/changed definitions and affected query artifacts.

### Non-Goals

- No watch mode.
- No LSP.
- No persistent on-disk cache.
- No full multi-file module system.
- No new language semantics.
- No sealed dispatch optimization.
- No parametric types.
- No effects/restarts.
- No Cranelift.

### Fence

The session layer owns source-file identity, per-stage artifact snapshots,
stable artifact-key reporting, dumps, and edit comparisons; it explicitly does
not own language semantics, lowering rules, runtime object storage, or
`execution_tape` bytecode internals.

The `tools/cairn_lab` crate owns file IO, CLI parsing, process exit behavior,
and presentation; it explicitly does not own compiler state or semantic
decisions.

### Plumbing

```text
.cairn file(s)
  -> cairn_compiler::session
  -> syntax / ast / sem / lower / tape / runtime
  -> dumps + artifact graph + edit diff
  -> tools/cairn_lab CLI
```

### Steps

1. Add `session` API to `cairn_compiler`:
   - named source files with `SourceFileId`;
   - per-stage artifacts;
   - diagnostics aggregation;
   - optional runtime result;
   - artifact-key listing and artifact graph dump;
   - stage dumps.
2. Add structural dumps for CST, AST, and semantic IR.
3. Add session diff:
   - preserved definitions;
   - changed definitions;
   - affected query artifacts for focused method body edits.
4. Add `tools/cairn_lab` CLI:
   - `run <file>`;
   - `dump <file> --stage syntax|ast|sem|lowered|tape|all`;
   - `artifacts <file>`;
   - `diff <before> <after>`.
5. Add fixture programs and CLI/session tests for:
   - harmless edit preserving IDs;
   - focused method edit affecting expected artifacts;
   - CLI commands producing useful output.
6. Update progress notes and close ticket.
7. Run full gates:
   - `cargo fmt --all`;
   - `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
   - `cargo test --workspace --all-features`;
   - `cargo doc --workspace --all-features --no-deps`;
   - existing examples;
   - CLI fixture commands.

### Risks

- Adding dumps can bloat the core API. Keep dump methods simple text views and
  avoid adding dependencies.
- `invalidation 0.2` uses dense internal keys. Preserve public `ArtifactKey`
  reporting while keeping dense tracker indexes internal.
- The compiler crate is `no_std + alloc`. Keep filesystem and CLI concerns in
  `tools/cairn_lab`.
