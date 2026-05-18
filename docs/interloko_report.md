# Interloko HTML Reports

`interloko_lab report` is the preferred way to show one Loko source file as an
inspectable compiler/workbench artifact. It runs the normal session pipeline
and writes a single self-contained HTML file with embedded CSS, JavaScript, and
structured JSON data.

```sh
cargo run -p interloko_lab -- report tools/interloko_lab/fixtures/staged_records.loko --output /tmp/staged-records.html
```

Open the generated file directly from the filesystem.

## What It Shows

The first viewport gives the compact story: source file, runtime result,
diagnostic count, selected grammar stack, and staged-region count.

The rest of the report shows:

- original source with line numbers and provenance highlights;
- a source legend for activation directives, staged regions, frontend-form
  spans, and diagnostics;
- the active language environment, grammar stack, and host grammar catalog;
- staged grammar regions, when present;
- active grammar -> frontend forms -> generated AST/semantic object links, with
  materialized Loko shown as a debug projection;
- generated classes, slots, accessors, generics, methods, functions, EOP ids,
  readable object facts, raw inspection facts, and affordances;
- structured diagnostics with codes, spans, labels, and arguments;
- artifact keys, per-artifact work/timing summaries, and the query dependency
  graph;
- a compact runtime execution timeline plus a raw trace dump when the report
  runs `main`;
- lowered IR and `execution_tape` disassembly.

Click a highlighted source span, staged region, frontend form, object, or
artifact to highlight related nodes where the current artifacts expose a
relation.

## Concepts

- **Report:** a static HTML presentation of one compiler session.
- **Language environment:** the selected parsing context. It owns the resolved
  grammar stack and activation provenance for this compile.
- **Grammar catalog:** host-side registry plumbing used to discover available
  environments and staged grammar extensions.
- **Frontend form:** an inert structured form emitted by a trusted staged
  grammar extension and lowered directly into AST definitions.
- **Materialized Loko:** debug text generated from frontend forms for
  inspection and reports. It is not the semantic compiler input.
- **EOP object:** an inspectable environment object with stable id, kind,
  presentation, storage/origin facts, and affordances.
- **Artifact key:** stable query artifact identity used by the incremental
  compiler workbench.

## Boundaries

The report does not bypass the compiler. It uses the same session artifacts as
`run`, `dump`, `eop`, `inspect`, `dispatch`, and `artifacts`.

Reports request normal runtime execution with tracing enabled. The trace uses
the `execution_tape` tracing sink API and records run boundaries, call-frame
scope changes, and host-call scope changes. Instruction events are available
through `interloko_lab trace --instructions`, but are intentionally omitted
from the default report timeline so short demos stay readable.

It is also not a live tool yet. There is no server, editor, watch mode, browser
RPC, persistence, or LSP integration. The generated JavaScript only provides
local relation highlighting inside the static file.

## Good Demo Files

The most complete demo today is:

```sh
cargo run -p interloko_lab -- report tools/interloko_lab/fixtures/staged_records.loko --output /tmp/staged-records.html
```

That file demonstrates staged records syntax becoming frontend class forms,
then generated AST/semantic class, slot, accessor, and setter-generic objects.
It also runs a small `main` function that constructs generated classes, reads
slots, and writes one slot through the synthesized setter generic. The
materialized Loko column remains in the report so readers can compare the
staged syntax to ordinary Loko syntax, but staged class/function forms are
lowered directly into AST definitions rather than reparsed from that debug
text.

For a plain non-staged program:

```sh
cargo run -p interloko_lab -- report tools/interloko_lab/fixtures/generic_area.loko --output /tmp/generic-area.html
```

The report still shows the normal source, runtime result, MOP/EOP objects,
artifact graph, lowered IR, and tape disassembly, with an explicit empty state
for staged regions.

## Future Live Tool

The static report is intentionally shaped like a future workbench screen:
source left, provenance and generated forms in the middle, objects/artifacts
around it, and executable IR available on demand. A live version can reuse the
same conceptual data, but would need separate work for edit sessions, browser
RPC, incremental refresh, persistent views, and richer graph layout.
