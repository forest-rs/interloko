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
- immutable top-level constants named in the Dylan style, such as
  `define constant $answer :: <integer> = 42;`;
- nominal classes with typed slots, init keywords, inherited slots, and slot
  assignment through setter generics;
- generic functions and multi-argument method dispatch over nominal classes;
- simple strings, symbols, keywords, and immutable vectors;
- recoverable condition/restart forms;
- read-only MOP inspection for classes, slots, generics, methods, and dispatch;
- embedding-declared scalar host functions with typed integer, boolean, and
  string arguments/results, EOP discovery, and a handler-aware call surface;
- read-only grammar MOP inspection plus trusted staged grammar adapters;
- structured EOP records for environment/workbench objects and affordances;
- explicit host-backed class/slot metadata for Rust interop experiments;
- compiler sessions, incremental artifacts, cache reports, and stage dumps;
- structured diagnostics rendered through MessageFormat 2 catalogs.

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

`interloko_lab` is the quickest way to see what is going on:

```sh
cargo run -p interloko_lab -- run <file.loko>
cargo run -p interloko_lab -- dump <file.loko> --stage syntax|ast|sem|mop|eop|lowered|tape|all
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

The compiler uses published `execution_tape` 0.1.0 from crates.io. Lowering records
each register's storage kind when assigning its identity; the backend seeds an
assembler from the declared function ABI and appends backend temporaries.
It does not recover declarations by scanning emitted instructions. Closure
environments and indirect signatures use the VM's compositional contracts.

Compiled tapes are regeneratable artifacts. The current wire version is 0.5;
recompile older tapes and regenerate any source sidecars with them. Runtime
results use an explicit owned output vector and remain readable after their
execution context is dropped.

Compiler error and trace wrappers containing `TrapInfo` implement `Clone` and
`Debug`, and no longer derive `Eq`/`PartialEq`: traps may carry an owned typed
host error. Match the error variant or inspect its cause instead of comparing
whole errors. `FunctionCallError::Trap` owns its trap in a box to keep normal
error results compact. `lower::Function::register_kinds` contains declarations
for registers r1 onward; r0 remains the effect token.

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
