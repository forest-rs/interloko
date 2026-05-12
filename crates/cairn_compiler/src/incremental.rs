//! Thin invalidation shell for Cairn compiler query artifacts.
//!
//! Cairn owns artifacts and scheduling. This module only maps compiler query
//! artifacts into `invalidation` keys and exposes affected-order drains.

use alloc::vec::Vec;

use invalidation::{Channel, InvalidationTracker, LazyPolicy};

use crate::sem::{GenericFunctionId, MethodId, Module};

const COMPILER: Channel = Channel::new(0);
const SOURCE: u32 = 0;
const PARSE: u32 = 1;
const AST: u32 = 2;
const BIND: u32 = 3;
const METHOD_LOOKUP_BASE: u32 = 1_000;
const METHOD_LOWER_BASE: u32 = 2_000;
const METHOD_TAPE_BASE: u32 = 3_000;
const GENERIC_DISPATCH_BASE: u32 = 4_000;
const TAPE_PROGRAM: u32 = 5_000;

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
    /// Method lookup result for a method.
    MethodLookup(MethodId),
    /// Lowered method body.
    MethodLowering(MethodId),
    /// Tape function artifact for a method body.
    MethodTape(MethodId),
    /// Generated dispatcher artifact for a generic.
    GenericDispatcher(GenericFunctionId),
    /// Whole verified tape program artifact.
    TapeProgram,
}

impl QueryArtifact {
    fn key(self) -> u32 {
        match self {
            Self::Source => SOURCE,
            Self::Parse => PARSE,
            Self::Ast => AST,
            Self::Bind => BIND,
            Self::MethodLookup(id) => METHOD_LOOKUP_BASE + id.0,
            Self::MethodLowering(id) => METHOD_LOWER_BASE + id.0,
            Self::MethodTape(id) => METHOD_TAPE_BASE + id.0,
            Self::GenericDispatcher(id) => GENERIC_DISPATCH_BASE + id.0,
            Self::TapeProgram => TAPE_PROGRAM,
        }
    }

    fn from_key(key: u32) -> Self {
        match key {
            SOURCE => Self::Source,
            PARSE => Self::Parse,
            AST => Self::Ast,
            BIND => Self::Bind,
            TAPE_PROGRAM => Self::TapeProgram,
            key if (METHOD_LOOKUP_BASE..METHOD_LOWER_BASE).contains(&key) => {
                Self::MethodLookup(MethodId(key - METHOD_LOOKUP_BASE))
            }
            key if (METHOD_LOWER_BASE..METHOD_TAPE_BASE).contains(&key) => {
                Self::MethodLowering(MethodId(key - METHOD_LOWER_BASE))
            }
            key if (METHOD_TAPE_BASE..GENERIC_DISPATCH_BASE).contains(&key) => {
                Self::MethodTape(MethodId(key - METHOD_TAPE_BASE))
            }
            key if (GENERIC_DISPATCH_BASE..TAPE_PROGRAM).contains(&key) => {
                Self::GenericDispatcher(GenericFunctionId(key - GENERIC_DISPATCH_BASE))
            }
            _ => Self::TapeProgram,
        }
    }
}

/// Dependency-aware invalidation shell for compiler query artifacts.
#[derive(Clone, Debug)]
pub struct QueryInvalidation {
    tracker: InvalidationTracker<u32>,
}

impl QueryInvalidation {
    /// Builds a query invalidation graph for a semantic module.
    #[must_use]
    pub fn for_module(module: &Module) -> Self {
        let mut tracker = InvalidationTracker::new();
        add(&mut tracker, QueryArtifact::Parse, QueryArtifact::Source);
        add(&mut tracker, QueryArtifact::Ast, QueryArtifact::Parse);
        add(&mut tracker, QueryArtifact::Bind, QueryArtifact::Ast);
        add(
            &mut tracker,
            QueryArtifact::TapeProgram,
            QueryArtifact::Bind,
        );

        for method in &module.methods {
            add(
                &mut tracker,
                QueryArtifact::MethodLookup(method.id),
                QueryArtifact::Bind,
            );
            add(
                &mut tracker,
                QueryArtifact::MethodLowering(method.id),
                QueryArtifact::MethodLookup(method.id),
            );
            add(
                &mut tracker,
                QueryArtifact::MethodTape(method.id),
                QueryArtifact::MethodLowering(method.id),
            );
            add(
                &mut tracker,
                QueryArtifact::GenericDispatcher(method.generic),
                QueryArtifact::MethodLookup(method.id),
            );
            add(
                &mut tracker,
                QueryArtifact::TapeProgram,
                QueryArtifact::MethodTape(method.id),
            );
        }

        for generic in &module.generics {
            add(
                &mut tracker,
                QueryArtifact::TapeProgram,
                QueryArtifact::GenericDispatcher(generic.id),
            );
        }

        Self { tracker }
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
        self.mark_and_drain(QueryArtifact::MethodLookup(method))
    }

    fn mark_and_drain(&mut self, artifact: QueryArtifact) -> Vec<QueryArtifact> {
        self.tracker
            .mark_with(artifact.key(), COMPILER, &LazyPolicy);
        self.tracker
            .drain_affected_sorted(COMPILER)
            .map(QueryArtifact::from_key)
            .collect()
    }
}

fn add(
    tracker: &mut InvalidationTracker<u32>,
    dependent: QueryArtifact,
    dependency: QueryArtifact,
) {
    tracker
        .add_dependency(dependent.key(), dependency.key(), COMPILER)
        .expect("compiler query graph must remain acyclic");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ast, sem, syntax};

    fn lower_source(source: &str) -> sem::Module {
        let tree = syntax::parse(source);
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

        let mut invalidation = QueryInvalidation::for_module(&module);
        let affected = invalidation.method_changed(MethodId(0));

        assert!(affected.contains(&QueryArtifact::MethodLookup(MethodId(0))));
        assert!(affected.contains(&QueryArtifact::MethodLowering(MethodId(0))));
        assert!(affected.contains(&QueryArtifact::MethodTape(MethodId(0))));
        assert!(affected.contains(&QueryArtifact::GenericDispatcher(GenericFunctionId(0))));
        assert!(affected.contains(&QueryArtifact::TapeProgram));
        assert!(!affected.contains(&QueryArtifact::Parse));
        assert!(!affected.contains(&QueryArtifact::Ast));
        assert!(!affected.contains(&QueryArtifact::Bind));
        assert!(!affected.contains(&QueryArtifact::MethodLookup(MethodId(1))));
        assert!(!affected.contains(&QueryArtifact::MethodLowering(MethodId(1))));
        assert!(!affected.contains(&QueryArtifact::MethodTape(MethodId(1))));
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
        assert!(affected.contains(&QueryArtifact::MethodLookup(MethodId(0))));
        assert!(affected.contains(&QueryArtifact::MethodLowering(MethodId(0))));
        assert!(affected.contains(&QueryArtifact::MethodTape(MethodId(0))));
        assert!(affected.contains(&QueryArtifact::TapeProgram));
    }
}
