# Interloko

Interloko is an experimental compiler/runtime substrate for a small
Dylan-shaped source language called Loko.

The short version:

- **Interloko** is the compiler, runtime, object model, and inspection
  substrate.
- **Loko** is the source notation, stored in `.loko` files.
- **Vivarium** is the possible future collaborative environment built on top of
  this substrate.

This repository is not trying to be a production language yet. It is a working
place to make object-system semantics, generic dispatch, host interop,
incremental compilation, and inspectable execution concrete.

## Try It

Run one corpus program:

```sh
cargo run -p interloko_lab -- run corpus/pass/generics/two_class_dispatch.loko
```

Generate the staged-grammar report demo:

```sh
cargo run -p interloko_lab -- report tools/interloko_lab/fixtures/staged_records.loko --output /tmp/staged-records.html
```

Open `/tmp/staged-records.html` directly in a browser. That report is the
current best demo of the grammar/frontend work: a trusted `records` region
emits structural class forms, the AST consumes those forms directly, and the
report links source spans to generated classes, slots, accessors, artifacts,
lowered IR, tape, and runtime trace output.

Inspect a compiler stage:

```sh
cargo run -p interloko_lab -- dump corpus/pass/generics/two_class_dispatch.loko --stage sem
cargo run -p interloko_lab -- dump corpus/pass/generics/two_class_dispatch.loko --stage tape
```

Run the small-program corpus:

```sh
cargo run -p interloko_corpus -- check corpus
```

Start the REPL:

```sh
cargo run -p interloko_repl
```

Run the float calculator example, either over its built-in formulas or on an
expression in the staged calculator grammar:

```sh
cargo run -p interloko_float_calculator_example
cargo run -p interloko_float_calculator_example -- "(1.0 + sqrt(5.0)) / 2.0"
```

## A Small Loko Program

```loko
define class <circle> (<object>)
  slot radius :: <integer>;
end;

define class <square> (<object>)
  slot side :: <integer>;
end;

define generic area(x);

define method area(c :: <circle>)
  c.radius * c.radius
end;

define method area(s :: <square>)
  s.side * s.side
end;

define function main()
  area(make(<square>, side: 9))
end;
```

That program parses through the normal pipeline:

```text
source -> syntax -> AST -> semantic IR -> lowered IR -> execution_tape
```

Generic dispatch is represented in Interloko's semantic/lowered IR, then
emitted to `execution_tape` through ordinary calls and runtime host calls rather
than special generic-dispatch bytecode.

## What Works Today

The current slices include:

- function definitions, locals, arithmetic, booleans, conditionals, and calls;
- `<integer>` arithmetic on 64-bit fixnums that traps with
  `runtime.integer-overflow` instead of wrapping when `+`, `-`, `*`, or
  negation leaves the 64-bit range (bignum promotion is planned);
- immutable top-level constants named in the Dylan style, such as
  `define constant $answer :: <integer> = 42;`;
- nominal classes with typed slots, init keywords, inherited slots, and slot
  assignment through setter generics;
- generic functions and multi-argument method dispatch over nominal classes;
- a numeric class tower: `<number>` > `<real>`, then `<rational>` >
  `<integer>` and `<float>` > `<double-float>`, where only `<integer>` and
  `<double-float>` are concrete; methods may specialize on any of them, and
  an abstract specializer such as `<real>` or `<float>` receives the boxed
  value unless the generic fixes a scalar parameter type, while annotations
  name a concrete class;
- floats: literals such as `1.5`/`2e10` are `<double-float>` (IEEE 754
  binary64) instances under the abstract `<number>` > `<real>` > `<float>`
  classes, with slots, boxing to `<object>`, `<double-float>`
  annotations and method dispatch, and IEEE `+ - * /`,
  negation, and comparisons; an integer meeting a float is converted to a
  float for arithmetic and compared exactly (`2^53 + 1 > 2.0^53`), by the
  rules in `promotion.rs`, and `/` on two integers is rejected. When an
  operand is `<object>`, abstract numeric or a user class, the operator
  dispatches through a built-in generic (`+`, `<`, `negative`, ...) whose
  methods come from the same rules; these generics show up in EOP and soft
  typing like any other and are sealed over `<real>`; explicit
  `as(<double-float>, n)` and exact
  `as(<integer>, x)` conversions, and a float library (`abs`, `min`, `max`,
  `remainder`, `nan?`, `finite?`, `floor`, `ceiling`, `round`, `truncate`,
  `sqrt`, `cbrt`, `exp`, `log`, `log2`, `log10`, trigonometric and
  hyperbolic functions, `atan2`, `pow`, `hypot`) whose names user
  definitions may shadow; `float-to-string`, `integer-to-string`, and
  `concatenate` build result text;
- simple strings, symbols, keywords, and immutable vectors;
- recoverable condition/restart forms;
- read-only MOP inspection for classes, slots, generics, methods, and dispatch;
- embedding-declared scalar host functions with typed integer, float,
  boolean, and string arguments/results, EOP discovery, and a handler-aware call surface;
- read-only grammar MOP inspection plus trusted staged grammar adapters;
- structured EOP records for environment/workbench objects and affordances;
- explicit host-backed class/slot metadata for Rust interop experiments;
- compiler sessions, incremental artifacts, cache reports, and stage dumps;
- structured diagnostics rendered through MessageFormat 2 catalogs.

## Retained Sessions

For embedding in a retained tool, [`interloko_session`](crates/interloko_session/README.md)
provides accepted compiler sessions with revision-checked source replacement,
object inspection, typed scalar calls, explicit host handlers, mutation and
rewind. Rejected edits preserve accepted code and history. The wrapper is
`no_std + alloc`; callers own transport, persistence and scheduling.

## What This Is Not Yet

Interloko is intentionally not:

- a stable public language or Dylan compatibility project;
- a persistent image or database;
- the Vivarium product/environment layer;
- an LSP, watch mode, or IDE;
- a macro system;
- a Cranelift backend;
- a full module system;
- a sealed-dispatch optimizer.

The project is useful because the compiler and runtime already expose enough
structure to inspect what the object model means and how it lowers, without
pretending the larger environment exists yet.

## Repository Layout

```text
crates/interloko_model       shared ids and small substrate facts
crates/interloko_runtime     runtime layout, host calls, object storage
crates/interloko_compiler    Loko parser, AST, semantic IR, lowering, sessions
crates/interloko_session     retained, revision-checked embedding sessions

tools/interloko_lab          command-line workbench for dumps and inspection
tools/interloko_corpus       corpus runner for small .loko programs
tools/interloko_diagnostics  diagnostic catalog loading/rendering
tools/interloko_repl         small interactive REPL over compiler sessions

corpus/                      pass/fail .loko fixture programs
examples/                    Rust embedding examples
docs/                        focused design notes
```

Useful docs:

- [`DESIGN.md`](DESIGN.md) explains the broader compiler/runtime direction.
- [`docs/interloko_boundary.md`](docs/interloko_boundary.md) explains the
  Interloko/Loko/Vivarium naming split.
- [`docs/mop_protocol.md`](docs/mop_protocol.md) describes read-only MOP
  protocols.
- [`docs/grammar_mop.md`](docs/grammar_mop.md) describes the read-only grammar
  projection, grammar registry, and staged adapter boundary.
- [`docs/eop_workbench.md`](docs/eop_workbench.md) describes environment object
  records and workbench affordances.
- [`docs/eop_mutation.md`](docs/eop_mutation.md) describes the in-memory
  live-mutation contract for the executable `add-method` EOP affordance.
- [`docs/interloko_report.md`](docs/interloko_report.md) explains the static
  HTML report command and its staged-grammar demo path.
- [`corpus/README.md`](corpus/README.md) explains corpus expectations.

## Workbench Commands

`TranscriptLoadError::Manifest` owns its TOML deserializer cause in a `Box` so
transcript-loading errors stay compact. Code constructing that variant should
use `Box::new(error)`; matching and inspecting the cause through a reference
continues to work through dereferencing.

`interloko_lab` is the quickest way to see what is going on:

```sh
cargo run -p interloko_lab -- run <file.loko>
cargo run -p interloko_lab -- dump <file.loko> --stage syntax|ast|sem|mop|eop|lowered|tape|typing|all
cargo run -p interloko_lab -- artifacts <file.loko>
cargo run -p interloko_lab -- report <file.loko> --output <report.html>
cargo run -p interloko_lab -- dispatch <file.loko> [dispatch-site-id]
cargo run -p interloko_lab -- inspect <file.loko> <object-id>
cargo run -p interloko_lab -- affordances <file.loko> <object-id>
cargo run -p interloko_lab -- diff <before.loko> <after.loko>
```

Diagnostics can be rendered with the bundled catalogs or a custom catalog:

```sh
INTERLOKO_DIAGNOSTIC_LOCALE=pseudo cargo run -p interloko_lab -- run corpus/fail/diagnostics/unresolved_local.loko
INTERLOKO_DIAGNOSTIC_CATALOG=path/to/catalog.mf2.toml cargo run -p interloko_lab -- run some-file.loko
```

## Development

The normal validation set is:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo doc --workspace --all-features --no-deps
cargo run -p interloko_corpus -- check corpus
```

Core crates should stay small and boring. New examples, experiments, and
benchmarks should live in top-level workspace crates rather than adding
development dependencies to the core crates.

## Execution Tape Contract

The compiler pins `execution_tape` to a git revision of `forest-rs/execution`
`main` (see the root `Cargo.toml`) until the next crates.io release. Lowering records
each register's storage kind when assigning its identity; the backend seeds an
assembler from the declared function ABI and appends backend temporaries.
It does not recover declarations by scanning emitted instructions. Closure
environments and indirect signatures use the VM's compositional contracts.

Compiled tapes are regeneratable artifacts. The current wire version is 0.6;
recompile older tapes and regenerate any source sidecars with them. Runtime
results use an explicit owned output vector and remain readable after their
execution context is dropped.

Boxed `<object>` values are VM objects of one host type. Floats, booleans,
integers within 47 bits and references to runtime objects travel inside the
object handle and never allocate a box; wider integers live in the runtime
host's heap.

The runtime heap has two regions. Each call is one *turn*: everything it
allocates (instances, strings, vectors, metaobjects, heap boxes) goes to a
nursery that is emptied in one step when the call returns or traps. The
long-lived image holds interned symbols and objects an embedding creates
outside a turn. `SessionArtifacts::runtime` returns a `SessionRuntime`
whose image persists across calls (the REPL uses one; `:heap` shows it),
while `SessionArtifacts::call_function` uses a fresh one per call.
Turns are transactional: a turn's stores into image objects are recorded
in a write log, and a trapped turn replays it backwards, so the image is
left as the turn found it. When a turn returns, the nursery entries
reachable from its roots (values the caller keeps, image roots registered with
`InterlokoHost::register_root`, and the image slots its write log names)
are evacuated into the image in increasing nursery order, charged in work
units against an evacuation budget; everything else is dropped.
`FunctionCallOutcome::turn` reports what a call allocated (the nursery
high-water mark), what was reachable at the end, what was promoted, the
evacuation work and the number of logged stores; `interloko_corpus heap
[root...]` prints survival for every program with a `main`.

Compiler error and trace wrappers containing `TrapInfo` implement `Clone` and
`Debug`, and no longer derive `Eq`/`PartialEq`: traps may carry an owned typed
host error. Match the error variant or inspect its cause instead of comparing
whole errors. `FunctionCallError::Trap` owns its trap in a box to keep normal
error results compact. `lower::Function::register_kinds` contains declarations
for registers r1 onward; r0 remains the effect token.

Traced runs record with `execution_tape::recording::Recorder`; cairn has no
trace sink of its own. `ExecutionTrace` shares the owned upstream `Recording`
(invocation values, host calls with guest call stacks, the final return or
trap with its stack pinned before unwinding, optional instruction history,
capture completeness, and checked replay). `ExecutionTraceDetail::Boundaries`
counts every entered activation but lists only those visible at a host call
or the outcome; `ExecutionTraceDetail::Instructions` adds up to
`TRACE_INSTRUCTION_LIMIT` attempted instructions with their stacks.
`SessionArtifacts::execution_trace_view` adds Interloko meaning: tape function
ids resolve to lowered functions, their roles and environment object ids, and
instruction spans resolve to source line and column. Text dumps and HTML
reports render that view.

## Glossary

- **Loko:** the current Dylan-shaped source notation and `.loko` file format.
- **Interloko:** the compiler/runtime substrate that parses, analyzes, lowers,
  executes, and inspects Loko programs.
- **MOP:** Metaobject Protocol; structured facts about classes, slots, generic
  functions, methods, and dispatch.
- **EOP:** Environment Object Protocol; structured facts that make compiler,
  runtime, host, and future workspace objects inspectable.
- **Host-backed object:** a Rust-owned value exposed to Interloko through
  explicit metadata and runtime adapters.
- **Vivarium:** the future collaborative environment layer, not implemented in
  this repository yet.
