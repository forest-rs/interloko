# Cairn DESIGN.md

## Status

Cairn is a bounded language/compiler experiment, not a production language and not a Dylan compatibility project.

The purpose of Cairn is to provide a concrete, demanding frontend for `execution_tape`, while exploring a modern Dylan-shaped language design centered on generic functions, typed/restartable effects, refinement-friendly types, and incremental compilation.

The first milestone must stay small enough to implement and validate end-to-end.

## One-sentence summary

Cairn is a small Dylan-inspired incremental compiler/runtime experiment that lowers a rich semantic IR through a DFMC-like middle layer into `execution_tape`, with Cranelift left as a later backend.

## Goals

* Exercise `execution_tape` with a real frontend instead of designing the instruction set in isolation.
* Build a small, inspectable compiler pipeline: source → CST → AST → semantic IR → lowered IR → execution tape.
* Explore typed effects as a modernized, generalized condition/restart/control-flow system.
* Explore parametric types in a Dylan-like, generic-function-centered language without making parametric dispatch the first milestone.
* Provide a serious workload for incremental invalidation: syntax, bindings, semantic analysis, method lookup, lowering, and backend artifacts.
* Keep the runtime and compiler introspectable: spans, stable IDs, diagnostics, traces, and explicit dependencies.
* Preserve a path toward sealed/static optimization without losing Dylan-style interactive/open-world development.

## Non-goals

* Do not implement Dylan compatibility.
* Do not revive Dylan as a public language project.
* Do not implement macros in the first milestone.
* Do not implement a full module system in the first milestone.
* Do not implement Cranelift in the first milestone.
* Do not make `execution_tape` a high-level Dylan VM.
* Do not require the first version to have a complete type system.
* Do not implement generic dispatch over parameterized type arguments in the first milestone.
* Do not make this a replacement for Rust.

## Design posture

Cairn should be designed as a sequence of small vertical slices.

The project should prefer:

* clear IR boundaries;
* stable IDs;
* explicit dependencies;
* boring, verifiable lowering;
* simple runtime calls before special-purpose bytecode instructions;
* tests at every stage;
* inspectable dumps for every IR;
* small source examples that exercise real semantics.

The project should avoid:

* speculative instruction-set expansion;
* premature Cranelift integration;
* full language design before the first end-to-end execution works;
* clever type-system features without a lowering story;
* macro support before the syntax/binding/semantic pipeline is stable.

## Language identity

Cairn is Dylan-inspired, but not Dylan-compatible.

Its likely long-term identity is:

> an open, interactive, generic-function language with optional static precision, typed effects, restartable conditions, parametric types, refinements, and incremental compilation.

The language should support a progression from loose exploratory code to checked/sealed optimized code.

Example progression:

```dylan
// loose
define method draw(x, surface)
  ...
end;

// typed
define method draw(x :: <shape>, surface :: <surface>)
  ...
end;

// relationship-preserving
define method transform[T <: <shape>](x :: T, matrix :: <matrix>)
 => (result :: T)
  ...
end;

// effect-aware
define sealed method normalize[T](xs :: <non-empty-sequence>[T])
 => (ys :: <non-empty-sequence>[T])
 effects { allocate }
  ...
end;
```

## Compiler pipeline

The intended pipeline is:

```text
source text
  ↓
lossless CST
  ↓
AST / surface IR
  ↓
binding + definition IR
  ↓
typed/effectful semantic IR
  ↓
DFMC-like optimization IR
  ↓
lowered executable IR
  ↓
execution_tape backend
  ↓
runtime / interpreter
```

Possible later backend:

```text
DFMC-like optimization IR
  ↓
Cranelift backend
```

## Stage responsibilities

### Source text

The source language should initially be small and Dylan-like enough to exercise the pipeline.

The first milestone should include:

* function definitions;
* local bindings;
* numeric literals;
* arithmetic;
* function calls;
* return values;
* source spans;
* enough syntax to run one end-to-end example.

The first milestone should not include:

* macros;
* full modules;
* classes;
* generic functions;
* effects;
* refinements;
* Cranelift;
* sophisticated type inference.

Those belong in later milestones.

### Lossless CST

The CST should be resilient, lossless enough for tooling, and suitable for incremental reparsing.

It should preserve:

* source spans;
* comments/trivia when practical;
* malformed constructs via error nodes;
* enough structure for good diagnostics;
* stable node identity where possible.

Malformed source should not catastrophically stop the pipeline. Partial programs are normal in an editor/incremental compiler.

### AST / surface IR

The AST represents source-level constructs after basic parsing and simple desugaring.

It should not carry all inferred type/effect information directly. Prefer side tables or later semantic IR for analysis results.

Early AST concepts:

* function definition;
* parameter list;
* result annotation placeholder;
* let binding;
* block/body;
* literal;
* variable reference;
* call;
* arithmetic expression.

Later AST concepts:

* class definition;
* generic definition;
* method definition;
* type expression;
* effect expression;
* refinement expression;
* block/return;
* handle/restart forms;
* select/typecase forms.

### Binding + definition IR

This stage assigns durable identities to definitions and bindings.

Important IDs include:

```text
SourceFileId
SyntaxNodeId
AstNodeId
DefinitionId
ModuleId
BindingId
FunctionId
GenericFunctionId
MethodId
ClassId
TypeId
EffectId
RestartId
SpanId
```

The first milestone may only need a subset:

```text
SourceFileId
SyntaxNodeId
AstNodeId
FunctionId
LocalId
SpanId
```

Binding should produce diagnostics, not panics, when names are unresolved.

### Semantic IR

Semantic IR is the first representation where Cairn-specific semantics become explicit.

Long-term semantic IR should represent:

* lexical variables;
* direct calls;
* generic calls;
* object construction;
* slot access;
* type expressions;
* effect expressions;
* refinement checks;
* condition signaling;
* handlers;
* restarts;
* escape continuations;
* dynamic context/capabilities.

For milestone 0, semantic IR can be much smaller:

* constants;
* locals;
* let bindings;
* arithmetic;
* direct calls;
* function bodies.

Semantic IR should preserve spans and stable IDs for diagnostics and tracing.

### DFMC-like optimization IR

This layer is inspired by Dylan compiler architecture, especially the need for a representation that understands generic functions, method lookup, type propagation, and optimization assumptions.

Long-term responsibilities:

* direct-call resolution;
* generic dispatch analysis;
* sealed-domain optimization;
* type propagation;
* effect propagation;
* refinement proof/check placement;
* closure conversion;
* handler/restart lowering decisions;
* representation decisions;
* dependency tracking for invalidation.

Milestone 0 may implement this as a minimal pass-through lowering layer. That is acceptable. The boundary is still worth preserving.

### Lowered executable IR

Lowered IR is backend-neutral and close to executable form.

It should avoid high-level language semantics where possible.

Examples:

* constants;
* registers/temporaries;
* direct calls;
* branches;
* primitive arithmetic;
* runtime calls;
* host calls;
* explicit checks/traps where needed.

### Execution tape backend

The execution tape backend emits `execution_tape` instructions from lowered IR.

The tape should remain boring and verifiable.

Prefer runtime calls before adding high-level instructions for language semantics.

For example, initially lower a generic call as:

```text
source:   area(c)
semantic: GenericCall area(c)
lowered:  runtime_dispatch(area_id, args)
tape:     HOST_CALL runtime_dispatch
```

Only later, after sealed-domain analysis:

```text
source:   area(c)
semantic: GenericCall area(c)
dfmc:     DirectCall area_circle(c)
tape:     CALL area_circle
```

## Execution tape design pressure

Cairn should help answer what `execution_tape` needs.

Likely useful tape capabilities:

* typed constants;
* register values;
* direct calls;
* indirect calls or runtime dispatch calls;
* host calls;
* branches;
* traps/checks;
* spans;
* trace hooks;
* effect/capability token flow;
* function/type/constant pools;
* deterministic execution;
* inspectable dumps;
* verification.

Avoid adding high-level operations unless demanded by repeated lowering pressure.

Avoid early instructions like:

```text
INVOKE_GENERIC
SIGNAL_CONDITION
INVOKE_RESTART
CHECK_REFINEMENT
LOOKUP_MODULE_BINDING
```

Prefer runtime calls or lowered primitives first.

## Runtime model sketch

Milestone 0 runtime can be very small:

* function table;
* constants;
* numeric values;
* call support;
* primitive arithmetic;
* simple diagnostics/tracing.

Later runtime responsibilities:

* object model;
* class metadata;
* generic function dispatch;
* method tables;
* type descriptors;
* condition/effect dispatch;
* restart frames;
* handler stack;
* debugger hooks;
* refinement failure policy;
* runtime type checks;
* optional reified parametric type descriptors.

## Parametric types

Cairn should support parametric types, but they should not become the primary dispatch mechanism at first.

Core principle:

> Parametric types provide gradual semantic precision for an open generic-function language.

They enrich value descriptions, storage layout decisions, API contracts, diagnostics, refinements, and optimization. Generic functions and methods remain central.

### Initial parametric type goals

Support examples like:

```dylan
define class <box>[T] (<object>)
  slot value :: T;
end;

define function first[T](xs :: <sequence>[T])
 => (value :: T)
  ...
end;
```

Initial support should include:

* nominal parametric classes/types;
* function/method type parameters;
* type expressions with parameters;
* substitution in signatures;
* basic semantic checking;
* diagnostics that preserve source names.

### Initial parametric type non-goals

Do not initially support:

* higher-kinded types;
* full dependent types;
* value parameters such as `<vector>[T, N]`;
* Rust-like trait solving;
* dispatch over parameterized type arguments;
* universal reification of all type parameters;
* aggressive monomorphization.

### Dispatch rule

Initial rule:

> Method dispatch uses the runtime class/domain specializer. Parametric arguments constrain applicability only when statically known or explicitly checked; they do not create independent runtime dispatch domains by default.

This means:

```dylan
define method sum(xs :: <vector>[<integer>])
 => (n :: <integer>)
  ...
end;
```

is semantically a method specialized on `<vector>` with an additional type constraint/check/proof involving `<integer>`.

It is not initially a separate runtime dispatch domain from:

```dylan
define method sum(xs :: <vector>[<float>])
 => (n :: <float>)
  ...
end;
```

The compiler should reject or clearly diagnose ambiguous/unsupported parametric dispatch until such dispatch is deliberately designed.

### Variance

Default to invariant type parameters.

Do not assume:

```text
<vector>[<integer>] <: <vector>[<number>]
```

Later, read-only abstractions may support covariance and sink-like abstractions may support contravariance, but that is not an early milestone.

### Reification

Cairn should eventually support runtime type descriptors for some parameterized types.

However, do not require all type parameters to be universally reified in the first implementation.

Possible later forms:

```dylan
define reified class <typed-vector>[T] (<object>)
  ...
end;
```

or:

```dylan
define class <vector>[T] (<object>)
  reified parameter T;
end;
```

Early implementation may treat many parametric types as semantic/checking constructs that erase during lowering unless an explicit runtime check is required.

### Type objects

Cairn should preserve a Dylan-like notion of type/class objects.

Eventually, this should work:

```dylan
make(<vector>[<string>])
parse-as(text, <integer>)
ensure-type(value, <sequence>[<integer>])
```

This requires type expressions that can become runtime type descriptors when needed.

### Refinements and parametric types

Refinements should compose with parametric types.

Prefer early forms like:

```dylan
define type <non-empty-sequence>[T]
  = <sequence>[T] where size(value) > 0;
end;
```

Instead of starting with value parameters like:

```dylan
<vector>[T, 3]
```

Initial rule:

> Cairn v0/v1 supports type parameters. Value-level constraints are represented as refinements, not generic parameters.

### Parametric effects

Effects should eventually be parametric too.

Example:

```dylan
define effect <choose>[T] (options :: <sequence>[T])
 => (choice :: T);
```

Example condition/restart shape:

```dylan
define effect <invalid-value>[T] (value :: <object>, expected :: <type>[T])
  restart use-value(value :: T) => T;
  restart abort() => <never>;
end;
```

This is not milestone 0, but the semantic IR should not make it impossible.

### Semantic type representation

Long-term semantic types may look like:

```text
Type =
  Any
  Never
  Nominal(ClassId)
  Apply(TypeConstructorId, [Type])
  Param(TypeParamId)
  Union([Type])
  Singleton(ConstId)
  Refinement(base: Type, predicate: PredicateId)
  Function(args: [Type], results: [Type], effects: EffectSet)
```

## Effects and conditions

Cairn’s effect system should be understood as a modernized, generalized condition/restart/control-flow system.

Core idea:

> An effect is a named, typed control opportunity raised by code and handled by dynamically enclosing code.

Effects are not just exceptions and not just IO annotations.

They should eventually cover:

* conditions;
* resumable recovery;
* restarts;
* non-local exits;
* parser/compiler recovery;
* debugger intervention;
* policy injection;
* query-like requests to dynamic context;
* dependency-producing compiler operations.

### Conditions and restarts

A condition/effect can expose possible restarts:

```dylan
define effect <missing-file> (path :: <pathname>)
  restart use-value(value :: <object>) => <object>;
  restart create-file() => <stream>;
  restart abort() => <never>;
end;
```

Signaling code offers restart opportunities. Handlers select policy.

The default interactive debugger can be treated as a handler for unhandled effects.

### Effects and invalidation

Compiler effects can also be dependency-producing operations.

Example conceptual operation:

```text
perform resolve-name(scope, "draw")
```

The handler can return both:

```text
binding = BindingId(...)
dependency = NameResolutionDependency(scope, "draw")
```

This connects the effect system to incremental compilation.

Potential compiler effects:

```text
read-source
resolve-name
lookup-type
lookup-methods
report-diagnostic
assume-refinement
request-macro-expansion
```

Each can be both a control operation and a dependency edge.

### Static effect declarations

Effect declarations should be optional at first, then useful for checked/sealed code.

Example eventual syntax:

```dylan
define function parse(input :: <string>)
 => (ast :: <ast>)
 effects { <parse-error>, allocate }
  ...
end;
```

A sealed/static context can reject undeclared effects.

An exploratory context can infer and report them.

## Incremental compilation and invalidation

Cairn should be built around queryable compiler stages.

Possible queries:

```text
parse(file) -> CST
lower_ast(definition) -> AST
resolve_name(scope, name) -> Binding
infer_type(expr) -> Type
infer_effects(function) -> EffectSet
applicable_methods(generic, arg_types) -> MethodSet
lower_function(function) -> LoweredFunction
emit_tape(function) -> TapeFunction
```

Each query should record dependencies.

A change should invalidate the smallest reasonable set of downstream artifacts.

This makes Cairn a useful workload for the invalidation crate.

## Milestones

### Milestone 0: direct functions to tape

Goal:

> Compile and run a tiny program with direct functions, local bindings, arithmetic, calls, spans, and tests.

Required source subset:

* function definitions;
* parameters;
* numeric literals;
* local `let` bindings;
* arithmetic;
* direct function calls;
* return value by final expression or explicit minimal convention.

Required compiler pieces:

* lexer/parser;
* lossless-enough CST;
* AST lowering;
* binding for functions and locals;
* minimal semantic IR;
* lowered IR;
* execution tape emission;
* tiny runtime/interpreter;
* diagnostics with spans;
* tests for each stage;
* one end-to-end example.

Example:

```dylan
define function forty-two()
  40 + 2
end;

define function main()
  forty-two()
end;
```

Validation:

```text
cargo fmt
cargo clippy --workspace --all-targets
cargo test --workspace
```

### Milestone 1: classes and simple object model

Add:

* class definitions;
* slots;
* object construction;
* slot reads;
* nominal type annotations;
* runtime class descriptors.

Example:

```dylan
define class <circle> (<object>)
  slot radius :: <float>;
end;

define function make-circle-area()
  let c = make(<circle>, radius: 10.0);
  3.14159 * c.radius * c.radius
end;
```

### Milestone 2: generic functions and runtime dispatch

Add:

* generic function declarations;
* method definitions;
* method table;
* runtime dispatch stub;
* diagnostics for missing/ambiguous methods.

Example:

```dylan
define generic area(x) => (result :: <float>);

define method area(c :: <circle>) => (result :: <float>)
  3.14159 * c.radius * c.radius
end;
```

### Milestone 3: sealed generic direct dispatch

Add:

* sealed generic or sealed method marker;
* simple method lookup analysis;
* direct-call lowering when method target is known;
* dependency on method table revision/sealed domain.

### Milestone 4: parametric type syntax and semantic checking

Add:

* parametric type expressions;
* type parameters on classes/functions/methods;
* semantic representation of `Apply` and `Param` types;
* invariant parameter checking;
* no parametric dispatch yet.

Example:

```dylan
define class <box>[T] (<object>)
  slot value :: T;
end;

define function unbox[T](box :: <box>[T]) => (value :: T)
  box.value
end;
```

### Milestone 5: refinement checks

Add:

* simple refinement type aliases;
* runtime checks where unproven;
* refinement failure diagnostic or runtime condition placeholder.

Example:

```dylan
define type <non-negative-float>
  = <float> where value >= 0.0;
end;
```

### Milestone 6: effects as semantic IR

Add:

* effect declarations;
* function effect annotations;
* semantic IR nodes for `perform`/`signal`;
* lowering to runtime calls;
* no full restart protocol yet.

### Milestone 7: restartable conditions

Add:

* handler frames;
* restart declarations;
* runtime restart invocation;
* debugger/policy hook placeholder.

### Milestone 8: Cranelift backend exploration

Only after tape execution and semantic lowering are stable.

Add:

* Cranelift lowering for direct functions;
* no generic dispatch at first unless already lowered to direct calls/runtime calls;
* compare behavior with tape backend.

## Suggested workspace layout

Initial simple layout:

```text
cairn/
  crates/
    cairn_syntax/
    cairn_ast/
    cairn_sem/
    cairn_lower/
    cairn_tape/
    cairn_runtime/
    cairn_driver/
  examples/
  tests/
  DESIGN.md
```

Possible later layout:

```text
cairn_syntax       // lexer, CST, syntax nodes
cairn_parse        // parser, error recovery
cairn_ast          // surface AST
cairn_bind         // scopes, definitions, binding IDs
cairn_sem          // typed/effectful semantic IR
cairn_dfmc         // analysis and optimization layer
cairn_lower        // backend-neutral lowering
cairn_tape         // execution_tape backend
cairn_clif         // Cranelift backend
cairn_runtime      // object model, dispatch, conditions
cairn_driver       // incremental query driver
cairn_diagnostics  // diagnostic types/rendering if needed
```

Follow repository conventions:

* prefer `#![no_std] + alloc` for core/kernel crates where reasonable;
* keep examples and benchmarks outside core crates;
* document public APIs;
* maintain `cargo fmt` and `cargo clippy` cleanliness;
* keep implementation small and staged.

## Diagnostics

Diagnostics should be structured from the beginning.

A diagnostic should carry:

* severity;
* primary span;
* optional secondary spans;
* stable diagnostic code if useful;
* message;
* optional notes;
* optional suggested fixes later.

The compiler should accumulate diagnostics where possible instead of panicking.

## Dumps and inspection

Every stage should have a debug dump format:

* CST dump;
* AST dump;
* binding table dump;
* semantic IR dump;
* lowered IR dump;
* tape dump;
* runtime trace where useful.

These dumps are essential for Codex work, debugging, and regression tests.

## Testing strategy

Use snapshot-style tests where helpful, but keep them readable and stable.

Test categories:

* lexer tests;
* parser/CST tests;
* AST lowering tests;
* binding tests;
* semantic IR tests;
* lowering tests;
* tape emission tests;
* runtime execution tests;
* diagnostics tests.

Milestone 0 must include at least one end-to-end test that compiles and runs a `main` function.

## Codex goal seed

A good first Codex goal should be narrow and verifiable:

```text
/goal Build the initial Cairn compiler prototype as a bounded execution_tape frontend experiment.

Read DESIGN.md first. Implement only Milestone 0:
- parse a tiny Dylan-like syntax for function definitions, let bindings, numeric literals, arithmetic, and calls;
- produce a lossless-enough CST with spans;
- lower to a simple AST;
- bind functions and locals;
- lower AST to a minimal semantic IR;
- emit execution_tape for direct function calls and arithmetic;
- add tests for parser, AST lowering, semantic lowering, tape emission, and execution;
- add one end-to-end example that compiles and runs `main()` returning 42.

Do not implement generic functions, classes, effects, refinements, macros, Cranelift, or a full module system yet.

Stop only when `cargo fmt`, `cargo clippy --workspace --all-targets`, and `cargo test --workspace` pass, and the progress log explains what is implemented and what is intentionally deferred.
```

## Kill criteria

Pause or narrow the project if:

* the first milestone starts requiring full language design before tape execution works;
* `execution_tape` changes become speculative rather than demanded by concrete lowering;
* Cranelift work starts before the semantic/tape path is validated;
* macros become necessary before the parser/binder/semantic stages are stable;
* parametric types force dispatch semantics before simple generic dispatch exists;
* the project stops producing small end-to-end examples;
* tests and dumps lag behind implementation.

## Open questions

* How close should the syntax remain to Dylan?
* Should `block`/`return` be included before conditions/effects?
* Should type objects be available before parametric types?
* How much of `execution_tape` should be aware of effect tokens in milestone 0?
* Should the initial runtime be an interpreter over tape, or should tape execution already exist elsewhere?
* What exact stable ID strategy should be used for CST/AST/semantic nodes?
* Should the first implementation use a rowan-like green/red tree, or a simpler arena CST?
* How soon should invalidation/query infrastructure be introduced?
* Should this live inside an existing repo, or begin as a separate experimental repo?

## Working principle

Keep the tape boring. Let the semantic layers be interesting.

Cairn succeeds if it teaches us what the boring substrate must support while giving us a concrete playground for generic functions, typed effects, restartable conditions, parametric types, refinements, and incremental compilation.
