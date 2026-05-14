# ADR: Stable-ish Compiler IDs and Artifact Keys

## Status

Accepted for the current single-file Cairn compiler prototype.

## Context

Cairn now has a full source-to-tape pipeline and a thin invalidation shell. The
early semantic IDs (`ClassId`, `FunctionId`, `MethodId`, etc.) are dense indexes
inside one compiled module. They are useful for table lookup, but they are not
stable artifact identities: inserting a preceding method can renumber a later
method.

The next increment needs IDs that survive small edits well enough to make
incremental invalidation credible.

## Decision

Keep dense semantic IDs for indexing, and add stable-ish IDs alongside them:

- `SourceFileId` identifies a logical source file. It is derived from a caller
  supplied path through `syntax::parse_file`; `syntax::parse` uses an anonymous
  source id for backwards compatibility.
- `SyntaxNodeId` identifies CST nodes. Top-level definition nodes are anchored
  by definition kind and source name, so whitespace/comment/body edits do not
  renumber the definition node. Expression and body nodes are structural hashes
  of non-trivia token text and child node IDs, so they are whitespace-insensitive
  but may change when the expression changes.
- `AstNodeId` is derived from the source `SyntaxNodeId`, with child IDs for AST
  nodes that do not have a direct CST node.
- `DefinitionId` identifies named compiler definitions by source file, kind,
  name, and a discriminator. Methods use the generic name plus specializer as
  their stable identity. Slots use the class name as discriminator.
- `ArtifactKey` identifies query artifacts by source file, artifact kind, and
  optional `DefinitionId`.
- The in-memory incremental engine exposes additional artifact kinds for
  diagnostics, the whole lowered executable module, and runtime results. These
  are still keyed by `SourceFileId` and optional `DefinitionId` through the same
  `ArtifactKey` mechanism.

Existing dense IDs keep their current role:

- `FunctionId`, `ClassId`, `GenericFunctionId`, `MethodId`, `SlotId`, and
  `LocalId` are per-semantic-module table indexes. They are intentionally not
  durable across definition insertion or removal.
- `Span` remains a byte range in the current source text. It is diagnostic
  location data, not an edit-stable identity. Tape `SpanId` values are derived
  from current source spans during emission and have the same non-durable
  stability model.

The invalidation tracker still uses dense internal indexes because
`invalidation 0.2` requires dense keys. `QueryInvalidation` maps public
`ArtifactKey` values to those dense indexes internally.

## Guarantees

Stable across:

- whitespace and comment edits around a definition;
- body edits that do not rename the definition;
- inserting unrelated definitions before an existing named definition;
- method ordinal changes when the method's generic name and specializer stay the
  same.

Not guaranteed across:

- renaming a definition;
- changing a method specializer;
- duplicate definitions with the same stable discriminator;
- multi-file module identity, because Cairn does not yet have a module system;
- expression edits, except that trivia-only expression edits are ignored.

## Consequences

- Existing dense semantic IDs remain boring table indexes.
- Incremental artifacts can be addressed by stable keys without rewriting the
  compiler pipeline around persistence yet.
- Adding new artifact kinds is a public API extension. Callers that exhaustively
  match `ArtifactKind` or `QueryArtifact` must handle diagnostics,
  lowered-module, and runtime-result artifacts.
- Future multi-file work should replace anonymous source IDs at driver entry
  points and introduce module/package identity before stable IDs become
  externally durable.
