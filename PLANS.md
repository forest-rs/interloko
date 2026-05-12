# Plans

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
