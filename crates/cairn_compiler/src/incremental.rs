//! Thin invalidation shell for Cairn compiler query artifacts.
//!
//! Cairn owns artifacts and scheduling. This module only maps compiler query
//! artifacts into `invalidation` keys and exposes affected-order drains.

use alloc::vec::Vec;

use invalidation::{Channel, InvalidationTracker, LazyPolicy};

use crate::identity::{ArtifactKey, ArtifactKind, DefinitionId, SourceFileId};
use crate::sem::{MethodId, Module};

const COMPILER: Channel = Channel::new(0);

/// Query artifact tracked by the invalidation shell.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum QueryArtifact {
    /// Source text input.
    Source,
    /// Parse/CST result.
    Parse,
    /// AST lowering result.
    Ast,
    /// Binding and semantic symbol table result.
    Bind,
    /// Aggregated diagnostics result.
    Diagnostics,
    /// Method lookup result for a method.
    MethodLookup(DefinitionId),
    /// Lowered method body.
    MethodLowering(DefinitionId),
    /// Tape function artifact for a method body.
    MethodTape(DefinitionId),
    /// Generated dispatcher artifact for a generic.
    GenericDispatcher(DefinitionId),
    /// Whole lowered executable IR module.
    LoweredModule,
    /// Whole verified tape program artifact.
    TapeProgram,
    /// Runtime result artifact for an optional `main` run.
    RuntimeResult,
}

/// A query artifact and its stable external key.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ArtifactReport {
    /// Query artifact.
    pub artifact: QueryArtifact,
    /// Stable artifact key.
    pub key: ArtifactKey,
}

/// Dependency edge in the compiler artifact graph.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ArtifactEdge {
    /// Artifact affected by `dependency`.
    pub dependent: QueryArtifact,
    /// Artifact this edge depends on.
    pub dependency: QueryArtifact,
}

impl QueryArtifact {
    /// Returns the stable artifact key for this query artifact in `source_file`.
    #[must_use]
    pub fn key(self, source_file: SourceFileId) -> ArtifactKey {
        match self {
            Self::Source => ArtifactKey::new(source_file, ArtifactKind::Source, None),
            Self::Parse => ArtifactKey::new(source_file, ArtifactKind::Parse, None),
            Self::Ast => ArtifactKey::new(source_file, ArtifactKind::Ast, None),
            Self::Bind => ArtifactKey::new(source_file, ArtifactKind::Bind, None),
            Self::Diagnostics => ArtifactKey::new(source_file, ArtifactKind::Diagnostics, None),
            Self::MethodLookup(id) => {
                ArtifactKey::new(source_file, ArtifactKind::MethodLookup, Some(id))
            }
            Self::MethodLowering(id) => {
                ArtifactKey::new(source_file, ArtifactKind::MethodLowering, Some(id))
            }
            Self::MethodTape(id) => {
                ArtifactKey::new(source_file, ArtifactKind::MethodTape, Some(id))
            }
            Self::GenericDispatcher(id) => {
                ArtifactKey::new(source_file, ArtifactKind::GenericDispatcher, Some(id))
            }
            Self::LoweredModule => ArtifactKey::new(source_file, ArtifactKind::LoweredModule, None),
            Self::TapeProgram => ArtifactKey::new(source_file, ArtifactKind::TapeProgram, None),
            Self::RuntimeResult => ArtifactKey::new(source_file, ArtifactKind::RuntimeResult, None),
        }
    }
}

/// Dependency-aware invalidation shell for compiler query artifacts.
#[derive(Clone, Debug)]
pub struct QueryInvalidation {
    source_file: SourceFileId,
    tracker: InvalidationTracker<usize>,
    artifacts: Vec<(ArtifactKey, QueryArtifact)>,
    edges: Vec<ArtifactEdge>,
    methods: Vec<DefinitionId>,
}

impl QueryInvalidation {
    /// Builds a query invalidation graph for a semantic module.
    #[must_use]
    pub fn for_module(module: &Module) -> Self {
        let mut this = Self {
            source_file: module.source_file,
            tracker: InvalidationTracker::new(),
            artifacts: Vec::new(),
            edges: Vec::new(),
            methods: module
                .methods
                .iter()
                .map(|method| method.definition)
                .collect(),
        };
        this.add(QueryArtifact::Parse, QueryArtifact::Source);
        this.add(QueryArtifact::Ast, QueryArtifact::Parse);
        this.add(QueryArtifact::Bind, QueryArtifact::Ast);
        this.add(QueryArtifact::Diagnostics, QueryArtifact::Bind);
        this.add(QueryArtifact::LoweredModule, QueryArtifact::Bind);
        this.add(QueryArtifact::TapeProgram, QueryArtifact::Bind);
        this.add(QueryArtifact::TapeProgram, QueryArtifact::LoweredModule);
        this.add(QueryArtifact::RuntimeResult, QueryArtifact::TapeProgram);

        for method in &module.methods {
            let method = method.definition;
            this.add(QueryArtifact::MethodLookup(method), QueryArtifact::Bind);
            this.add(
                QueryArtifact::MethodLowering(method),
                QueryArtifact::MethodLookup(method),
            );
            this.add(
                QueryArtifact::MethodTape(method),
                QueryArtifact::MethodLowering(method),
            );
            this.add(
                QueryArtifact::LoweredModule,
                QueryArtifact::MethodLowering(method),
            );
            this.add(
                QueryArtifact::TapeProgram,
                QueryArtifact::MethodTape(method),
            );
        }

        for generic in &module.generics {
            this.add(
                QueryArtifact::LoweredModule,
                QueryArtifact::GenericDispatcher(generic.definition),
            );
            this.add(
                QueryArtifact::TapeProgram,
                QueryArtifact::GenericDispatcher(generic.definition),
            );
        }

        for method in &module.methods {
            let Some(generic) = module.generics.get(method.generic.0 as usize) else {
                continue;
            };
            this.add(
                QueryArtifact::GenericDispatcher(generic.definition),
                QueryArtifact::MethodLookup(method.definition),
            );
        }

        this
    }

    /// Returns this graph's source file identity.
    #[must_use]
    pub const fn source_file(&self) -> SourceFileId {
        self.source_file
    }

    /// Returns a stable key for `artifact`.
    #[must_use]
    pub fn key_for(&self, artifact: QueryArtifact) -> ArtifactKey {
        artifact.key(self.source_file)
    }

    /// Returns known artifacts and their stable keys.
    #[must_use]
    pub fn artifact_reports(&self) -> Vec<ArtifactReport> {
        self.artifacts
            .iter()
            .map(|(key, artifact)| ArtifactReport {
                artifact: *artifact,
                key: *key,
            })
            .collect()
    }

    /// Returns artifact dependency edges.
    #[must_use]
    pub fn artifact_edges(&self) -> &[ArtifactEdge] {
        &self.edges
    }

    /// Marks source text changed and drains affected artifacts in dependency order.
    pub fn source_changed(&mut self) -> Vec<QueryArtifact> {
        self.mark_and_drain(QueryArtifact::Source)
    }

    /// Marks one method lookup changed and drains affected artifacts in dependency order.
    ///
    /// This represents a fine-grained edit after parsing/binding has identified
    /// which method body changed.
    pub fn method_changed(&mut self, method: MethodId) -> Vec<QueryArtifact> {
        let Some(definition) = self.methods.get(method.0 as usize).copied() else {
            return Vec::new();
        };
        self.method_definition_changed(definition)
    }

    /// Marks one method definition changed and drains affected artifacts.
    pub fn method_definition_changed(&mut self, method: DefinitionId) -> Vec<QueryArtifact> {
        self.mark_and_drain(QueryArtifact::MethodLookup(method))
    }

    fn mark_and_drain(&mut self, artifact: QueryArtifact) -> Vec<QueryArtifact> {
        let key = self.track(artifact);
        self.tracker.mark_with(key, COMPILER, &LazyPolicy);
        let keys: Vec<_> = self.tracker.drain_affected_sorted(COMPILER).collect();
        keys.into_iter()
            .map(|key| self.artifact_for_key(key))
            .collect()
    }

    fn add(&mut self, dependent: QueryArtifact, dependency: QueryArtifact) {
        let dependent = self.track(dependent);
        let dependency = self.track(dependency);
        self.edges.push(ArtifactEdge {
            dependent: self.artifact_for_key(dependent),
            dependency: self.artifact_for_key(dependency),
        });
        self.tracker
            .add_dependency(dependent, dependency, COMPILER)
            .expect("compiler query graph must remain acyclic");
    }

    fn artifact_for_key(&self, key: usize) -> QueryArtifact {
        self.artifacts
            .get(key)
            .map_or(QueryArtifact::TapeProgram, |(_, artifact)| *artifact)
    }

    fn track(&mut self, artifact: QueryArtifact) -> usize {
        let key = artifact.key(self.source_file);
        if let Some(index) = self
            .artifacts
            .iter()
            .position(|(existing, _)| *existing == key)
        {
            return index;
        }
        let index = self.artifacts.len();
        self.artifacts.push((key, artifact));
        index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ast, identity::SourceFileId, sem, syntax};

    fn lower_source(source: &str) -> sem::Module {
        lower_file(SourceFileId::ANONYMOUS, source)
    }

    fn lower_file(source_file: SourceFileId, source: &str) -> sem::Module {
        let tree = syntax::parse_file(source_file, source);
        let ast = ast::lower(&tree);
        sem::lower(&ast)
    }

    #[test]
    fn method_change_invalidates_only_related_lookup_lowering_and_tape() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius;\nend;\n\
             define class <square> (<object>)\n  slot side;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
             define method area(s :: <square>)\n  s.side * s.side\nend;",
        );
        assert_eq!(module.diagnostics, Vec::new());
        let circle = module.methods[0].definition;
        let square = module.methods[1].definition;
        let area = module.generics[0].definition;

        let mut invalidation = QueryInvalidation::for_module(&module);
        let affected = invalidation.method_changed(MethodId(0));

        assert!(affected.contains(&QueryArtifact::MethodLookup(circle)));
        assert!(affected.contains(&QueryArtifact::MethodLowering(circle)));
        assert!(affected.contains(&QueryArtifact::MethodTape(circle)));
        assert!(affected.contains(&QueryArtifact::GenericDispatcher(area)));
        assert!(affected.contains(&QueryArtifact::TapeProgram));
        assert!(!affected.contains(&QueryArtifact::Parse));
        assert!(!affected.contains(&QueryArtifact::Ast));
        assert!(!affected.contains(&QueryArtifact::Bind));
        assert!(!affected.contains(&QueryArtifact::MethodLookup(square)));
        assert!(!affected.contains(&QueryArtifact::MethodLowering(square)));
        assert!(!affected.contains(&QueryArtifact::MethodTape(square)));
    }

    #[test]
    fn source_change_reaches_parse_bind_methods_and_tape() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius * c.radius\nend;",
        );
        let mut invalidation = QueryInvalidation::for_module(&module);
        let affected = invalidation.source_changed();

        assert!(affected.contains(&QueryArtifact::Source));
        assert!(affected.contains(&QueryArtifact::Parse));
        assert!(affected.contains(&QueryArtifact::Ast));
        assert!(affected.contains(&QueryArtifact::Bind));
        let method = module.methods[0].definition;
        assert!(affected.contains(&QueryArtifact::MethodLookup(method)));
        assert!(affected.contains(&QueryArtifact::MethodLowering(method)));
        assert!(affected.contains(&QueryArtifact::MethodTape(method)));
        assert!(affected.contains(&QueryArtifact::TapeProgram));
    }

    #[test]
    fn artifact_keys_follow_definition_ids_across_insertions() {
        let source_file = SourceFileId::from_path("examples/area.cairn");
        let before = lower_file(
            source_file,
            "define class <circle> (<object>)\n  slot radius;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius * c.radius\nend;",
        );
        let edited = lower_file(
            source_file,
            "define class <circle> (<object>)\n  slot radius;\nend;\n\
             define class <square> (<object>)\n  slot side;\nend;\n\
             define generic area(x);\n\
             define method area(s :: <square>)\n  s.side * s.side\nend;\n\
             define method area(c :: <circle>)\n  c.radius * c.radius\nend;",
        );

        let circle_method = before.methods[0].definition;
        let edited_circle = edited
            .methods
            .iter()
            .find(|method| method.definition == circle_method)
            .expect("circle method definition should keep its stable id");
        assert_ne!(edited_circle.id, MethodId(0));

        let before_graph = QueryInvalidation::for_module(&before);
        let edited_graph = QueryInvalidation::for_module(&edited);
        let before_key = before_graph.key_for(QueryArtifact::MethodLookup(circle_method));
        let edited_key =
            edited_graph.key_for(QueryArtifact::MethodLookup(edited_circle.definition));

        assert_eq!(before_key, edited_key);
    }
}
