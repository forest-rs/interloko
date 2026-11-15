//! Thin invalidation shell for Interloko compiler query artifacts.
//!
//! Interloko owns artifacts and scheduling. This module only maps compiler query
//! artifacts into `invalidation` keys and exposes affected-order drains.
//!
//! # Early cutoff
//!
//! Some artifacts are firewalls: [`QueryArtifact::has_early_cutoff`]. When a
//! drain reaches one, the caller says whether its value actually changed, and
//! an unchanged firewall stops the drain there. A body edit that keeps a
//! callee's [`QueryArtifact::Contract`] therefore stops at the contract and
//! never reaches its callers' typing.
//!
//! # Typing edges
//!
//! A [`QueryArtifact::DefinitionTyping`] depends on its own
//! [`QueryArtifact::DefinitionSem`], on the contracts its reasons cite, on
//! the [`QueryArtifact::MethodTable`] of every generic its verdicts assume
//! closed, and on the [`QueryArtifact::ClassCone`] of every closed or sealed
//! class they assume. A class cone covers the subclasses and their sealing,
//! so it changes when a sealed class gains or loses a subclass or is
//! sealed or unsealed (such class edits renumber classes and re-check every
//! definition today, so the edge is what a finer drain will follow). A
//! method table also depends on its generic's own sem output, so sealing or
//! unsealing a generic, a local edit, reaches every decision that assumes
//! its method set. These edges come from the typing artifact the graph is built with
//! ([`crate::typing::DefinitionTyping::inputs`]); when typing did not run,
//! the graph has none of them.
//!
//! Lowering trusts the same contracts at each call, so a method's
//! [`QueryArtifact::MethodLowering`] (and, for functions and constants, the
//! [`QueryArtifact::LoweredModule`]) depends on the contracts its body cites.
//!
//! # Codegen policy edges
//!
//! The [`QueryArtifact::LoweredModule`] and every method's
//! [`QueryArtifact::MethodLowering`] always depend on the
//! [`QueryArtifact::CodegenPolicy`], so changing the policy lowers again.
//! Under [`CodegenPolicy::Proved`] lowering also acts on typing decisions:
//! a method's lowering (for functions and constants, the lowered module)
//! then depends on its own [`QueryArtifact::DefinitionTyping`] and on that
//! of every constant it reads, since a constant's value is lowered where it
//! is read. Under [`CodegenPolicy::Off`] lowering reads no decision, and a
//! typing change reaches no lowering.
//!
//! [`QueryArtifact::has_early_cutoff`]: crate::incremental::QueryArtifact::has_early_cutoff
//! [`QueryArtifact::Contract`]: crate::incremental::QueryArtifact::Contract
//! [`QueryArtifact::DefinitionTyping`]: crate::incremental::QueryArtifact::DefinitionTyping
//! [`QueryArtifact::DefinitionSem`]: crate::incremental::QueryArtifact::DefinitionSem
//! [`QueryArtifact::MethodTable`]: crate::incremental::QueryArtifact::MethodTable
//! [`QueryArtifact::ClassCone`]: crate::incremental::QueryArtifact::ClassCone
//! [`QueryArtifact::MethodLowering`]: crate::incremental::QueryArtifact::MethodLowering
//! [`QueryArtifact::LoweredModule`]: crate::incremental::QueryArtifact::LoweredModule
//! [`QueryArtifact::CodegenPolicy`]: crate::incremental::QueryArtifact::CodegenPolicy
//! [`CodegenPolicy::Proved`]: crate::lower::CodegenPolicy::Proved
//! [`CodegenPolicy::Off`]: crate::lower::CodegenPolicy::Off

use alloc::{boxed::Box, collections::BTreeMap, format, string::String, vec, vec::Vec};

use invalidation::{AddDependency, Channel, InvalidationTracker, LazyPolicy};

use crate::identity::{ArtifactKey, ArtifactKind, DefinitionId, SourceFileId};
use crate::lower::CodegenPolicy;
use crate::sem::{MethodId, Module};
use crate::typing::TypingArtifact;

const COMPILER: Channel = Channel::new(0);

/// Query artifact tracked by the invalidation shell.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum QueryArtifact {
    /// Source text input.
    Source,
    /// Language environment resolution result.
    EnvironmentResolution,
    /// Resolved grammar stack result.
    GrammarStack,
    /// Grammar frontend parse report.
    GrammarParse,
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
    /// Soft typing decisions for one function, method, or constant.
    DefinitionTyping(DefinitionId),
    /// Soft typing result for the module: per-definition decisions under the
    /// session's typing configuration.
    Typing,
    /// Session-owned soft typing configuration input. Changing it re-derives
    /// typing diagnostics without re-checking any definition.
    TypingConfig,
    /// Semantic output of one function, method, generic, or constant: its
    /// signature and body as binding resolved them.
    DefinitionSem(DefinitionId),
    /// Type contract of one definition: what code compiled against it
    /// trusts. For a function, method, or generic this is
    /// [`crate::sem::Module::contract`]; for a constant, the type its value
    /// is inferred to have; for a slot, its accessor's result. Has early
    /// cutoff: a body edit that keeps the contract stops here.
    Contract(DefinitionId),
    /// Method table of one generic: its methods and their specializers,
    /// which a dispatch verdict assumes closed. Has early cutoff.
    MethodTable(DefinitionId),
    /// Cone of one class: whether it is closed and which classes it holds,
    /// each with its sealing and abstractness, which a verdict about a
    /// closed or sealed class assumes. Has early cutoff.
    ClassCone(DefinitionId),
    /// Session-owned codegen policy input
    /// ([`crate::lower::CodegenPolicy`]): whether lowering acts on proved
    /// typing facts.
    CodegenPolicy,
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
    /// Returns the stable artifact kind.
    #[must_use]
    pub const fn kind(self) -> ArtifactKind {
        match self {
            Self::Source => ArtifactKind::Source,
            Self::EnvironmentResolution => ArtifactKind::EnvironmentResolution,
            Self::GrammarStack => ArtifactKind::GrammarStack,
            Self::GrammarParse => ArtifactKind::GrammarParse,
            Self::Parse => ArtifactKind::Parse,
            Self::Ast => ArtifactKind::Ast,
            Self::Bind => ArtifactKind::Bind,
            Self::Diagnostics => ArtifactKind::Diagnostics,
            Self::MethodLookup(_) => ArtifactKind::MethodLookup,
            Self::MethodLowering(_) => ArtifactKind::MethodLowering,
            Self::MethodTape(_) => ArtifactKind::MethodTape,
            Self::GenericDispatcher(_) => ArtifactKind::GenericDispatcher,
            Self::LoweredModule => ArtifactKind::LoweredModule,
            Self::TapeProgram => ArtifactKind::TapeProgram,
            Self::RuntimeResult => ArtifactKind::RuntimeResult,
            Self::DefinitionTyping(_) => ArtifactKind::DefinitionTyping,
            Self::Typing => ArtifactKind::Typing,
            Self::TypingConfig => ArtifactKind::TypingConfig,
            Self::DefinitionSem(_) => ArtifactKind::DefinitionSem,
            Self::Contract(_) => ArtifactKind::Contract,
            Self::MethodTable(_) => ArtifactKind::MethodTable,
            Self::ClassCone(_) => ArtifactKind::ClassCone,
            Self::CodegenPolicy => ArtifactKind::CodegenPolicy,
        }
    }

    /// Returns the definition this artifact belongs to, if it is
    /// per-definition.
    #[must_use]
    pub const fn definition(self) -> Option<DefinitionId> {
        match self {
            Self::MethodLookup(id)
            | Self::MethodLowering(id)
            | Self::MethodTape(id)
            | Self::GenericDispatcher(id)
            | Self::DefinitionTyping(id)
            | Self::DefinitionSem(id)
            | Self::Contract(id)
            | Self::MethodTable(id)
            | Self::ClassCone(id) => Some(id),
            Self::Source
            | Self::EnvironmentResolution
            | Self::GrammarStack
            | Self::GrammarParse
            | Self::Parse
            | Self::Ast
            | Self::Bind
            | Self::Diagnostics
            | Self::LoweredModule
            | Self::TapeProgram
            | Self::RuntimeResult
            | Self::Typing
            | Self::TypingConfig
            | Self::CodegenPolicy => None,
        }
    }

    /// Whether a drain stops at this artifact when its value is unchanged.
    ///
    /// These are the firewalls between one definition and the definitions
    /// that rely on it: a contract, a generic's method table, and a class's
    /// cone. See [`QueryInvalidation::changed_with_cutoff`].
    #[must_use]
    pub const fn has_early_cutoff(self) -> bool {
        matches!(
            self,
            Self::Contract(_) | Self::MethodTable(_) | Self::ClassCone(_)
        )
    }

    /// Returns the stable label used by dumps and presentation tools.
    #[must_use]
    pub fn label(self) -> String {
        let kind = self.kind().as_str();
        match self.definition() {
            Some(id) => format!("{kind} {id}"),
            None => kind.into(),
        }
    }

    /// Returns the stable artifact key for this query artifact in `source_file`.
    #[must_use]
    pub fn key(self, source_file: SourceFileId) -> ArtifactKey {
        ArtifactKey::new(source_file, self.kind(), self.definition())
    }
}

/// Dependency-aware invalidation shell for compiler query artifacts.
#[derive(Clone, Debug)]
pub struct QueryInvalidation {
    source_file: SourceFileId,
    /// Boxed: the tracker keeps fixed per-channel tables inline (about 9 KiB),
    /// and this value travels inside every [`SessionArtifacts`] that is moved,
    /// cloned or returned. Inline, it made each such stack slot 11 KiB, and
    /// unoptimized frames that hold several of them overflowed a 1 MiB stack.
    ///
    /// [`SessionArtifacts`]: crate::session::SessionArtifacts
    tracker: Box<InvalidationTracker<usize>>,
    artifacts: Vec<(ArtifactKey, QueryArtifact)>,
    /// Tracker key of each artifact, by stable key.
    indices: BTreeMap<ArtifactKey, usize>,
    edges: Vec<ArtifactEdge>,
    methods: Vec<DefinitionId>,
}

/// The constants whose values lowering `definition` lowers inline: those
/// its typing cites, and transitively those their values read. Typing cites
/// only the constants a body reads directly, since it types a constant's
/// value without keeping the reasons.
pub(crate) fn constants_read(
    module: &Module,
    typing: &TypingArtifact,
    definition: DefinitionId,
) -> Vec<DefinitionId> {
    let is_constant = |definition: DefinitionId| {
        module
            .constants
            .iter()
            .any(|constant| constant.definition == definition)
    };
    let mut read = Vec::new();
    let mut pending = vec![definition];
    while let Some(current) = pending.pop() {
        let Some(typed) = typing.definition(current) else {
            continue;
        };
        for cited in typed.inputs().contracts {
            if is_constant(cited) && cited != definition && !read.contains(&cited) {
                read.push(cited);
                pending.push(cited);
            }
        }
    }
    read.sort_unstable();
    read
}

impl QueryInvalidation {
    /// Builds the query invalidation graph for a semantic module, the
    /// typing artifact checked against it, and the codegen policy it is
    /// lowered under.
    ///
    /// The typing artifact supplies the edges into each definition's typing
    /// and lowering from the contracts, method tables, and class cones it
    /// relies on. A blocked typing artifact supplies none. Under
    /// [`CodegenPolicy::Proved`], each lowering also depends on the typing
    /// it acts on; see the module docs.
    #[must_use]
    pub fn for_module(module: &Module, typing: &TypingArtifact, policy: CodegenPolicy) -> Self {
        let mut this = Self {
            source_file: module.source_file,
            tracker: Box::new(InvalidationTracker::new()),
            artifacts: Vec::new(),
            indices: BTreeMap::new(),
            edges: Vec::new(),
            methods: module
                .methods
                .iter()
                .map(|method| method.definition)
                .collect(),
        };
        this.add(QueryArtifact::EnvironmentResolution, QueryArtifact::Source);
        this.add(
            QueryArtifact::GrammarStack,
            QueryArtifact::EnvironmentResolution,
        );
        this.add(QueryArtifact::GrammarParse, QueryArtifact::GrammarStack);
        this.add(QueryArtifact::Parse, QueryArtifact::GrammarParse);
        this.add(QueryArtifact::Ast, QueryArtifact::Parse);
        this.add(QueryArtifact::Bind, QueryArtifact::Ast);
        this.add(QueryArtifact::Diagnostics, QueryArtifact::Bind);
        this.add(QueryArtifact::Typing, QueryArtifact::Bind);
        this.add(QueryArtifact::Typing, QueryArtifact::TypingConfig);
        this.add(QueryArtifact::Diagnostics, QueryArtifact::Typing);
        this.add(QueryArtifact::LoweredModule, QueryArtifact::Bind);
        this.add(QueryArtifact::TapeProgram, QueryArtifact::Bind);
        this.add(QueryArtifact::TapeProgram, QueryArtifact::LoweredModule);
        this.add(QueryArtifact::RuntimeResult, QueryArtifact::TapeProgram);
        this.add(QueryArtifact::LoweredModule, QueryArtifact::CodegenPolicy);

        for generic in &module.generics {
            let generic = generic.definition;
            this.add(QueryArtifact::DefinitionSem(generic), QueryArtifact::Bind);
            this.add(
                QueryArtifact::GenericDispatcher(generic),
                QueryArtifact::DefinitionSem(generic),
            );
            this.add(
                QueryArtifact::Contract(generic),
                QueryArtifact::DefinitionSem(generic),
            );
            this.add(QueryArtifact::MethodTable(generic), QueryArtifact::Bind);
            // Sealing the generic changes what a dispatch over it assumes.
            this.add(
                QueryArtifact::MethodTable(generic),
                QueryArtifact::DefinitionSem(generic),
            );
        }

        for method in &module.methods {
            let generic = module.generics.get(method.generic.0 as usize);
            let method = method.definition;
            this.add(QueryArtifact::DefinitionSem(method), QueryArtifact::Bind);
            this.add(QueryArtifact::MethodLookup(method), QueryArtifact::Bind);
            this.add(
                QueryArtifact::MethodLookup(method),
                QueryArtifact::DefinitionSem(method),
            );
            this.add(
                QueryArtifact::MethodLowering(method),
                QueryArtifact::MethodLookup(method),
            );
            this.add(
                QueryArtifact::MethodLowering(method),
                QueryArtifact::DefinitionSem(method),
            );
            this.add(
                QueryArtifact::MethodLowering(method),
                QueryArtifact::CodegenPolicy,
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
            this.add(
                QueryArtifact::Contract(method),
                QueryArtifact::DefinitionSem(method),
            );
            let Some(generic) = generic else {
                continue;
            };
            this.add(
                QueryArtifact::GenericDispatcher(generic.definition),
                QueryArtifact::MethodLookup(method),
            );
            this.add(
                QueryArtifact::MethodTable(generic.definition),
                QueryArtifact::MethodLookup(method),
            );
            // A method's results are its generic's.
            this.add(
                QueryArtifact::Contract(method),
                QueryArtifact::Contract(generic.definition),
            );
            // Its body assumes the generic's parameter ranges and checks
            // its results against the generic's, so its typing and lowering
            // follow the generic's contract wherever that declares a range.
            // (An open result is inferred from the methods, so an edge from
            // every one would make sibling methods depend on each other.)
            if generic.param_ranges.iter().any(Option::is_some)
                || generic.result_ranges.iter().any(Option::is_some)
            {
                this.add(
                    QueryArtifact::DefinitionTyping(method),
                    QueryArtifact::Contract(generic.definition),
                );
                this.add(
                    QueryArtifact::MethodLowering(method),
                    QueryArtifact::Contract(generic.definition),
                );
            }
            // An open result is inferred from every method's body.
            if generic.return_annotation.is_none() {
                this.add(
                    QueryArtifact::Contract(generic.definition),
                    QueryArtifact::DefinitionSem(method),
                );
            }
        }

        for function in &module.functions {
            let function = function.definition;
            this.add(QueryArtifact::DefinitionSem(function), QueryArtifact::Bind);
            this.add(
                QueryArtifact::LoweredModule,
                QueryArtifact::DefinitionSem(function),
            );
            this.add(
                QueryArtifact::Contract(function),
                QueryArtifact::DefinitionSem(function),
            );
        }

        for constant in &module.constants {
            let constant = constant.definition;
            this.add(QueryArtifact::DefinitionSem(constant), QueryArtifact::Bind);
            this.add(
                QueryArtifact::LoweredModule,
                QueryArtifact::DefinitionSem(constant),
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

        this.add_typing_edges(module, typing, policy);
        this
    }

    /// Adds the edges the typing artifact names: into each definition's
    /// typing, into the lowering that trusts the same contracts, and into
    /// each constant's inferred contract.
    fn add_typing_edges(
        &mut self,
        module: &Module,
        typing: &TypingArtifact,
        policy: CodegenPolicy,
    ) {
        let is_constant = |definition: DefinitionId| {
            module
                .constants
                .iter()
                .any(|constant| constant.definition == definition)
        };
        let is_method = |definition: DefinitionId| {
            module
                .methods
                .iter()
                .any(|method| method.definition == definition)
        };
        for typed in &typing.definitions {
            let definition = typed.definition;
            let inputs = typed.inputs();
            self.add(
                QueryArtifact::DefinitionTyping(definition),
                QueryArtifact::DefinitionSem(definition),
            );
            self.add(
                QueryArtifact::Typing,
                QueryArtifact::DefinitionTyping(definition),
            );
            let lowering = if is_method(definition) {
                QueryArtifact::MethodLowering(definition)
            } else {
                QueryArtifact::LoweredModule
            };
            // A constant's value is lowered where it is read, and so is
            // every constant that value reads in turn.
            let inlined = constants_read(module, typing, definition);
            for constant in &inlined {
                self.add(lowering, QueryArtifact::DefinitionSem(*constant));
            }
            // Proved lowering acts on this definition's decisions, and on
            // those of each constant whose value it lowers inline.
            if policy == CodegenPolicy::Proved {
                self.add(lowering, QueryArtifact::DefinitionTyping(definition));
                for constant in inlined {
                    self.add(lowering, QueryArtifact::DefinitionTyping(constant));
                }
            }
            for cited in inputs.contracts {
                self.add_contract_node(module, cited);
                self.add(
                    QueryArtifact::DefinitionTyping(definition),
                    QueryArtifact::Contract(cited),
                );
                // Lowering trusts a constant's binding category, not the
                // type typing infers for its value.
                let trusted = if is_constant(cited) {
                    QueryArtifact::DefinitionSem(cited)
                } else {
                    QueryArtifact::Contract(cited)
                };
                self.add(lowering, trusted);
            }
            for generic in inputs.generics {
                if let Some(generic) = module.generics.get(generic.0 as usize) {
                    self.add(
                        QueryArtifact::DefinitionTyping(definition),
                        QueryArtifact::MethodTable(generic.definition),
                    );
                }
            }
            // A closed class's cone, and a sealed class's pieces, are both
            // read from its class cone, which covers every subclass and its
            // sealing.
            for class in inputs.classes.into_iter().chain(inputs.sealed) {
                if let Some(class) = module.classes.get(class.0 as usize) {
                    self.add(
                        QueryArtifact::ClassCone(class.definition),
                        QueryArtifact::Bind,
                    );
                    self.add(
                        QueryArtifact::DefinitionTyping(definition),
                        QueryArtifact::ClassCone(class.definition),
                    );
                }
            }
        }

        // A constant's contract is the type typing infers for its value,
        // which follows every constant it reads and every other contract
        // those values cite. Constants may read each other in a cycle, so
        // the closure is over their sem output rather than their contracts.
        for constant in &module.constants {
            let mut closure = vec![constant.definition];
            let mut index = 0;
            while let Some(current) = closure.get(index).copied() {
                index += 1;
                self.add(
                    QueryArtifact::Contract(constant.definition),
                    QueryArtifact::DefinitionSem(current),
                );
                let Some(typed) = typing.definition(current) else {
                    continue;
                };
                for cited in typed.inputs().contracts {
                    if is_constant(cited) {
                        if !closure.contains(&cited) {
                            closure.push(cited);
                        }
                    } else {
                        self.add_contract_node(module, cited);
                        self.add(
                            QueryArtifact::Contract(constant.definition),
                            QueryArtifact::Contract(cited),
                        );
                    }
                }
            }
        }
    }

    /// Ensures a cited contract has its own dependencies. Function, method,
    /// generic, and constant contracts already do; anything else is a slot
    /// accessor's, which binding derives from its class.
    fn add_contract_node(&mut self, module: &Module, definition: DefinitionId) {
        let known = module
            .functions
            .iter()
            .any(|function| function.definition == definition)
            || module
                .methods
                .iter()
                .any(|method| method.definition == definition)
            || module
                .generics
                .iter()
                .any(|generic| generic.definition == definition)
            || module
                .constants
                .iter()
                .any(|constant| constant.definition == definition);
        if !known {
            self.add(QueryArtifact::Contract(definition), QueryArtifact::Bind);
        }
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

    /// Returns whether `artifact` is a node of this graph.
    #[must_use]
    pub fn contains(&self, artifact: QueryArtifact) -> bool {
        self.index_of(artifact).is_some()
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
    ///
    /// Without knowing what changed, every firewall is assumed changed: the
    /// drain reaches every sender of the method's generic. Use
    /// [`Self::changed_with_cutoff`] when the old and new values are known.
    pub fn method_definition_changed(&mut self, method: DefinitionId) -> Vec<QueryArtifact> {
        self.mark_and_drain(QueryArtifact::DefinitionSem(method))
    }

    /// Marks the typing configuration changed and drains affected artifacts.
    ///
    /// Per-definition typing decisions do not depend on the configuration, so
    /// only the module typing result and the aggregated diagnostics are
    /// affected.
    pub fn typing_config_changed(&mut self) -> Vec<QueryArtifact> {
        self.mark_and_drain(QueryArtifact::TypingConfig)
    }

    /// Marks the codegen policy changed and drains affected artifacts: the
    /// lowered module and everything built from it.
    pub fn codegen_policy_changed(&mut self) -> Vec<QueryArtifact> {
        self.mark_and_drain(QueryArtifact::CodegenPolicy)
    }

    /// Marks `roots` changed and drains the artifacts they affect, in
    /// dependency order, with early cutoff.
    ///
    /// An artifact is affected when it is a root, or when one of its
    /// dependencies is affected and passes the change on. Every dependency
    /// passes a change on except an early-cutoff artifact
    /// ([`QueryArtifact::has_early_cutoff`]) for which `unchanged` returns
    /// true: it is affected (it had to be re-derived) but its value is the
    /// same, so its dependents are not. Roots that are not in the graph are
    /// ignored.
    pub fn changed_with_cutoff(
        &mut self,
        roots: &[QueryArtifact],
        mut unchanged: impl FnMut(QueryArtifact) -> bool,
    ) -> Vec<QueryArtifact> {
        let roots: Vec<usize> = roots
            .iter()
            .filter_map(|root| self.index_of(*root))
            .collect();
        for root in &roots {
            self.tracker.mark_with(*root, COMPILER, &LazyPolicy);
        }
        let order: Vec<usize> = self.tracker.drain_affected_sorted(COMPILER).collect();
        let mut passes_on = vec![None::<bool>; self.artifacts.len()];
        let mut affected = Vec::new();
        for key in order {
            let reached = roots.contains(&key)
                || self
                    .tracker
                    .graph()
                    .dependencies(key, COMPILER)
                    .any(|dependency| passes_on.get(dependency).copied().flatten() == Some(true));
            if !reached {
                continue;
            }
            let artifact = self.artifact_for_key(key);
            let cut_off = artifact.has_early_cutoff() && unchanged(artifact);
            if let Some(slot) = passes_on.get_mut(key) {
                *slot = Some(!cut_off);
            }
            affected.push(artifact);
        }
        affected
    }

    fn mark_and_drain(&mut self, artifact: QueryArtifact) -> Vec<QueryArtifact> {
        self.changed_with_cutoff(&[artifact], |_| false)
    }

    fn add(&mut self, dependent: QueryArtifact, dependency: QueryArtifact) {
        let dependent_key = self.track(dependent);
        let dependency_key = self.track(dependency);
        let added = self
            .tracker
            .add_dependency(dependent_key, dependency_key, COMPILER)
            .expect("compiler query graph must remain acyclic");
        if added == AddDependency::Added {
            self.edges.push(ArtifactEdge {
                dependent,
                dependency,
            });
        }
    }

    fn artifact_for_key(&self, key: usize) -> QueryArtifact {
        self.artifacts
            .get(key)
            .map_or(QueryArtifact::TapeProgram, |(_, artifact)| *artifact)
    }

    fn index_of(&self, artifact: QueryArtifact) -> Option<usize> {
        self.indices.get(&artifact.key(self.source_file)).copied()
    }

    fn track(&mut self, artifact: QueryArtifact) -> usize {
        let key = artifact.key(self.source_file);
        if let Some(index) = self.indices.get(&key) {
            return *index;
        }
        let index = self.artifacts.len();
        self.artifacts.push((key, artifact));
        self.indices.insert(key, index);
        index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ast, grammar_frontend, identity::SourceFileId, sem, typing};

    fn graph(module: &sem::Module) -> QueryInvalidation {
        graph_under(module, CodegenPolicy::Off)
    }

    fn graph_under(module: &sem::Module, policy: CodegenPolicy) -> QueryInvalidation {
        let typing = typing::analyze(module, typing::TypingConfig::default());
        QueryInvalidation::for_module(module, &typing, policy)
    }

    fn lower_source(source: &str) -> sem::Module {
        lower_file(SourceFileId::ANONYMOUS, source)
    }

    fn lower_file(source_file: SourceFileId, source: &str) -> sem::Module {
        let tree = grammar_frontend::parse_file(source_file, source).syntax;
        let ast = ast::lower(&tree);
        sem::lower(&ast)
    }

    #[test]
    fn method_change_invalidates_only_related_lookup_lowering_and_tape() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
             define method area(s :: <square>)\n  s.side * s.side\nend;",
        );
        assert_eq!(module.diagnostics, Vec::new());
        let circle = module.methods[0].definition;
        let square = module.methods[1].definition;
        let area = module.generics[0].definition;

        let mut invalidation = graph(&module);
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
    fn method_change_retypes_senders_only_when_the_method_table_changes() {
        let module = lower_source(
            "define class <circle> (<object>)\nend;\n\
             define class <square> (<object>)\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  1\nend;\n\
             define method area(s :: <square>)\n  2\nend;\n\
             define function main()\n  area(make(<circle>))\nend;",
        );
        assert_eq!(module.diagnostics, Vec::new());
        let circle = module.methods[0].definition;
        let square = module.methods[1].definition;
        let area = module.generics[0].definition;
        let main = module.functions[0].definition;
        let mut invalidation = graph(&module);
        assert!(invalidation.artifact_edges().contains(&ArtifactEdge {
            dependent: QueryArtifact::DefinitionTyping(main),
            dependency: QueryArtifact::MethodTable(area),
        }));

        // A body edit keeps the method table and the generic's contract.
        let affected =
            invalidation.changed_with_cutoff(&[QueryArtifact::DefinitionSem(circle)], |_| true);
        assert!(affected.contains(&QueryArtifact::DefinitionTyping(circle)));
        assert!(affected.contains(&QueryArtifact::MethodTable(area)));
        assert!(affected.contains(&QueryArtifact::Typing));
        assert!(affected.contains(&QueryArtifact::Diagnostics));
        assert!(!affected.contains(&QueryArtifact::DefinitionTyping(square)));
        assert!(!affected.contains(&QueryArtifact::DefinitionTyping(main)));

        // Without knowing the values, the drain assumes the table changed.
        let affected = invalidation.method_changed(MethodId(0));
        assert!(affected.contains(&QueryArtifact::DefinitionTyping(main)));
        assert!(!affected.contains(&QueryArtifact::DefinitionTyping(square)));
    }

    #[test]
    fn callee_contract_is_a_firewall_for_caller_typing_and_lowering() {
        let module = lower_source(
            "define function helper(x :: <integer>)\n  x + 1\nend;\n\
             define generic area(x);\n\
             define method area(x :: <integer>)\n  helper(x)\nend;\n\
             define function main()\n  helper(2)\nend;",
        );
        assert_eq!(module.diagnostics, Vec::new());
        let helper = module.functions[0].definition;
        let main = module.functions[1].definition;
        let method = module.methods[0].definition;
        let mut invalidation = graph(&module);
        for edge in [
            (
                QueryArtifact::DefinitionTyping(main),
                QueryArtifact::Contract(helper),
            ),
            (
                QueryArtifact::MethodLowering(method),
                QueryArtifact::Contract(helper),
            ),
            (
                QueryArtifact::LoweredModule,
                QueryArtifact::Contract(helper),
            ),
        ] {
            assert!(
                invalidation.artifact_edges().contains(&ArtifactEdge {
                    dependent: edge.0,
                    dependency: edge.1,
                }),
                "{edge:?}"
            );
        }

        let body_only =
            invalidation.changed_with_cutoff(&[QueryArtifact::DefinitionSem(helper)], |_| true);
        assert!(body_only.contains(&QueryArtifact::DefinitionTyping(helper)));
        assert!(body_only.contains(&QueryArtifact::Contract(helper)));
        assert!(!body_only.contains(&QueryArtifact::DefinitionTyping(main)));
        assert!(!body_only.contains(&QueryArtifact::DefinitionTyping(method)));
        assert!(!body_only.contains(&QueryArtifact::MethodLowering(method)));

        let contract =
            invalidation.changed_with_cutoff(&[QueryArtifact::DefinitionSem(helper)], |_| false);
        assert!(contract.contains(&QueryArtifact::DefinitionTyping(main)));
        assert!(contract.contains(&QueryArtifact::DefinitionTyping(method)));
        assert!(contract.contains(&QueryArtifact::MethodLowering(method)));
    }

    #[test]
    fn constant_contracts_follow_the_constants_they_read_even_in_a_cycle() {
        let module = lower_source(
            "define constant a = b + 1;\n\
             define constant b = a + 1;\n\
             define constant c = 3;\n\
             define function main()\n  a\nend;",
        );
        assert_eq!(module.diagnostics, Vec::new());
        let a = module.constants[0].definition;
        let b = module.constants[1].definition;
        let c = module.constants[2].definition;
        let main = module.functions[0].definition;
        let mut invalidation = graph(&module);

        let affected =
            invalidation.changed_with_cutoff(&[QueryArtifact::DefinitionSem(b)], |_| false);
        assert!(affected.contains(&QueryArtifact::Contract(a)));
        assert!(affected.contains(&QueryArtifact::DefinitionTyping(main)));
        assert!(!affected.contains(&QueryArtifact::Contract(c)));
    }

    #[test]
    fn codegen_policy_change_lowers_again_without_retyping() {
        let module = lower_source(
            "define generic area(x);\n\
             define method area(x :: <integer>)\n  x * x\nend;\n\
             define function main()\n  area(3)\nend;",
        );
        let method = module.methods[0].definition;
        let main = module.functions[0].definition;
        for policy in CodegenPolicy::ALL {
            let mut invalidation = graph_under(&module, *policy);
            let affected = invalidation.codegen_policy_changed();
            for lowered in [
                QueryArtifact::CodegenPolicy,
                QueryArtifact::MethodLowering(method),
                QueryArtifact::MethodTape(method),
                QueryArtifact::LoweredModule,
                QueryArtifact::TapeProgram,
                QueryArtifact::RuntimeResult,
            ] {
                assert!(affected.contains(&lowered), "{policy:?} {lowered:?}");
            }
            for untouched in [
                QueryArtifact::Typing,
                QueryArtifact::DefinitionTyping(main),
                QueryArtifact::DefinitionTyping(method),
                QueryArtifact::Diagnostics,
                QueryArtifact::Bind,
            ] {
                assert!(!affected.contains(&untouched), "{policy:?} {untouched:?}");
            }
        }
    }

    #[test]
    fn proved_lowering_follows_only_its_own_typing() {
        let module = lower_source(
            "define constant k = 2;\n\
             define generic area(x);\n\
             define method area(x :: <integer>)\n  x * k\nend;\n\
             define method area(x :: <double-float>)\n  x * x\nend;\n\
             define function main()\n  area(3)\nend;",
        );
        let [integer, float] = [module.methods[0].definition, module.methods[1].definition];
        let k = module.constants[0].definition;

        // Off lowering reads no decision, so a typing change lowers nothing.
        let mut off = graph_under(&module, CodegenPolicy::Off);
        let affected =
            off.changed_with_cutoff(&[QueryArtifact::DefinitionTyping(integer)], |_| false);
        assert!(!affected.contains(&QueryArtifact::MethodLowering(integer)));
        assert!(!affected.contains(&QueryArtifact::LoweredModule));

        // Proved lowering acts on the method's own decisions...
        let mut proved = graph_under(&module, CodegenPolicy::Proved);
        let affected =
            proved.changed_with_cutoff(&[QueryArtifact::DefinitionTyping(integer)], |_| false);
        assert!(affected.contains(&QueryArtifact::MethodLowering(integer)));
        assert!(affected.contains(&QueryArtifact::LoweredModule));
        assert!(!affected.contains(&QueryArtifact::MethodLowering(float)));

        // ...and on those of the constants it lowers inline.
        let affected = proved.changed_with_cutoff(&[QueryArtifact::DefinitionTyping(k)], |_| false);
        assert!(affected.contains(&QueryArtifact::MethodLowering(integer)));
        assert!(!affected.contains(&QueryArtifact::MethodLowering(float)));
    }

    #[test]
    fn proved_lowering_follows_constants_read_through_constants() {
        let module = lower_source(
            "define constant inner = 2 + 3;\n\
             define constant outer = inner;\n\
             define generic area(x);\n\
             define method area(x :: <integer>)\n  outer\nend;\n\
             define function main()\n  area(3)\nend;",
        );
        let method = module.methods[0].definition;
        let inner = module.constants[0].definition;
        let mut proved = graph_under(&module, CodegenPolicy::Proved);
        // `area`'s body cites only `outer`, but lowers `inner`'s value and
        // its checks inline too.
        let affected =
            proved.changed_with_cutoff(&[QueryArtifact::DefinitionTyping(inner)], |_| false);
        assert!(affected.contains(&QueryArtifact::MethodLowering(method)));
        let affected = proved.changed_with_cutoff(&[QueryArtifact::DefinitionSem(inner)], |_| true);
        assert!(affected.contains(&QueryArtifact::MethodLowering(method)));
    }

    #[test]
    fn typing_config_change_skips_definition_checks_and_lowering() {
        let module = lower_source("define function main()\n  1\nend;");
        let main = module.functions[0].definition;
        let mut invalidation = graph(&module);
        let affected = invalidation.typing_config_changed();

        assert_eq!(
            affected,
            [
                QueryArtifact::TypingConfig,
                QueryArtifact::Typing,
                QueryArtifact::Diagnostics
            ]
        );
        assert!(!affected.contains(&QueryArtifact::DefinitionTyping(main)));
        assert!(!affected.contains(&QueryArtifact::LoweredModule));
    }

    #[test]
    fn source_change_reaches_parse_bind_methods_and_tape() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius * c.radius\nend;",
        );
        let mut invalidation = graph(&module);
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
        let source_file = SourceFileId::from_path("examples/area.loko");
        let before = lower_file(
            source_file,
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius * c.radius\nend;",
        );
        let edited = lower_file(
            source_file,
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
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

        let before_graph = graph(&before);
        let edited_graph = graph(&edited);
        let before_key = before_graph.key_for(QueryArtifact::MethodLookup(circle_method));
        let edited_key =
            edited_graph.key_for(QueryArtifact::MethodLookup(edited_circle.definition));

        assert_eq!(before_key, edited_key);
    }
}
