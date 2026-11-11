//! Compiler session API and inspectable stage artifacts.
//!
//! The session layer owns source-file identity, per-stage artifact snapshots,
//! stable artifact-key reporting, dumps, and edit comparisons. It explicitly
//! does not own language semantics, lowering rules, runtime object storage, or
//! `execution_tape` bytecode internals.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::fmt::{self, Write as _};

use execution_tape::host::{Host, HostContext, HostError, SigHash, ValueRef};
use execution_tape::trace::TraceMask;
use execution_tape::value::{FuncRef, Value};
use execution_tape::vm::{Limits, Trap, TrapInfo, Vm};

use crate::diagnostic::{Diagnostic, DiagnosticCode};
use crate::execution_trace::TraceCapture;
pub use crate::execution_trace::{
    ExecutionTrace, ExecutionTraceDetail, ExecutionTraceView, SourcePosition,
    TRACE_INSTRUCTION_LIMIT, TracedFunction, TracedHostCall, TracedInstruction, TracedLocation,
    TracedOutcome, TracedStack,
};
use crate::grammar_frontend::{
    self, FrontendForm, FrontendFormKind, FrontendParseArtifact, GrammarActivation,
    GrammarDiagnosticFact, GrammarEnvironmentSelection, GrammarFrontendRegistry,
    GrammarParseReport, GrammarRule, GrammarRuleApplication, LanguageEnvironment,
};
use crate::identity::{
    ArtifactKey, DefinitionId, DefinitionKind, SourceFileId, StableIdBuilder, SyntaxNodeId,
};
use crate::incremental::{ArtifactEdge, ArtifactReport, QueryArtifact, QueryInvalidation};
use crate::runtime as runtime_bridge;
use crate::syntax::{CstElement, CstKind, CstNode, Span, SyntaxTree, TokenKind};
use crate::{ast, lower, sem, tape, typing};
/// Upstream execution evidence carried by [`ExecutionTrace`]: recorded
/// values, host and final outcomes, instruction outcomes and replay.
pub use execution_tape::recording;
use interloko_runtime::control::ControlFault;
use interloko_runtime::{
    self as runtime, HostObjectKind, InterlokoHost, RuntimeLayout, RuntimeLayoutError,
};
/// Runtime heap accounting reported by [`FunctionCallOutcome::turn`].
pub use interloko_runtime::{HeapStats, TurnStats};

/// Named source file accepted by a compiler session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceFile {
    /// Stable source file id.
    pub id: SourceFileId,
    /// Caller-facing source name or path.
    pub name: String,
    /// Source text.
    pub source: String,
    /// Grammar environment selected for this source.
    pub grammar_environment: GrammarEnvironmentSelection,
}

impl SourceFile {
    /// Creates a named source file using `name` as the stable identity input.
    #[must_use]
    pub fn named(name: &str, source: &str) -> Self {
        Self::with_id(SourceFileId::from_path(name), name, source)
    }

    /// Creates a named source file using `name` as the stable identity input
    /// and an explicit grammar environment.
    #[must_use]
    pub fn named_with_grammar(
        name: &str,
        source: &str,
        grammar_environment: GrammarEnvironmentSelection,
    ) -> Self {
        Self::with_id_and_grammar(
            SourceFileId::from_path(name),
            name,
            source,
            grammar_environment,
        )
    }

    /// Creates a named source file with an explicit id.
    #[must_use]
    pub fn with_id(id: SourceFileId, name: &str, source: &str) -> Self {
        Self::with_id_and_grammar(id, name, source, GrammarEnvironmentSelection::base_loko())
    }

    /// Creates a named source file with an explicit id and grammar environment.
    #[must_use]
    pub fn with_id_and_grammar(
        id: SourceFileId,
        name: &str,
        source: &str,
        grammar_environment: GrammarEnvironmentSelection,
    ) -> Self {
        Self {
            id,
            name: name.to_string(),
            source: source.to_string(),
            grammar_environment,
        }
    }
}

/// Runtime mode for a session compile.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RuntimeMode {
    /// Compile only.
    CompileOnly,
    /// Compile and run zero-argument `main` when possible.
    RunMain,
    /// Compile and run zero-argument `main` while recording an execution
    /// trace.
    RunMainTraced {
        /// Evidence retained by the recording.
        detail: ExecutionTraceDetail,
    },
}

/// Result of running `main` through the session API.
///
/// A successful run carries the same owned [`FunctionValue`] that
/// [`SessionArtifacts::call_function`] returns: `main` is executed through the
/// typed scalar call path, so every tool renders and compares results through
/// one value model.
#[derive(Clone, Debug)]
pub enum RuntimeResult {
    /// `main` returned one scalar value.
    Returned(FunctionValue),
    /// No `main` function was available.
    MissingMain,
    /// `main` has parameters.
    MainHasParameters,
    /// Runtime trapped.
    Trap {
        /// Trap details.
        trap: TrapInfo,
    },
    /// A declared host function could not be executed. Session-driven runs
    /// supply no host-function handler, so any declared host call fails here.
    HostFunctionFailed {
        /// Loko-visible host function name.
        name: String,
        /// Precise handler or value-conversion failure.
        failure: HostFunctionFailure,
    },
    /// `main` does not return exactly one scalar value that can leave the
    /// runtime.
    UnsupportedResult {
        /// Declared result categories.
        declared: String,
    },
    /// A signalled condition found no applicable live handler. Handler search
    /// runs before any frame unwinds, so no cleanup ran on its behalf.
    UnhandledCondition {
        /// Source name of the condition's class.
        class: String,
    },
}

impl RuntimeResult {
    /// Stable code for non-successful runtime/session outcomes.
    #[must_use]
    pub const fn code(&self) -> Option<DiagnosticCode> {
        match self {
            Self::Returned(_) => None,
            Self::MissingMain => Some(DiagnosticCode::RuntimeMissingMain),
            Self::MainHasParameters => Some(DiagnosticCode::RuntimeMainHasParameters),
            Self::Trap { trap } => Some(trap_code(trap)),
            Self::HostFunctionFailed { .. } => Some(DiagnosticCode::RuntimeHostFunctionFailed),
            Self::UnsupportedResult { .. } => Some(DiagnosticCode::RuntimeUnsupportedResult),
            Self::UnhandledCondition { .. } => Some(DiagnosticCode::RuntimeUnhandledCondition),
        }
    }

    /// Returns the scalar value returned by `main`, when the run succeeded.
    #[must_use]
    pub const fn value(&self) -> Option<&FunctionValue> {
        match self {
            Self::Returned(value) => Some(value),
            _ => None,
        }
    }
}

impl fmt::Display for RuntimeResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Returned(value) => write!(formatter, "returned {value}"),
            Self::MissingMain => formatter.write_str("missing main"),
            Self::MainHasParameters => formatter.write_str("main has parameters"),
            Self::Trap { trap } => fmt_trap(formatter, trap),
            Self::HostFunctionFailed { name, failure } => {
                write!(formatter, "host function `{name}` failed: {failure}")
            }
            Self::UnsupportedResult { declared } => {
                write!(
                    formatter,
                    "main result {declared} is not one supported scalar"
                )
            }
            Self::UnhandledCondition { class } => {
                write!(formatter, "unhandled condition {class}")
            }
        }
    }
}

/// Returns the Loko condition an explicit tape trap signals, if any.
///
/// Traps raised by the VM itself (resource limits, failed host calls, ...)
/// are not Loko conditions and yield `None`.
#[must_use]
pub const fn loko_trap(trap: &TrapInfo) -> Option<lower::LokoTrap> {
    match trap.trap {
        Trap::TrapCode(code) => lower::LokoTrap::from_code(code),
        _ => None,
    }
}

/// Stable diagnostic code for a runtime trap: the Loko condition's own code
/// when it has one, otherwise [`DiagnosticCode::RuntimeTrap`].
const fn trap_code(trap: &TrapInfo) -> DiagnosticCode {
    match loko_trap(trap) {
        Some(condition) => condition.diagnostic_code(),
        None => DiagnosticCode::RuntimeTrap,
    }
}

fn fmt_trap(formatter: &mut fmt::Formatter<'_>, trap: &TrapInfo) -> fmt::Result {
    match loko_trap(trap) {
        Some(condition) => write!(
            formatter,
            "runtime trap: {} ({trap})",
            condition.description()
        ),
        None => write!(formatter, "runtime trap: {trap}"),
    }
}

/// Owned scalar value passed to or returned by an Interloko direct function.
///
/// `FunctionValue` is `PartialEq` but deliberately not `Eq`: a
/// [`FunctionValue::Float`] compares with IEEE 754 equality, so a NaN result
/// is not equal to itself and `0.0 == -0.0`. Callers that need identity of the
/// returned bits compare [`f64::to_bits`] or the rendered literal text.
#[derive(Clone, Debug, PartialEq)]
pub enum FunctionValue {
    /// Signed integer value.
    Integer(i64),
    /// IEEE 754 binary64 float value.
    Float(f64),
    /// Boolean value.
    Boolean(bool),
    /// Runtime string value copied out of the execution host.
    String(String),
}

impl fmt::Display for FunctionValue {
    /// Renders the value as Loko literal text: integers in decimal, floats
    /// as their shortest round-trip literal (`1.0`, `2.5e-7`; `inf`, `-inf`,
    /// and `nan` for non-finite values), booleans as `#t`/`#f`, and strings
    /// quoted with Loko escapes.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Integer(value) => write!(formatter, "{value}"),
            Self::Float(value) => write!(formatter, "{}", sem::FloatBits::from_f64(*value)),
            Self::Boolean(true) => formatter.write_str("#t"),
            Self::Boolean(false) => formatter.write_str("#f"),
            Self::String(value) => {
                formatter.write_char('"')?;
                for ch in value.chars() {
                    match ch {
                        '"' => formatter.write_str("\\\"")?,
                        '\\' => formatter.write_str("\\\\")?,
                        '\n' => formatter.write_str("\\n")?,
                        '\t' => formatter.write_str("\\t")?,
                        ch => formatter.write_char(ch)?,
                    }
                }
                formatter.write_char('"')
            }
        }
    }
}

impl FunctionValue {
    /// Returns the scalar value's machine category.
    #[must_use]
    pub const fn kind(&self) -> FunctionValueKind {
        match self {
            Self::Integer(_) => FunctionValueKind::Integer,
            Self::Float(_) => FunctionValueKind::Float,
            Self::Boolean(_) => FunctionValueKind::Boolean,
            Self::String(_) => FunctionValueKind::String,
        }
    }
}

/// Scalar category accepted by the calm function-call API.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FunctionValueKind {
    /// Signed integer value.
    Integer,
    /// IEEE 754 binary64 float value.
    Float,
    /// Boolean value.
    Boolean,
    /// Runtime string value.
    String,
}

impl fmt::Display for FunctionValueKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Integer => formatter.write_str("integer"),
            Self::Float => formatter.write_str("float"),
            Self::Boolean => formatter.write_str("boolean"),
            Self::String => formatter.write_str("string"),
        }
    }
}

/// Successful scalar function call and its optional execution trace.
#[derive(Clone, Debug)]
pub struct FunctionCallOutcome {
    /// Scalar value returned by the function.
    pub value: FunctionValue,
    /// Recorded trace when tracing was requested.
    pub trace: Option<ExecutionTrace>,
    /// Heap accounting for the call, which is one runtime turn: what it
    /// allocated and what is still reachable from its result at the end.
    pub turn: TurnStats,
}

/// A runtime image that outlives calls.
///
/// [`SessionArtifacts::call_function`] runs each call on a fresh runtime
/// host. A `SessionRuntime` instead keeps one host, so long-lived runtime
/// state (interned symbols today) persists, and runs each call as one
/// *turn*: the call allocates into a nursery that is emptied when it
/// returns or traps, so repeated calls do not grow the heap.
pub struct SessionRuntime {
    host: InterlokoHost,
}

impl SessionRuntime {
    /// Runs `function_name` from `artifacts` as one turn of this runtime.
    ///
    /// `artifacts` must have the runtime layout the image was created with;
    /// otherwise the call fails with
    /// [`FunctionCallError::RuntimeLayoutChanged`] and the image is
    /// untouched.
    pub fn call_function(
        &mut self,
        artifacts: &SessionArtifacts,
        function_name: &str,
        arguments: &[FunctionValue],
        trace: Option<ExecutionTraceDetail>,
    ) -> Result<FunctionCallOutcome, FunctionCallError> {
        let tape = artifacts
            .tape
            .as_ref()
            .ok_or(FunctionCallError::TapeUnavailable)?;
        if *self.host.layout() != tape.runtime {
            return Err(FunctionCallError::RuntimeLayoutChanged);
        }
        call_function(
            &artifacts.sem,
            &artifacts.host_registry,
            tape,
            &mut self.host,
            function_name,
            arguments,
            trace,
            None,
        )
    }

    /// Returns the sizes of the whole runtime heap. Between turns the
    /// nursery is empty, so this is the image.
    #[must_use]
    pub fn heap_stats(&self) -> HeapStats {
        self.host.heap_stats()
    }
}

/// Embedding callback for scalar functions declared through
/// [`sem::HostMetaRegistry`].
///
/// The compiler/runtime adapter owns conversion between VM values and
/// [`FunctionValue`]. Implementors receive only owned scalar values and cannot
/// access the Interloko heap or VM directly.
pub trait HostFunctionHandler {
    /// Executes one declared host function.
    fn call(
        &mut self,
        function: &sem::HostFunction,
        arguments: &[FunctionValue],
    ) -> Result<FunctionValue, HostFunctionFailure>;
}

/// A host-function handler's bounded, caller-facing rejection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostFunctionFailure {
    message: String,
}

impl HostFunctionFailure {
    /// Creates a failure with a human-readable explanation.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the handler-provided explanation.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for HostFunctionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

/// Failure to prepare or execute a scalar Interloko direct function call.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum FunctionCallError {
    /// The artifacts do not contain an emitted tape program.
    TapeUnavailable,
    /// The requested function was not present in the semantic artifacts.
    MissingFunction {
        /// Requested function name.
        name: String,
    },
    /// Supplied argument count differs from the function signature.
    ArgumentCountMismatch {
        /// Requested function name.
        name: String,
        /// Declared parameter count.
        expected: usize,
        /// Supplied argument count.
        actual: usize,
    },
    /// One scalar argument cannot satisfy the declared parameter category.
    ArgumentTypeMismatch {
        /// Requested function name.
        name: String,
        /// Zero-based argument position.
        index: usize,
        /// Declared semantic category.
        expected: String,
        /// Supplied scalar category.
        actual: FunctionValueKind,
    },
    /// A valid scalar argument could not be represented in the runtime host.
    ArgumentMaterializationFailed {
        /// Zero-based argument position.
        index: usize,
        /// Supplied scalar category.
        kind: FunctionValueKind,
    },
    /// Runtime trapped during verified execution.
    Trap {
        /// Trap details.
        trap: alloc::boxed::Box<TrapInfo>,
        /// Partial trace captured before the trap, when requested.
        trace: Option<ExecutionTrace>,
    },
    /// A declared host function could not be executed by the embedding
    /// handler.
    HostFunctionFailed {
        /// Loko-visible host function name.
        name: String,
        /// Precise handler or value-conversion failure.
        failure: HostFunctionFailure,
        /// Partial trace captured before the failed host call, when requested.
        trace: Option<ExecutionTrace>,
    },
    /// The function does not return exactly one supported scalar value.
    UnsupportedResult {
        /// Requested function name.
        name: String,
        /// Declared result categories.
        declared: String,
    },
    /// A declared string result could not be recovered from the runtime host.
    ResultMaterializationFailed {
        /// Requested function name.
        name: String,
    },
    /// The runtime could not open or close the call's turn.
    TurnFailed {
        /// Underlying runtime error.
        error: runtime::RuntimeError,
    },
    /// A [`SessionRuntime`] was asked to run artifacts whose runtime layout
    /// differs from the one its image was built for.
    RuntimeLayoutChanged,
    /// A signalled condition found no applicable live handler.
    UnhandledCondition {
        /// Source name of the condition's class.
        class: String,
        /// Partial trace captured up to the signal, when requested.
        trace: Option<ExecutionTrace>,
    },
}

impl fmt::Display for FunctionCallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TapeUnavailable => formatter.write_str("tape unavailable"),
            Self::MissingFunction { name } => write!(formatter, "missing function `{name}`"),
            Self::ArgumentCountMismatch {
                name,
                expected,
                actual,
            } => write!(
                formatter,
                "function `{name}` expects {expected} argument(s), received {actual}"
            ),
            Self::ArgumentTypeMismatch {
                name,
                index,
                expected,
                actual,
            } => write!(
                formatter,
                "function `{name}` argument {index} expects {expected}, received {actual}"
            ),
            Self::ArgumentMaterializationFailed { index, kind } => write!(
                formatter,
                "function argument {index} of kind {kind} could not enter the runtime"
            ),
            Self::Trap { trap, .. } => fmt_trap(formatter, trap),
            Self::HostFunctionFailed { name, failure, .. } => {
                write!(formatter, "host function `{name}` failed: {failure}")
            }
            Self::UnsupportedResult { name, declared } => write!(
                formatter,
                "function `{name}` result {declared} is not one supported scalar"
            ),
            Self::ResultMaterializationFailed { name } => write!(
                formatter,
                "function `{name}` returned a runtime value that could not be inspected"
            ),
            Self::TurnFailed { error } => write!(formatter, "runtime turn failed: {error:?}"),
            Self::RuntimeLayoutChanged => {
                formatter.write_str("the runtime image was built for a different runtime layout")
            }
            Self::UnhandledCondition { class, .. } => {
                write!(formatter, "unhandled condition {class}")
            }
        }
    }
}

impl core::error::Error for FunctionCallError {}

/// Stage that can be rendered as a text dump.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DumpStage {
    /// CST/syntax dump.
    Syntax,
    /// AST dump.
    Ast,
    /// Semantic IR dump.
    Sem,
    /// Metaobject protocol summary.
    Mop,
    /// Environment object protocol summary.
    Eop,
    /// Lowered executable IR dump.
    Lowered,
    /// `execution_tape` disassembly.
    Tape,
    /// Soft typing configuration and decisions.
    Typing,
    /// All stage dumps.
    All,
}

/// Stable identifier for an environment object projected by the session layer.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct EnvironmentObjectId(String);

impl EnvironmentObjectId {
    /// Creates an environment object id from a stable text form.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the stable text form of this id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EnvironmentObjectId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Environment-level kind for a projected object.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EnvironmentKind {
    /// A named source file known to the session.
    SourceFile,
    /// A session-owned grammar frontend registry.
    GrammarRegistry,
    /// A language environment registered with the grammar frontend registry.
    RegisteredGrammarEnvironment,
    /// A staged grammar adapter registered with the grammar frontend registry.
    StagedGrammarAdapter,
    /// A registered staged grammar extension descriptor.
    GrammarExtension,
    /// A concrete staged grammar extension activation.
    GrammarExtensionActivation,
    /// A frontend form emitted by a staged grammar extension.
    GrammarExtensionEmission,
    /// A provenance rule charged to a staged grammar extension.
    GrammarExtensionProvenance,
    /// A language environment metaobject.
    LanguageEnvironment,
    /// A grammar metaobject.
    Grammar,
    /// A source-authored grammar package.
    GrammarPackage,
    /// A source-authored grammar resource file.
    GrammarResource,
    /// A direct grammar rule metaobject.
    GrammarRule,
    /// A deterministic action attached to a grammar rule.
    GrammarAction,
    /// An effective grammar rule after grammar-stack resolution.
    EffectiveGrammarRule,
    /// A grammar activation over a parse region.
    GrammarActivation,
    /// A staged source region parsed by a registered grammar adapter.
    StagedRegion,
    /// A semantic class metaobject.
    Class,
    /// A semantic slot metaobject.
    Slot,
    /// A semantic accessor metaobject.
    Accessor,
    /// A semantic generic function metaobject.
    Generic,
    /// A semantic method metaobject.
    Method,
    /// A direct function definition.
    Function,
    /// An immutable top-level constant definition.
    Constant,
    /// A semantic generic dispatch-site object.
    DispatchSite,
    /// A compiler diagnostic.
    Diagnostic,
    /// A runtime layout row.
    RuntimeLayoutRow,
    /// A query artifact.
    Artifact,
    /// A scalar function declared by the embedding host.
    HostFunction,
    /// An explicitly projected host-owned environment object.
    HostObject,
    /// One entry in the session's in-memory mutation log.
    Mutation,
    /// The session's soft typing configuration.
    TypingConfig,
    /// One soft typing decision about a residual runtime check.
    TypingDecision,
}

impl EnvironmentKind {
    /// Returns the stable dump label for this environment kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceFile => "source-file",
            Self::GrammarRegistry => "grammar-registry",
            Self::RegisteredGrammarEnvironment => "registered-grammar-environment",
            Self::StagedGrammarAdapter => "staged-grammar-adapter",
            Self::GrammarExtension => "grammar-extension",
            Self::GrammarExtensionActivation => "grammar-extension-activation",
            Self::GrammarExtensionEmission => "grammar-extension-emission",
            Self::GrammarExtensionProvenance => "grammar-extension-provenance",
            Self::LanguageEnvironment => "language-environment",
            Self::Grammar => "grammar",
            Self::GrammarPackage => "grammar-package",
            Self::GrammarResource => "grammar-resource",
            Self::GrammarRule => "grammar-rule",
            Self::GrammarAction => "grammar-action",
            Self::EffectiveGrammarRule => "effective-grammar-rule",
            Self::GrammarActivation => "grammar-activation",
            Self::StagedRegion => "staged-region",
            Self::Class => "class",
            Self::Slot => "slot",
            Self::Accessor => "accessor",
            Self::Generic => "generic",
            Self::Method => "method",
            Self::Function => "function",
            Self::Constant => "constant",
            Self::DispatchSite => "dispatch-site",
            Self::Diagnostic => "diagnostic",
            Self::RuntimeLayoutRow => "runtime-layout-row",
            Self::Artifact => "artifact",
            Self::HostFunction => "host-function",
            Self::HostObject => "host-object",
            Self::Mutation => "mutation",
            Self::TypingConfig => "typing-config",
            Self::TypingDecision => "typing-decision",
        }
    }
}

/// Origin of a projected environment object.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EnvironmentOrigin {
    /// The object is authored by Loko source.
    SourceAuthored,
    /// The object is declared by explicit host-backed metadata.
    HostBacked,
    /// The object is generated by compiler/session work.
    CompilerGenerated,
    /// The object is contributed by an explicit host projection adapter.
    HostProvided,
    /// The object was introduced by an in-memory session mutation.
    Session,
}

impl EnvironmentOrigin {
    /// Returns the stable dump label for this origin.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceAuthored => "source-authored",
            Self::HostBacked => "host-backed",
            Self::CompilerGenerated => "compiler-generated",
            Self::HostProvided => "host-provided",
            Self::Session => "session",
        }
    }
}

/// Storage ownership for a projected environment object.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EnvironmentStorage {
    /// The object is represented by Loko source definitions.
    SourceDefined,
    /// The object is represented by explicit host registry metadata.
    HostRegistry,
    /// The object is represented by an adapter-owned host slot bridge.
    HostAdapter,
    /// The object is represented by the grammar frontend registry.
    FrontendRegistry,
    /// The object is represented by cached compiler/session artifacts.
    CachedArtifact,
    /// The object is represented by host-owned state outside Interloko.
    HostOwned,
    /// The object lives only in the session's in-memory mutation log; it is
    /// not persisted, serialized, or carried across session boundaries.
    TransientInMemory,
}

impl EnvironmentStorage {
    /// Returns the stable dump label for this storage owner.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceDefined => "source-defined",
            Self::HostRegistry => "host-registry",
            Self::HostAdapter => "host-adapter",
            Self::FrontendRegistry => "frontend-registry",
            Self::CachedArtifact => "cached-artifact",
            Self::HostOwned => "host-owned",
            Self::TransientInMemory => "transient-in-memory",
        }
    }
}

/// Stable workbench affordance names for environment objects.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EnvironmentAffordance {
    /// Inspect this object.
    Inspect,
    /// Dump the CST/syntax stage.
    DumpSyntax,
    /// Dump the AST stage.
    DumpAst,
    /// Dump semantic IR.
    DumpSem,
    /// Dump MOP facts.
    DumpMop,
    /// Dump EOP facts.
    DumpEop,
    /// Dump lowered executable IR.
    DumpLowered,
    /// Dump `execution_tape` disassembly.
    DumpTape,
    /// Show generic dispatch information.
    ShowDispatch,
    /// Show runtime layout information.
    ShowRuntimeLayout,
    /// Show query artifact dependencies.
    ShowDependencies,
    /// Show documentation comments attached to an object.
    ShowDocumentation,
    /// Show the accessor for a slot.
    ShowAccessor,
    /// Show the slot read by an accessor.
    ShowSlot,
    /// Run executable code.
    Run,
    /// Edit a function or method body.
    EditBody,
    /// Add a method to a generic function.
    AddMethod,
    /// Show artifact reports.
    Artifacts,
    /// Diff two source versions.
    Diff,
    /// Open the source location for an object.
    OpenSource,
    /// Dump query artifacts.
    DumpArtifacts,
    /// Show the slots owned by a class.
    ShowSlots,
    /// Show the resolved grammar stack.
    ShowGrammarStack,
    /// Show grammars owned by a language environment or related to a grammar.
    ShowGrammars,
    /// Show direct rules projected by a grammar.
    ShowDirectRules,
    /// Show effective rules resolved for a grammar.
    ShowEffectiveRules,
    /// Show grammar activations for an environment or grammar.
    ShowActivations,
    /// Show a projected grammar rule.
    ShowGrammarRule,
    /// Show the structured expression for a grammar rule.
    ShowExpression,
    /// Show the deterministic action attached to a grammar rule.
    ShowAction,
    /// Show the captures consumed by a grammar action.
    ShowCaptures,
    /// Show the output shape produced by a grammar action.
    ShowActionOutput,
    /// Show parse provenance and grammar activations.
    ShowParseTrace,
    /// Show the primary/oracle parser shadow report.
    ShowParserShadow,
    /// Show registered language environments and staged grammar adapters.
    ShowGrammarRegistry,
    /// Show staged grammar extension descriptors and concrete emissions.
    ShowGrammarExtension,
    /// Show soft typing policy or one typing decision with its provenance.
    ShowTyping,
}

impl EnvironmentAffordance {
    /// Returns every stable environment affordance in canonical order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Inspect,
            Self::DumpSyntax,
            Self::DumpAst,
            Self::DumpSem,
            Self::DumpMop,
            Self::DumpEop,
            Self::DumpLowered,
            Self::DumpTape,
            Self::ShowDispatch,
            Self::ShowRuntimeLayout,
            Self::ShowDependencies,
            Self::ShowDocumentation,
            Self::ShowAccessor,
            Self::ShowSlot,
            Self::Run,
            Self::EditBody,
            Self::AddMethod,
            Self::Artifacts,
            Self::Diff,
            Self::OpenSource,
            Self::DumpArtifacts,
            Self::ShowSlots,
            Self::ShowGrammarStack,
            Self::ShowGrammars,
            Self::ShowDirectRules,
            Self::ShowEffectiveRules,
            Self::ShowActivations,
            Self::ShowGrammarRule,
            Self::ShowExpression,
            Self::ShowAction,
            Self::ShowCaptures,
            Self::ShowActionOutput,
            Self::ShowParseTrace,
            Self::ShowParserShadow,
            Self::ShowGrammarRegistry,
            Self::ShowGrammarExtension,
            Self::ShowTyping,
        ]
    }

    /// Parses a stable environment affordance name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::all()
            .iter()
            .copied()
            .find(|affordance| affordance.as_str() == name)
    }

    /// Returns the stable affordance name used in EOP dumps.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inspect => "inspect",
            Self::DumpSyntax => "dump-syntax",
            Self::DumpAst => "dump-ast",
            Self::DumpSem => "dump-sem",
            Self::DumpMop => "dump-mop",
            Self::DumpEop => "dump-eop",
            Self::DumpLowered => "dump-lowered",
            Self::DumpTape => "dump-tape",
            Self::ShowDispatch => "show-dispatch",
            Self::ShowRuntimeLayout => "show-runtime-layout",
            Self::ShowDependencies => "show-dependencies",
            Self::ShowDocumentation => "show-documentation",
            Self::ShowAccessor => "show-accessor",
            Self::ShowSlot => "show-slot",
            Self::Run => "run",
            Self::EditBody => "edit-body",
            Self::AddMethod => "add-method",
            Self::Artifacts => "artifacts",
            Self::Diff => "diff",
            Self::OpenSource => "open-source",
            Self::DumpArtifacts => "dump-artifacts",
            Self::ShowSlots => "show-slots",
            Self::ShowGrammarStack => "show-grammar-stack",
            Self::ShowGrammars => "show-grammars",
            Self::ShowDirectRules => "show-direct-rules",
            Self::ShowEffectiveRules => "show-effective-rules",
            Self::ShowActivations => "show-activations",
            Self::ShowGrammarRule => "show-grammar-rule",
            Self::ShowExpression => "show-expression",
            Self::ShowAction => "show-action",
            Self::ShowCaptures => "show-captures",
            Self::ShowActionOutput => "show-action-output",
            Self::ShowParseTrace => "show-parse-trace",
            Self::ShowParserShadow => "show-parser-shadow",
            Self::ShowGrammarRegistry => "show-grammar-registry",
            Self::ShowGrammarExtension => "show-grammar-extension",
            Self::ShowTyping => "show-typing",
        }
    }
}

/// Presentation hints for an environment object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PresentationFacet {
    /// Human-facing stable title.
    pub title: String,
}

impl PresentationFacet {
    /// Creates a presentation facet with a stable title.
    #[must_use]
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
        }
    }
}

/// Source provenance carried by an environment object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceFacet {
    /// Source file that owns the source span, when the object is source-backed.
    pub source_file: Option<SourceFileId>,
    /// Stable source definition identity, when the object corresponds to one
    /// definition.
    pub definition: Option<DefinitionId>,
    /// Source span for this object, when the projection can identify one
    /// directly.
    pub span: Option<Span>,
}

impl SourceFacet {
    /// Creates a source facet.
    #[must_use]
    pub const fn new(
        source_file: Option<SourceFileId>,
        definition: Option<DefinitionId>,
        span: Option<Span>,
    ) -> Self {
        Self {
            source_file,
            definition,
            span,
        }
    }

    /// Creates an empty source facet for objects without source provenance.
    ///
    /// This is intentional for host-owned, runtime-only, and session-transient
    /// objects whose identity is real but not tied to a source span.
    #[must_use]
    pub const fn none() -> Self {
        Self::new(None, None, None)
    }
}

/// Documentation comments attached to an environment object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentationFacet {
    /// Normalized leading `///` comment lines, without the marker.
    pub lines: Vec<String>,
}

impl DocumentationFacet {
    /// Creates a documentation facet from normalized lines.
    #[must_use]
    pub fn new(lines: Vec<String>) -> Self {
        Self { lines }
    }
}

/// Query artifacts related to an environment object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactFacet {
    /// Stable artifact keys related to this object.
    pub keys: Vec<ArtifactKey>,
}

impl ArtifactFacet {
    /// Creates an artifact facet.
    #[must_use]
    pub fn new(keys: Vec<ArtifactKey>) -> Self {
        Self { keys }
    }
}

/// Action names exposed by an environment object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AffordanceFacet {
    /// Stable affordance names in presentation order.
    pub names: Vec<EnvironmentAffordance>,
}

impl AffordanceFacet {
    /// Creates an affordance facet.
    #[must_use]
    pub fn new(names: Vec<EnvironmentAffordance>) -> Self {
        Self { names }
    }

    /// Returns true when this facet exposes `name`.
    #[must_use]
    pub fn contains(&self, name: EnvironmentAffordance) -> bool {
        self.names.contains(&name)
    }
}

/// Executable behavior summary for an environment object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionFacet {
    /// Stable detail text rendered after `execution`.
    pub detail: String,
}

impl ExecutionFacet {
    /// Creates an execution facet.
    #[must_use]
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

/// Inspection summary for an environment object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InspectionFacet {
    /// Stable detail text rendered after `inspection`.
    pub detail: String,
}

impl InspectionFacet {
    /// Creates an inspection facet.
    #[must_use]
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

/// Typed soft typing data carried by typing environment objects.
///
/// Consumers read these values directly instead of parsing inspection text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TypingFacet {
    /// The session's typing configuration and pass status.
    Config {
        /// Reporting policy in effect.
        config: typing::TypingConfig,
        /// Whether the pass ran.
        status: typing::TypingStatus,
    },
    /// One classified runtime check with the severity its policy assigns.
    Decision {
        /// The decision, keyed by owner definition and index.
        decision: typing::TypingDecision,
        /// Severity the configuration assigns to the decision.
        severity: typing::TypingSeverity,
    },
}

/// Typed EOP record projected by the session or an explicit host adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvironmentObject {
    /// Stable environment object id.
    pub id: EnvironmentObjectId,
    /// Environment-level object kind.
    pub kind: EnvironmentKind,
    /// Meta-level class label for this environment object.
    pub meta_class: String,
    /// Where this object comes from.
    pub origin: EnvironmentOrigin,
    /// Who owns the object's current representation.
    pub storage: EnvironmentStorage,
    /// Presentation hints.
    pub presentation: PresentationFacet,
    /// Source provenance. Some projected objects intentionally carry an empty
    /// facet when they are host-owned, runtime-only, or session-transient.
    pub source: SourceFacet,
    /// Documentation comments associated with this object.
    pub documentation: DocumentationFacet,
    /// Related compiler artifacts.
    pub artifacts: ArtifactFacet,
    /// Supported environment actions.
    pub affordances: AffordanceFacet,
    /// Executable behavior summary, when applicable.
    pub execution: Option<ExecutionFacet>,
    /// Inspection details.
    pub inspection: InspectionFacet,
    /// Soft typing data for typing objects; `None` for every other kind.
    pub typing: Option<TypingFacet>,
}

/// Sink for structured environment object projection.
pub trait EnvironmentObjectSink {
    /// Emits one environment object.
    fn emit(&mut self, object: EnvironmentObject);
}

impl EnvironmentObjectSink for Vec<EnvironmentObject> {
    fn emit(&mut self, object: EnvironmentObject) {
        self.push(object);
    }
}

/// Explicit host-owned environment projection seam.
///
/// Implementors contribute host-owned objects into an EOP sink. This is
/// adapter-owned projection, not automatic runtime heap enumeration.
pub trait HostEnvironmentProvider {
    /// Projects host-owned environment objects into `sink`.
    fn project_environment(&self, sink: &mut dyn EnvironmentObjectSink);
}

/// Fully inspected compilation artifacts for one source file.
#[derive(Clone, Debug)]
pub struct SessionArtifacts {
    /// Source file.
    pub source: SourceFile,
    /// Grammar frontend registry used for this compilation.
    pub grammar_registry: GrammarFrontendRegistry,
    /// Host-backed metaobject registry used for this compilation.
    pub host_registry: sem::HostMetaRegistry,
    /// Parsed syntax tree.
    pub syntax: SyntaxTree,
    /// Grammar frontend parse report.
    pub grammar_parse: GrammarParseReport,
    /// Surface AST.
    pub ast: ast::Module,
    /// Semantic IR.
    pub sem: sem::Module,
    /// Lowered executable IR, when lowering succeeded.
    pub lowered: Option<lower::Module>,
    /// Lowering error, when lowering failed.
    pub lower_error: Option<lower::LowerError>,
    /// Verified tape module, when emission succeeded.
    pub tape: Option<tape::TapeModule>,
    /// Tape emission error, when emission failed.
    pub tape_error: Option<tape::TapeError>,
    /// Aggregated diagnostics from the compiler pipeline, including advisory
    /// soft typing warnings.
    pub diagnostics: Vec<Diagnostic>,
    /// Soft typing decisions and the configuration they were reported under.
    /// Advisory: lowering, tape emission, and the runtime do not consult it.
    pub typing: typing::TypingArtifact,
    /// Runtime result when [`RuntimeMode::RunMain`] or tracing was requested.
    pub runtime_result: Option<RuntimeResult>,
    /// Captured runtime trace when traced execution was requested.
    pub runtime_trace: Option<ExecutionTrace>,
    /// Live EOP mutation log in application order. Read through
    /// [`SessionArtifacts::mutations`]; the field stays private so the
    /// invariant "log drives provenance" is enforced at the type level.
    mutations: Vec<AppliedMutation>,
    /// Pristine source text fixed at first compile. Preserved byte-for-byte
    /// across every mutation rebuild so the session can recompose
    /// `source.source` from `pristine_source + mutations` on demand
    /// (rewind uses this; the existing `apply_mutation` path uses it too,
    /// instead of appending in place to `source.source`).
    pristine_source: String,
    /// Stable id counter for the next session event. Incremented on every
    /// successful `apply_mutation` and preserved (not reset) across every
    /// `rewind_to`. Together with the log, this guarantees mutation ids are
    /// never reused: a dropped entry's id is permanently retired.
    next_mutation_id: u32,
    invalidation: QueryInvalidation,
    artifact_metrics: Vec<ArtifactMetricEntry>,
}

/// A structured EOP mutation request applied to a [`SessionArtifacts`].
///
/// This slice supports only `AddMethod`. Future slices may extend the enum
/// with `RetractMethod`, `EditBody`, `DefineClass`, and others; each new
/// variant should compose cleanly with the existing affordance surface.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionMutation {
    /// Add a new method to an existing generic. `method_source` is a full
    /// Loko `define method ...; end;` clause; it is parsed immediately
    /// through the ordinary parser and the resulting AST method is appended
    /// to the session's AST before a full recompute. The named generic in
    /// the source must match `generic` after resolution.
    AddMethod {
        /// Target generic the new method specializes.
        generic: sem::GenericFunctionId,
        /// Loko `define method ...; end;` source text for the inserted method.
        method_source: String,
    },
}

/// Successful outcome of [`SessionArtifacts::apply_mutation`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MutationOutcome {
    /// Per-session monotonic mutation id assigned when this mutation is
    /// applied. Matches the `sequence` carried by the inserted method's
    /// [`sem::MethodOrigin::Session`].
    pub mutation_id: u32,
    /// Semantic method id of the inserted method in the post-mutation module.
    /// In this slice the runtime layout's method id is the same value because
    /// the layout is derived 1:1 from sem method ordering; a future slice
    /// that introduces an asymmetry can extend this struct without breaking
    /// the current contract.
    pub inserted_method: sem::MethodId,
    /// Query artifacts that the dependency graph identifies as touched by
    /// this mutation. This is precise impact reporting, not a record of
    /// what was reused from cache; per-method artifacts for unrelated
    /// methods are deliberately absent.
    pub affected_artifacts: Vec<QueryArtifact>,
    /// How the candidate was produced and applied. This slice always reports
    /// [`RecomputeStrategy::FullCandidateRebuild`]; a future slice that
    /// actually skips work through the incremental query cache will
    /// introduce additional variants.
    pub recompute_strategy: RecomputeStrategy,
}

/// One successful mutation as recorded in the session's mutation log.
///
/// The log is the single source of truth for session-mutation provenance.
/// Entries appear in application order. An entry's `mutation_id` equals the
/// `sequence` carried by the inserted method's
/// [`sem::MethodOrigin::Session`]; it is *not* in general equal to the
/// entry's position in the log. Mutation ids are stable session event ids,
/// so a rewind that drops a tail entry leaves a permanent gap in the
/// numbering of subsequent entries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppliedMutation {
    /// Per-session monotonic mutation id assigned when this mutation was
    /// applied. Matches `outcome.mutation_id`; surfaced on the wrapper so
    /// callers can identify the entry without descending into the outcome.
    pub mutation_id: u32,
    /// Original [`SessionMutation`] request that produced this entry.
    pub request: SessionMutation,
    /// Outcome returned by [`SessionArtifacts::apply_mutation`] for this
    /// entry.
    pub outcome: MutationOutcome,
}

/// How the post-mutation artifacts were produced.
///
/// The field on [`MutationOutcome`] reports the strategy honestly. CLI and
/// EOP wording should not surface cache-reuse claims until a variant that
/// actually delivers reuse exists.
#[non_exhaustive]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RecomputeStrategy {
    /// The candidate was built by recompiling the appended source from
    /// scratch and atomically swapped in. No cached query results were
    /// reused.
    FullCandidateRebuild,
}

/// Structured failure returned by [`SessionArtifacts::apply_mutation`].
///
/// On every variant the session is left in its pre-mutation state with no
/// partial updates visible. Construction is via candidate-and-swap: a
/// candidate set of artifacts is built and validated before any field on
/// the live session is replaced.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum MutationError {
    /// The mutation source did not parse, or its surface AST has structural
    /// errors. Carries the diagnostics added by the candidate compile.
    ParseFailed {
        /// New diagnostics introduced by the candidate compile.
        diagnostics: Vec<Diagnostic>,
    },
    /// The mutation source parsed but no new method definition appeared in
    /// the candidate AST. The most common cause is a `method_source` that
    /// is not a `define method` clause.
    NotAMethodDefinition,
    /// The target generic id is not present in the current sem module.
    UnknownGeneric {
        /// Generic id supplied by the caller.
        generic: sem::GenericFunctionId,
    },
    /// The parsed method names a different generic than the target id.
    GenericNameMismatch {
        /// Target generic id supplied by the caller.
        generic: sem::GenericFunctionId,
        /// Generic name resolved from the target id.
        expected: String,
        /// Generic name parsed from `method_source`.
        actual: String,
    },
    /// Semantic resolution rejected the candidate module. Carries the new
    /// diagnostics that fired between the pre-mutation and candidate compiles.
    SemanticFailed {
        /// New diagnostics introduced by the candidate compile.
        diagnostics: Vec<Diagnostic>,
    },
    /// The candidate method's specializer vector exactly matches an existing
    /// method on the same generic. The lower runtime layer would silently
    /// accept this and produce ambiguous dispatch; the affordance rejects it
    /// instead. Lower-layer ambiguity remains structurally reachable via
    /// direct [`RuntimeLayout::add_method`].
    DuplicateSpecializer {
        /// Existing method whose specializers the candidate duplicates.
        existing_method: sem::MethodId,
        /// Specializer vector shared by both methods.
        specializers: Vec<sem::MethodSpecializer>,
    },
    /// Runtime layout validation rejected the candidate layout. This is a
    /// safety net: the compiler bridge normally produces validated layouts.
    RuntimeValidationFailed {
        /// Underlying runtime layout error.
        error: RuntimeLayoutError,
    },
    /// The target is a built-in numeric generic. Its methods on numbers are
    /// sealed (see [`sem::GenericFunctionOrigin::admits_method`]), and Loko
    /// method source cannot name it yet, so no method can be added.
    SealedGeneric {
        /// Target generic id supplied by the caller.
        generic: sem::GenericFunctionId,
    },
}

/// Successful outcome of [`SessionArtifacts::rewind_to`].
///
/// Rewind is an operation *on* the mutation log, not an entry *in* it. The
/// outcome reports how many entries remain after the truncation and which
/// entries were dropped, so callers can react to the change without reading
/// the log themselves.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RewindOutcome {
    /// Number of mutation log entries retained after the rewind. This is
    /// the live log length, *not* the next id to be assigned: mutation ids
    /// are stable session event ids, so dropped ids are permanently retired
    /// and the next `apply_mutation` gets the next never-used id (which is
    /// in general greater than `retained_count` after a partial rewind).
    pub retained_count: usize,
    /// Mutation log entries that were dropped, in their original
    /// application order.
    pub dropped: Vec<AppliedMutation>,
    /// How the post-rewind artifacts were produced. The slice always reports
    /// [`RecomputeStrategy::FullCandidateRebuild`]; cache-reuse wording is
    /// reserved for the follow-up `IncrementalReuse` variant.
    pub recompute_strategy: RecomputeStrategy,
}

/// Structured failure returned by [`SessionArtifacts::rewind_to`].
///
/// On every variant the live session is left in its pre-rewind state with
/// no partial updates visible.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum RewindError {
    /// The target mutation id does not name an entry in the current log.
    /// Either the id was never assigned, or it was assigned and later
    /// dropped by a prior rewind (ids are not reused, so a dropped id
    /// never reappears). Callers can enumerate valid targets through
    /// [`SessionArtifacts::mutations`].
    TargetOutOfRange {
        /// Rejected target id.
        target: u32,
        /// Number of entries in the live log when the call was made.
        /// Useful as a hint, but does not bound valid targets: with stable
        /// session event ids the log can have gaps, so a small
        /// `current_count` does not mean only ids `0..current_count` are
        /// valid.
        current_count: usize,
    },
    /// Runtime layout validation rejected the candidate produced by
    /// rebuilding from the retained log. This is a safety net: a valid
    /// retained log should produce a valid layout, so this variant
    /// signals a compiler-bridge bug rather than an ordinary failure mode.
    RuntimeValidationFailed {
        /// Underlying runtime layout error.
        error: RuntimeLayoutError,
    },
}

/// Result of replaying a mutation log against a new base source.
///
/// Replay is *prefix replay*: entries are applied in order and the loop
/// stops at the first failure. `applied` contains the entries that
/// succeeded; `failed` is `Some` iff a later entry would have come next;
/// `artifacts` is the ephemeral session built from the new base plus the
/// successfully replayed prefix. The original session is unchanged.
#[derive(Clone, Debug)]
pub struct ReplayOutcome {
    /// Log entries that applied cleanly against the new base, in order.
    /// Each entry's `mutation_id` is the original id from the source log;
    /// replay never re-allocates ids.
    pub applied: Vec<AppliedMutation>,
    /// First entry that failed to apply, if any. When `None`, the entire
    /// log replayed cleanly.
    pub failed: Option<ReplayFailure>,
    /// Ephemeral session derived from the new base plus the successfully
    /// replayed prefix. Callers may inspect or adopt it; nothing about the
    /// original session has changed.
    pub artifacts: SessionArtifacts,
}

/// First log entry that failed to replay against a new base source.
#[derive(Clone, Debug)]
pub struct ReplayFailure {
    /// Original `mutation_id` of the failing entry.
    pub mutation_id: u32,
    /// Position of the failing entry in the original log.
    pub log_index: usize,
    /// Original [`SessionMutation`] request that failed to apply.
    pub request: SessionMutation,
    /// Structured cause from the apply path, reusing the same error
    /// taxonomy that [`SessionArtifacts::apply_mutation`] uses.
    pub cause: MutationError,
}

/// Kind of top-level source form reported by a session artifact.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TopLevelFormKind {
    /// `define class`.
    Class,
    /// `define generic`.
    Generic,
    /// `define method`.
    Method,
    /// `define function`.
    Function,
    /// `define constant`.
    Constant,
}

/// Source span for one parser-recognized top-level form.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TopLevelFormSpan {
    /// Zero-based index among parser-recognized top-level forms.
    pub index: usize,
    /// Form kind.
    pub kind: TopLevelFormKind,
    /// Byte span of the parsed form, excluding leading/trailing trivia.
    pub span: Span,
}

/// Origin of a compiler-recognized source form.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceFormOrigin {
    /// Form came from the base Loko CST.
    BaseCst,
    /// Form was emitted by a staged grammar region.
    StagedGrammar {
        /// Staged region slug.
        region: String,
        /// Zero-based form index inside the staged region.
        form_index: usize,
    },
}

/// Kind of compiler-recognized source form.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SourceFormKind {
    /// Base `define class`.
    Class,
    /// Base `define generic`.
    Generic,
    /// Base `define method`.
    Method,
    /// Base `define function`.
    Function,
    /// Base `define constant`.
    Constant,
    /// Staged frontend class form.
    FrontendClass,
    /// Staged frontend function form.
    FrontendFunction,
}

impl SourceFormKind {
    /// Returns a stable display label for this form kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Class => "class definition",
            Self::Generic => "generic definition",
            Self::Method => "method definition",
            Self::Function => "function definition",
            Self::Constant => "constant definition",
            Self::FrontendClass => "frontend class",
            Self::FrontendFunction => "frontend function",
        }
    }
}

/// Unified provenance record for one source-level form.
///
/// Base Loko definitions and staged grammar emissions both appear here so
/// tools can ask "what source forms did this compile see?" without knowing
/// which frontend path produced each form.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceFormProvenance {
    /// Stable display id within this compile.
    pub id: String,
    /// Form kind.
    pub kind: SourceFormKind,
    /// Human-readable title.
    pub title: String,
    /// Original source span.
    pub span: Span,
    /// Origin path that produced the form.
    pub origin: SourceFormOrigin,
    /// Grammar activation that parsed or emitted the form.
    pub activation: Option<grammar_frontend::GrammarActivationId>,
    /// Grammar that parsed or emitted the form.
    pub grammar: Option<grammar_frontend::GrammarId>,
    /// Environment-object ids produced from this form when known.
    pub produced_object_ids: Vec<String>,
    /// Leading documentation comment lines attached to this form.
    ///
    /// Lines come from the contiguous `///` block immediately before the
    /// source form, with the marker and one optional following space removed.
    pub doc_comments: Vec<String>,
    /// Grammar rule applications associated with this form.
    pub rule_applications: Vec<GrammarRuleApplication>,
    /// Staged frontend form payload, when this came from a staged grammar.
    pub frontend_form: Option<FrontendForm>,
}

/// Owned summary of one CST node that intersects a tooling query.
///
/// Returned by [`SessionArtifacts::syntax_nodes_containing_span`] so editor,
/// report, and REPL callers can reason about CST shape without borrowing the
/// full syntax tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntaxNodeSummary {
    /// Stable-ish CST node identity.
    pub id: SyntaxNodeId,
    /// CST node kind.
    pub kind: CstKind,
    /// Source span covered by this node.
    pub span: Span,
    /// Depth from the CST root, where the root is depth zero.
    pub depth: usize,
}

/// Owned summary of one token that intersects a tooling query.
///
/// Returned by [`SessionArtifacts::syntax_tokens_intersecting_span`] and
/// [`SessionArtifacts::syntax_trivia_intersecting_span`]. The token text is
/// copied out of the source so callers can display the summary without keeping
/// a borrow into [`SessionArtifacts`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntaxTokenSummary {
    /// Token kind.
    pub kind: TokenKind,
    /// Source span covered by this token.
    pub span: Span,
    /// Token text copied from the source.
    pub text: String,
}

/// Owned parse/provenance view for one source span.
///
/// Returned by [`SessionArtifacts::parse_view_for_span`] so editor, REPL, and
/// report callers can show the CST path, tokens, grammar applications, and
/// parser diagnostics for the same source selection without duplicating
/// traversal logic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceParseView {
    /// Queried source span.
    pub span: Span,
    /// CST nodes containing the queried span, outermost to innermost.
    pub nodes: Vec<SyntaxNodeSummary>,
    /// Non-trivia tokens intersecting the queried span.
    pub tokens: Vec<SyntaxTokenSummary>,
    /// Trivia tokens intersecting the queried span.
    pub trivia: Vec<SyntaxTokenSummary>,
    /// Grammar rule applications that touch the queried span, smallest first.
    pub rule_applications: Vec<GrammarRuleApplication>,
    /// Grammar diagnostic facts that touch the queried span.
    pub diagnostics: Vec<GrammarDiagnosticFact>,
}

impl SessionArtifacts {
    /// Returns stable artifact reports for this compilation.
    #[must_use]
    pub fn artifact_reports(&self) -> Vec<ArtifactReport> {
        self.invalidation.artifact_reports()
    }

    /// Returns stable artifact reports with work and timing metrics.
    #[must_use]
    pub fn artifact_metrics(&self) -> Vec<ArtifactMetricsReport> {
        self.invalidation
            .artifact_reports()
            .into_iter()
            .map(|report| ArtifactMetricsReport {
                artifact: report.artifact,
                key: report.key,
                metrics: self.metrics_for(report.artifact),
            })
            .collect()
    }

    /// Returns metrics for one query artifact.
    #[must_use]
    pub fn metrics_for(&self, artifact: QueryArtifact) -> ArtifactMetrics {
        self.artifact_metrics
            .iter()
            .find(|entry| entry.artifact == artifact)
            .map_or_else(
                || ArtifactMetrics {
                    work_units: artifact_work_units(artifact, self),
                    ..ArtifactMetrics::default()
                },
                |entry| entry.metrics,
            )
    }

    /// Returns these artifacts with soft typing diagnostics re-derived under
    /// `config`.
    ///
    /// Typing decisions do not depend on the configuration, so nothing is
    /// re-checked or recompiled: only the typing artifact's diagnostics and the
    /// aggregated diagnostic list change.
    #[must_use]
    pub fn with_typing_config(&self, config: typing::TypingConfig) -> Self {
        let mut artifacts = self.clone();
        let mut timings = ArtifactTimingCollector::default();
        let (typing, elapsed_nanos) =
            measure_elapsed(|| self.typing.with_config(&self.sem, config));
        timings.record(QueryArtifact::Typing, elapsed_nanos);
        let (diagnostics, elapsed_nanos) =
            measure_elapsed(|| aggregate_diagnostics(&self.sem, &typing));
        timings.record(QueryArtifact::Diagnostics, elapsed_nanos);
        artifacts.typing = typing;
        artifacts.diagnostics = diagnostics;
        artifacts.artifact_metrics = build_artifact_metrics(&artifacts, &timings);
        artifacts
    }

    /// Explains a typing environment object.
    ///
    /// `typing-config:session` renders the policy and every decision; a
    /// `typing:<definition>:t<n>` id renders one decision with its rule,
    /// operand types, and the policy that set its severity. Function, method,
    /// and constant ids render that definition's decisions.
    #[must_use]
    pub fn explain_typing(&self, id: &str) -> Option<String> {
        if id == TYPING_CONFIG_OBJECT_ID {
            return Some(self.typing.dump(&self.sem));
        }
        if let Some(decision) = self
            .typing
            .decisions()
            .find(|decision| typing_decision_object_id(decision) == id)
        {
            return Some(explain_typing_decision(self, decision));
        }
        let definition = self
            .environment_objects()
            .into_iter()
            .find(|object| object.id.as_str() == id)
            .filter(|object| {
                matches!(
                    object.kind,
                    EnvironmentKind::Function | EnvironmentKind::Method | EnvironmentKind::Constant
                )
            })?
            .source
            .definition?;
        Some(self.typing.dump_definition(&self.sem, definition))
    }

    /// Returns true when any aggregated diagnostic is an error.
    ///
    /// Warnings are advisory: a compilation whose only diagnostics are
    /// warnings still produced its lowered module and tape.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(Diagnostic::is_error)
    }

    /// Returns inspectable generic dispatch-site records in traversal order.
    #[must_use]
    pub fn dispatch_sites(&self) -> Vec<DispatchSiteRecord> {
        collect_dispatch_sites(self)
    }

    /// Returns parser-recognized top-level source forms in source order.
    ///
    /// These spans come from the compiler's CST, so tools can anchor their
    /// source-buffer views to parser facts rather than rescanning source text.
    #[must_use]
    pub fn top_level_forms(&self) -> Vec<TopLevelFormSpan> {
        collect_top_level_forms(&self.syntax)
    }

    /// Returns all source forms recognized by this compile.
    ///
    /// Unlike [`SessionArtifacts::top_level_forms`], this includes both base
    /// Loko CST definitions and structural frontend forms emitted by staged
    /// grammars. This is the preferred API for reports, workbenches, and source
    /// inspectors that need to explain the user-facing units of source.
    #[must_use]
    pub fn source_forms(&self) -> Vec<SourceFormProvenance> {
        collect_source_forms(self)
    }

    /// Returns the preserved frontend parse artifact.
    ///
    /// This is the stable bridge between grammar parsing and CST projection:
    /// it records which grammar activation parsed the source, which source
    /// variant was consumed, and the owned parse-tree projection when parsing
    /// succeeded.
    #[must_use]
    pub const fn frontend_parse_artifact(&self) -> &FrontendParseArtifact {
        &self.grammar_parse.frontend_parse
    }

    /// Returns CST nodes that contain `span`, ordered outermost to innermost.
    ///
    /// A zero-width `span` acts like a cursor position. Results are owned
    /// summaries rather than borrowed [`CstNode`] values so tooling can keep
    /// them after releasing the artifact borrow.
    #[must_use]
    pub fn syntax_nodes_containing_span(&self, span: Span) -> Vec<SyntaxNodeSummary> {
        let mut nodes = Vec::new();
        collect_syntax_nodes_containing_span(&self.syntax.root, span, 0, &mut nodes);
        nodes
    }

    /// Returns non-trivia tokens that intersect `span`, in source order.
    ///
    /// A zero-width `span` acts like a cursor position and returns the token
    /// under the cursor when one exists.
    #[must_use]
    pub fn syntax_tokens_intersecting_span(&self, span: Span) -> Vec<SyntaxTokenSummary> {
        self.syntax
            .tokens
            .iter()
            .copied()
            .filter(|token| !token.kind.is_trivia())
            .filter(|token| span_intersects_query(token.span, span))
            .map(|token| SyntaxTokenSummary {
                kind: token.kind,
                span: token.span,
                text: token.text(&self.syntax.source).to_string(),
            })
            .collect()
    }

    /// Returns trivia tokens that intersect `span`, in source order.
    ///
    /// A zero-width `span` acts like a cursor position and returns the trivia
    /// token under the cursor when one exists. Comments stay out of the CST but
    /// remain available through this query for formatters, documentation
    /// tooling, and source-preserving edits.
    #[must_use]
    pub fn syntax_trivia_intersecting_span(&self, span: Span) -> Vec<SyntaxTokenSummary> {
        self.syntax
            .tokens
            .iter()
            .copied()
            .filter(|token| token.kind.is_trivia())
            .filter(|token| span_intersects_query(token.span, span))
            .map(|token| SyntaxTokenSummary {
                kind: token.kind,
                span: token.span,
                text: token.text(&self.syntax.source).to_string(),
            })
            .collect()
    }

    /// Returns grammar diagnostic facts that touch `span`.
    ///
    /// This is a tooling query: non-empty spans overlap normally, while
    /// zero-width spans are treated as cursor positions.
    #[must_use]
    pub fn grammar_diagnostics_intersecting_span(&self, span: Span) -> Vec<GrammarDiagnosticFact> {
        self.grammar_parse
            .grammar_diagnostics
            .iter()
            .filter(|fact| span_touches_query(fact.span, span))
            .cloned()
            .collect()
    }

    /// Returns recorded grammar-rule applications that contain `span`.
    ///
    /// This uses the parser provenance recorded in
    /// [`GrammarParseReport::provenance`], not a form-kind heuristic. Results
    /// are ordered from the smallest containing span to the largest so cursor
    /// tooling sees the most specific rule first.
    #[must_use]
    pub fn grammar_rule_applications_containing_span(
        &self,
        span: Span,
    ) -> Vec<GrammarRuleApplication> {
        let mut applications = self
            .grammar_parse
            .provenance
            .applications
            .iter()
            .filter(|application| span_contains(application.span, span))
            .cloned()
            .collect::<Vec<_>>();
        applications.sort_by_key(|application| {
            (
                application.span.end.saturating_sub(application.span.start),
                application.span.start,
            )
        });
        applications
    }

    /// Returns the parse/provenance view for one source span.
    ///
    /// This is the preferred tooling entry point for source selections. It
    /// joins CST containment, token intersection, parser rule applications, and
    /// parser diagnostics into one owned value that downstream tools can render
    /// without keeping internal tree borrows alive.
    #[must_use]
    pub fn parse_view_for_span(&self, span: Span) -> SourceParseView {
        SourceParseView {
            span,
            nodes: self.syntax_nodes_containing_span(span),
            tokens: self.syntax_tokens_intersecting_span(span),
            trivia: self.syntax_trivia_intersecting_span(span),
            rule_applications: grammar_rule_applications_touching_span(&self.grammar_parse, span),
            diagnostics: self.grammar_diagnostics_intersecting_span(span),
        }
    }

    /// Returns the live EOP mutation log in application order.
    ///
    /// Each entry represents one successful
    /// [`SessionArtifacts::apply_mutation`] call. The log is the single
    /// source of truth for session-mutation provenance; the `sequence`
    /// carried by a method's [`sem::MethodOrigin::Session`] matches the
    /// `mutation_id` of the log entry that inserted it.
    #[must_use]
    pub fn mutations(&self) -> &[AppliedMutation] {
        &self.mutations
    }

    /// Returns the pristine source text fixed at first compile and
    /// preserved byte-for-byte across every mutation rebuild. This is
    /// the rebuild base used by `apply_mutation`, `rewind_to`, and
    /// `replay_against`; durable transcripts persist it as `base.loko`.
    #[must_use]
    pub fn pristine_source(&self) -> &str {
        &self.pristine_source
    }

    /// Returns the next session event id that would be assigned to a
    /// fresh `apply_mutation`. Preserved across rewind; may be strictly
    /// greater than `self.mutations().last().map(|e| e.mutation_id) + 1`
    /// when a tail of mutations was dropped.
    #[must_use]
    pub const fn next_mutation_id(&self) -> u32 {
        self.next_mutation_id
    }

    /// Overrides the next mutation id counter.
    ///
    /// Used by transcript restore to set the ephemeral session's
    /// counter to the originating session's recorded high-water mark,
    /// rather than the `max(applied) + 1` value the replay loop would
    /// otherwise leave behind. Crate-private because the only honest
    /// caller is the restore path; an outside caller setting an
    /// arbitrary counter could silently allow id reuse. Gated on the
    /// `std` feature because the transcript module that uses it is
    /// itself `std`-gated, and a default/no-std build would otherwise
    /// see this as dead code.
    #[cfg(feature = "std")]
    pub(crate) fn set_next_mutation_id(&mut self, value: u32) {
        self.next_mutation_id = value;
    }

    /// Returns one dispatch-site record by friendly selector id.
    #[must_use]
    pub fn dispatch_site(&self, id: &str) -> Option<DispatchSiteRecord> {
        self.dispatch_sites().into_iter().find(|site| site.id == id)
    }

    /// Dumps a compact list of generic dispatch sites.
    #[must_use]
    pub fn dump_dispatch_sites(&self) -> String {
        dump_dispatch_sites(self)
    }

    /// Dumps the resolved grammar stack.
    #[must_use]
    pub fn dump_grammar_stack(&self) -> String {
        dump_grammar_stack(&self.grammar_parse)
    }

    /// Dumps the registered language environments and staged grammar adapters.
    #[must_use]
    pub fn dump_grammar_registry(&self) -> String {
        dump_grammar_registry(self.grammar_registry)
    }

    /// Dumps registered grammar extension descriptors and concrete use records.
    #[must_use]
    pub fn dump_grammar_extensions(&self) -> String {
        dump_grammar_extensions(self)
    }

    /// Dumps parse provenance and grammar activations.
    #[must_use]
    pub fn dump_parse_trace(&self) -> String {
        dump_parse_trace(&self.grammar_parse)
    }

    /// Dumps the parser shadow report for this compile.
    #[must_use]
    pub fn dump_parser_shadow_report(&self) -> String {
        dump_parser_shadow_report(&self.grammar_parse)
    }

    /// Explains a staged grammar extension environment object id.
    #[must_use]
    pub fn explain_grammar_extension(&self, id: &str) -> Option<String> {
        explain_grammar_extension(self, id)
    }

    /// Explains the grammars owned by a language environment or one grammar.
    #[must_use]
    pub fn explain_grammar_environment(&self, id: &str) -> Option<String> {
        explain_grammar_environment(self, id)
    }

    /// Explains direct rules projected by one grammar.
    #[must_use]
    pub fn explain_direct_grammar_rules(&self, id: &str) -> Option<String> {
        explain_direct_grammar_rules(self, id)
    }

    /// Explains effective rules resolved for one grammar.
    #[must_use]
    pub fn explain_effective_grammar_rules(&self, id: &str) -> Option<String> {
        explain_effective_grammar_rules(self, id)
    }

    /// Explains grammar activations for a language environment or grammar.
    #[must_use]
    pub fn explain_grammar_activations(&self, id: &str) -> Option<String> {
        explain_grammar_activations(self, id)
    }

    /// Explains a grammar rule environment object id.
    #[must_use]
    pub fn explain_grammar_rule(&self, id: &str) -> Option<String> {
        self.grammar_parse
            .rules
            .iter()
            .find(|rule| grammar_frontend::grammar_rule_object_id(rule) == id)
            .map(explain_grammar_rule)
            .or_else(|| {
                self.grammar_parse
                    .effective_rules
                    .iter()
                    .find(|rule| grammar_frontend::effective_grammar_rule_object_id(rule) == id)
                    .map(explain_effective_grammar_rule)
            })
    }

    /// Explains the structured expression for a grammar rule object id.
    #[must_use]
    pub fn explain_grammar_expression(&self, id: &str) -> Option<String> {
        explain_grammar_expression(self, id)
    }

    /// Explains a grammar action object or the action attached to a rule.
    #[must_use]
    pub fn explain_grammar_action(&self, id: &str) -> Option<String> {
        explain_grammar_action(self, id)
    }

    /// Explains captures consumed by a grammar action object or rule.
    #[must_use]
    pub fn explain_grammar_action_captures(&self, id: &str) -> Option<String> {
        explain_grammar_action_captures(self, id)
    }

    /// Explains the output shape produced by a grammar action object or rule.
    #[must_use]
    pub fn explain_grammar_action_output(&self, id: &str) -> Option<String> {
        explain_grammar_action_output(self, id)
    }

    /// Explains one generic dispatch site by friendly selector id.
    #[must_use]
    pub fn explain_dispatch_site(&self, id: &str) -> Option<String> {
        self.dispatch_site(id)
            .map(|site| explain_dispatch_site(self, &site))
    }

    /// Calls a direct function with typed scalar arguments and records an
    /// execution trace when `trace` requests one.
    ///
    /// This calm embedding surface currently supports integer, boolean, and
    /// string arguments and exactly one result of one of those categories.
    /// Object handles and VM-owned storage do not cross the API boundary.
    pub fn call_function(
        &self,
        function_name: &str,
        arguments: &[FunctionValue],
        trace: Option<ExecutionTraceDetail>,
    ) -> Result<FunctionCallOutcome, FunctionCallError> {
        let tape = self
            .tape
            .as_ref()
            .ok_or(FunctionCallError::TapeUnavailable)?;
        let mut runtime = InterlokoHost::new(tape.runtime.clone());
        call_function(
            &self.sem,
            &self.host_registry,
            tape,
            &mut runtime,
            function_name,
            arguments,
            trace,
            None,
        )
    }

    /// Creates a runtime image for these artifacts that successive calls
    /// share; see [`SessionRuntime`].
    pub fn runtime(&self) -> Result<SessionRuntime, FunctionCallError> {
        let tape = self
            .tape
            .as_ref()
            .ok_or(FunctionCallError::TapeUnavailable)?;
        Ok(SessionRuntime {
            host: InterlokoHost::new(tape.runtime.clone()),
        })
    }

    /// Calls a direct function while serving declared scalar host functions
    /// through `handler`.
    ///
    /// Host calls remain inside the bounded VM run. The handler should capture
    /// intent or answer a query; product authorization and durable mutation
    /// belong after this method returns successfully.
    pub fn call_function_with_host(
        &self,
        function_name: &str,
        arguments: &[FunctionValue],
        trace: Option<ExecutionTraceDetail>,
        handler: &mut dyn HostFunctionHandler,
    ) -> Result<FunctionCallOutcome, FunctionCallError> {
        let tape = self
            .tape
            .as_ref()
            .ok_or(FunctionCallError::TapeUnavailable)?;
        let mut runtime = InterlokoHost::new(tape.runtime.clone());
        call_function(
            &self.sem,
            &self.host_registry,
            tape,
            &mut runtime,
            function_name,
            arguments,
            trace,
            Some(handler),
        )
    }

    /// Dumps the recorded runtime trace of `main`, when present.
    #[must_use]
    pub fn dump_execution_trace(&self) -> String {
        self.runtime_trace.as_ref().map_or_else(
            || "execution trace unavailable: traced execution was not requested\n".into(),
            |trace| self.execution_trace_view(trace).render(),
        )
    }

    /// Reads `trace` through these artifacts, resolving tape functions to
    /// Interloko definitions and instruction spans to source positions.
    ///
    /// Meaning is attached only when these artifacts emitted the exact
    /// program the trace recorded; otherwise the view reports raw ids.
    #[must_use]
    pub fn execution_trace_view<'a>(&'a self, trace: &'a ExecutionTrace) -> ExecutionTraceView<'a> {
        ExecutionTraceView::new(
            trace,
            self.lowered.as_ref(),
            self.tape.as_ref(),
            &self.source.source,
        )
    }

    /// Returns structured environment objects projected from these artifacts.
    #[must_use]
    pub fn environment_objects(&self) -> Vec<EnvironmentObject> {
        let mut objects = Vec::new();
        self.project_environment(&mut objects);
        objects
    }

    /// Projects structured environment objects from these artifacts into `sink`.
    pub fn project_environment(&self, sink: &mut dyn EnvironmentObjectSink) {
        project_session_environment(self, sink);
    }

    /// Returns compiler and host-provided environment objects.
    #[must_use]
    pub fn environment_objects_with_host(
        &self,
        host: &dyn HostEnvironmentProvider,
    ) -> Vec<EnvironmentObject> {
        let mut objects = Vec::new();
        self.project_environment_with_host(host, &mut objects);
        objects
    }

    /// Projects compiler and explicit host-provided environment objects.
    pub fn project_environment_with_host(
        &self,
        host: &dyn HostEnvironmentProvider,
        sink: &mut dyn EnvironmentObjectSink,
    ) {
        self.project_environment(sink);
        host.project_environment(sink);
    }

    /// Returns this artifact set's source file identity.
    #[must_use]
    pub const fn source_file(&self) -> SourceFileId {
        self.source.id
    }

    /// Dumps one stage as text.
    #[must_use]
    pub fn dump_stage(&self, stage: DumpStage) -> String {
        match stage {
            DumpStage::Syntax => dump_syntax(&self.syntax),
            DumpStage::Ast => dump_ast(&self.ast),
            DumpStage::Sem => dump_sem(&self.sem),
            DumpStage::Mop => dump_mop(self),
            DumpStage::Eop => dump_eop(self),
            DumpStage::Lowered => self.lowered.as_ref().map_or_else(
                || format!("lowered unavailable: {:?}\n", self.lower_error),
                lower::Module::dump,
            ),
            DumpStage::Tape => self.tape.as_ref().map_or_else(
                || format!("tape unavailable: {:?}\n", self.tape_error),
                |tape| {
                    format!(
                        "{}",
                        execution_tape::disasm::disassemble_verified(&tape.program)
                    )
                },
            ),
            DumpStage::Typing => self.typing.dump(&self.sem),
            DumpStage::All => {
                let mut out = String::new();
                for stage in [
                    DumpStage::Syntax,
                    DumpStage::Ast,
                    DumpStage::Sem,
                    DumpStage::Mop,
                    DumpStage::Eop,
                    DumpStage::Lowered,
                    DumpStage::Tape,
                    DumpStage::Typing,
                ] {
                    let _ = writeln!(out, "== {stage:?} ==");
                    out.push_str(&self.dump_stage(stage));
                    if !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push('\n');
                }
                out
            }
        }
    }

    /// Dumps one stage for a single projected environment object.
    ///
    /// This is the object-scoped counterpart to [`Self::dump_stage`]. Whole
    /// stage dumps remain available through [`Self::dump_stage`]; this method
    /// keeps workbench tools from slicing broad text dumps when the user has
    /// already selected an environment object.
    #[must_use]
    pub fn dump_environment_object_stage(
        &self,
        object_id: &str,
        stage: DumpStage,
    ) -> Option<String> {
        let object = self
            .environment_objects()
            .into_iter()
            .find(|object| object.id.as_str() == object_id)?;
        if object.kind == EnvironmentKind::SourceFile {
            return Some(self.dump_stage(stage));
        }
        Some(dump_environment_object_stage(self, &object, stage))
    }

    /// Dumps the MOP dispatch-table section.
    #[must_use]
    pub fn dump_mop_dispatch_table(&self) -> String {
        dump_mop_dispatch_table(&self.sem)
    }

    /// Dumps the MOP runtime-layout section.
    #[must_use]
    pub fn dump_mop_runtime_layout(&self) -> String {
        dump_mop_runtime_layout(self)
    }

    /// Dumps stable artifact keys and dependencies.
    #[must_use]
    pub fn dump_artifact_graph(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "artifact graph for {} ({})",
            self.source.name, self.source.id
        );
        let _ = writeln!(out, "artifacts:");
        for report in self.artifact_metrics() {
            let _ = writeln!(
                out,
                "  {} {} work={} ticks={} ns={}",
                report.key,
                artifact_label(report.artifact),
                report.metrics.work_units,
                report.metrics.elapsed_ticks,
                report.metrics.elapsed_nanos
            );
        }
        let _ = writeln!(out, "dependencies:");
        for edge in self.invalidation.artifact_edges() {
            let _ = writeln!(
                out,
                "  {} <- {}",
                artifact_label(edge.dependent),
                artifact_label(edge.dependency)
            );
        }
        out
    }

    /// Applies one structured EOP mutation through candidate-and-swap.
    ///
    /// On success the live session is replaced with a fully recomputed set of
    /// artifacts and a structured [`MutationOutcome`] is returned. On any
    /// failure the live session is left unchanged and the structured
    /// [`MutationError`] explains the rejection.
    ///
    /// This slice runs the full pipeline on every mutation; narrowing through
    /// the artifact graph is intentionally a follow-up commit.
    pub fn apply_mutation(
        &mut self,
        mutation: SessionMutation,
    ) -> Result<MutationOutcome, MutationError> {
        let mutation_id = self.next_mutation_id;
        self.apply_session_mutation_with_id(mutation, mutation_id)
    }

    /// Applies one structured EOP mutation through candidate-and-swap with
    /// an explicit mutation id.
    ///
    /// This is the internal entry point used by [`Self::apply_mutation`]
    /// (which supplies `self.next_mutation_id`), [`Self::replay_against`]
    /// (which supplies each replayed entry's original id), and the
    /// transcript restore path (which supplies each persisted event's
    /// recorded id). Replay must preserve original ids so the ephemeral
    /// session's [`sem::MethodOrigin::Session`] markers match the log it
    /// was derived from; allocating fresh ids during replay would
    /// silently break that correspondence.
    pub(crate) fn apply_session_mutation_with_id(
        &mut self,
        mutation: SessionMutation,
        mutation_id: u32,
    ) -> Result<MutationOutcome, MutationError> {
        let outcome = match &mutation {
            SessionMutation::AddMethod {
                generic,
                method_source,
            } => self.apply_add_method(*generic, method_source, mutation_id)?,
        };
        // The mutation log is the single source of truth for provenance.
        // apply_add_method already swapped in the candidate with the prior
        // log copied forward; this push records the new entry on top so
        // SessionArtifacts::mutations() reflects the application order.
        self.mutations.push(AppliedMutation {
            mutation_id: outcome.mutation_id,
            request: mutation,
            outcome: outcome.clone(),
        });
        Ok(outcome)
    }

    fn apply_add_method(
        &mut self,
        generic: sem::GenericFunctionId,
        method_source: &str,
        mutation_id: u32,
    ) -> Result<MutationOutcome, MutationError> {
        let target_generic = self
            .sem
            .generics
            .get(generic.0 as usize)
            .ok_or(MutationError::UnknownGeneric { generic })?
            .clone();
        if matches!(
            target_generic.origin,
            sem::GenericFunctionOrigin::Numeric { .. }
        ) {
            return Err(MutationError::SealedGeneric { generic });
        }

        let original_class_count = self.ast.classes.len();
        let original_generic_count = self.ast.generics.len();
        let original_function_count = self.ast.functions.len();
        let original_user_method_count = self.ast.methods.len();
        // Advisory warnings never reject a mutation; only errors count.
        let pre_mutation_diagnostic_codes: Vec<DiagnosticCode> = self
            .diagnostics
            .iter()
            .filter(|diag| diag.is_error())
            .map(|diag| diag.code)
            .collect();

        // Compose the candidate source deterministically from the pristine
        // source plus the existing log plus the pending request. This keeps
        // `self.source.source` from growing in place: pristine is the
        // rebuild base; the composed text is recomputed every time.
        let pending_request = SessionMutation::AddMethod {
            generic,
            method_source: method_source.to_string(),
        };
        let candidate_source_text = compose_source_from_log(
            &self.pristine_source,
            self.mutations
                .iter()
                .map(|entry| &entry.request)
                .chain(core::iter::once(&pending_request)),
        );
        let mut candidate_source_file = self.source.clone();
        candidate_source_file.source = candidate_source_text;

        let candidate = compile_file_with_registries(
            &candidate_source_file,
            RuntimeMode::CompileOnly,
            self.grammar_registry,
            &self.host_registry,
            self.typing.config,
        );

        let new_diagnostics: Vec<Diagnostic> = candidate
            .diagnostics
            .iter()
            .filter(|diag| diag.is_error())
            .filter(|diag| {
                let code = diag.code;
                let pre_count = pre_mutation_diagnostic_codes
                    .iter()
                    .filter(|c| **c == code)
                    .count();
                let after_count = candidate
                    .diagnostics
                    .iter()
                    .filter(|d| d.is_error() && d.code == code)
                    .count();
                after_count > pre_count
            })
            .cloned()
            .collect();

        // Parse failures must be reported before structural delta checks,
        // because a malformed mutation source may not produce a new AST method
        // at all.
        let parse_failures: Vec<Diagnostic> = new_diagnostics
            .iter()
            .filter(|d| {
                matches!(
                    d.code,
                    DiagnosticCode::SyntaxExpectedExpression | DiagnosticCode::SyntaxExpectedToken
                )
            })
            .cloned()
            .collect();
        if !parse_failures.is_empty() {
            return Err(MutationError::ParseFailed {
                diagnostics: parse_failures,
            });
        }

        // After parse success, the only allowed shape change is exactly one
        // new method appended. This rejects class/generic/function additions
        // that share the mutation source, which are out of scope for this
        // slice.
        if candidate.ast.classes.len() != original_class_count
            || candidate.ast.generics.len() != original_generic_count
            || candidate.ast.functions.len() != original_function_count
            || candidate.ast.methods.len() != original_user_method_count + 1
        {
            return Err(MutationError::NotAMethodDefinition);
        }

        // sem::lower_module lowers AST methods in source order before
        // appending compiler-synthesized slot setters, so a candidate that
        // appended exactly one AST method places the inserted method at sem
        // index original_user_method_count.
        let new_method_index = original_user_method_count;
        let new_method = candidate
            .sem
            .methods
            .get(new_method_index)
            .ok_or(MutationError::NotAMethodDefinition)?
            .clone();

        if new_method.generic != generic {
            let actual_name = candidate
                .sem
                .generics
                .get(new_method.generic.0 as usize)
                .map_or_else(|| String::from("<unresolved>"), |g| g.name.clone());
            return Err(MutationError::GenericNameMismatch {
                generic,
                expected: target_generic.name,
                actual: actual_name,
            });
        }

        // Duplicate-specializer check runs before the general semantic-error
        // check because an exact specializer match also fires sem's ambiguity
        // diagnostic; the session-level affordance reports this specific
        // failure mode rather than the broader semantic-failed bucket.
        for (other_index, other) in candidate.sem.methods.iter().enumerate() {
            if other_index == new_method_index {
                continue;
            }
            if other.generic == new_method.generic && other.specializers == new_method.specializers
            {
                return Err(MutationError::DuplicateSpecializer {
                    existing_method: other.id,
                    specializers: other.specializers.clone(),
                });
            }
        }

        // Anything else new in the diagnostic set is a genuine semantic
        // failure (unknown class, type mismatch, etc.).
        let semantic_failures: Vec<Diagnostic> = new_diagnostics
            .into_iter()
            .filter(|d| {
                !matches!(
                    d.code,
                    DiagnosticCode::SyntaxExpectedExpression
                        | DiagnosticCode::SyntaxExpectedToken
                        | DiagnosticCode::AmbiguousGenericMethodDispatch
                )
            })
            .collect();
        if !semantic_failures.is_empty() {
            return Err(MutationError::SemanticFailed {
                diagnostics: semantic_failures,
            });
        }

        let runtime_layout = runtime_bridge::runtime_layout_from_sem(&candidate.sem);
        if let Err(error) = runtime_layout.validate() {
            return Err(MutationError::RuntimeValidationFailed { error });
        }

        // Mutation ids are stable session event ids supplied by the caller
        // (apply_mutation uses self.next_mutation_id; replay uses each
        // entry's original id). The next-id counter advances to one past
        // whichever id is larger, so a normal apply increments by one and
        // a replay that uses out-of-order historical ids still leaves the
        // counter strictly above the max ever assigned.
        let new_method_definition = new_method.definition;
        let affected_generic_definition = candidate
            .sem
            .generics
            .get(generic.0 as usize)
            .map(|g| g.definition)
            .unwrap_or(new_method.definition);

        let mut new_artifacts = candidate;

        // Carry the mutation log forward onto the candidate so the new
        // entry can be appended (in apply_session_mutation_with_id) after
        // the swap.
        new_artifacts.mutations = self.mutations.clone();
        // Preserve the pristine source across the rebuild. The candidate's
        // pristine_source was initialized to the composed candidate text;
        // the live session's pristine_source is the rebuild base and must
        // not drift.
        new_artifacts.pristine_source = self.pristine_source.clone();
        // Advance the stable id counter. apply_session_mutation_with_id
        // appends the new AppliedMutation entry after the swap; the counter
        // on new_artifacts already accounts for it. max() against
        // self.next_mutation_id ensures replay's out-of-order ids do not
        // accidentally roll the counter backward.
        new_artifacts.next_mutation_id = self.next_mutation_id.max(mutation_id.saturating_add(1));

        // The candidate compile parses prior session-inserted methods out of
        // the appended source as ordinary source-origin definitions, which
        // clears their MethodOrigin::Session marker. Restore the marker for
        // every prior mutation by walking the log directly: each entry's
        // outcome.inserted_method still points at the same method id in the
        // recompiled sem module (slot setters move; user methods do not).
        // Provenance is derived from the log, not from method-count
        // arithmetic.
        for entry in &new_artifacts.mutations {
            let idx = entry.outcome.inserted_method.0 as usize;
            new_artifacts.sem.methods[idx].origin = sem::MethodOrigin::Session {
                sequence: entry.mutation_id,
            };
        }
        // Tag the freshly inserted method. apply_mutation will push the
        // matching AppliedMutation onto the log after the swap.
        new_artifacts.sem.methods[new_method.id.0 as usize].origin = sem::MethodOrigin::Session {
            sequence: mutation_id,
        };

        // Affected list is the dependency-graph truth, not "what we just
        // recomputed". The top-level pipeline artifacts genuinely change
        // because the source was appended; per-method artifacts for the
        // inserted method are new; the affected generic's dispatcher table
        // gained a method. Unrelated methods' per-method artifacts and
        // unrelated generics' dispatchers are intentionally omitted.
        //
        // EnvironmentResolution and GrammarStack are derived from the source
        // file's grammar environment, which is unchanged by an add-method
        // mutation, so they are not affected.
        let affected_artifacts = Vec::from([
            QueryArtifact::Source,
            QueryArtifact::GrammarParse,
            QueryArtifact::Parse,
            QueryArtifact::Ast,
            QueryArtifact::Bind,
            QueryArtifact::Diagnostics,
            QueryArtifact::MethodLookup(new_method_definition),
            QueryArtifact::MethodLowering(new_method_definition),
            QueryArtifact::MethodTape(new_method_definition),
            QueryArtifact::GenericDispatcher(affected_generic_definition),
            QueryArtifact::LoweredModule,
            QueryArtifact::TapeProgram,
            QueryArtifact::RuntimeResult,
        ]);

        *self = new_artifacts;

        Ok(MutationOutcome {
            mutation_id,
            inserted_method: new_method.id,
            affected_artifacts,
            recompute_strategy: RecomputeStrategy::FullCandidateRebuild,
        })
    }

    /// Truncates the live mutation log at `target` (or back to pristine when
    /// `None`) and rebuilds session state through candidate-and-swap.
    ///
    /// Rewind is an operation *on* the log, not an entry *in* it. The
    /// mutation log stays a forward transcript of domain mutations; the
    /// truncated tail is reported in the returned [`RewindOutcome`] but is
    /// never appended to the log.
    ///
    /// `target = Some(id)` requires `id` to name an entry currently in the
    /// live log; on success that entry and every earlier entry are
    /// retained, and the strict suffix is dropped. A target that does not
    /// name a live entry (because it was never assigned, or because it was
    /// dropped by a prior rewind) returns [`RewindError::TargetOutOfRange`].
    /// `target = None` retains no entries and returns the session to its
    /// pristine state. A no-op rewind (target names the current tail)
    /// succeeds without recompiling.
    ///
    /// On any failure the live session is left in its pre-rewind state.
    /// Mutation ids are stable session event ids: dropped ids are
    /// permanently retired and the next
    /// [`SessionArtifacts::apply_mutation`] gets the next never-used id,
    /// not a recycled one. The post-rewind log can carry a deliberate gap
    /// where a dropped id used to be.
    pub fn rewind_to(&mut self, target: Option<u32>) -> Result<RewindOutcome, RewindError> {
        let current_count = self.mutations.len();
        let retained_count = match target {
            None => 0,
            Some(id) => {
                // Stable session event ids mean the log can have gaps after
                // earlier rewinds, so a target id has to be looked up by
                // matching a live entry, not by treating the id as a log
                // index.
                let position = self
                    .mutations
                    .iter()
                    .position(|entry| entry.mutation_id == id)
                    .ok_or(RewindError::TargetOutOfRange {
                        target: id,
                        current_count,
                    })?;
                position + 1
            }
        };

        // No-op: target names the current tail; the log already has the
        // requested shape.
        if retained_count == current_count {
            return Ok(RewindOutcome {
                retained_count,
                dropped: Vec::new(),
                recompute_strategy: RecomputeStrategy::FullCandidateRebuild,
            });
        }

        let retained_requests = self.mutations[..retained_count]
            .iter()
            .map(|entry| &entry.request);
        let composed_source_text =
            compose_source_from_log(&self.pristine_source, retained_requests);
        let mut candidate_source_file = self.source.clone();
        candidate_source_file.source = composed_source_text;

        let candidate = compile_file_with_registries(
            &candidate_source_file,
            RuntimeMode::CompileOnly,
            self.grammar_registry,
            &self.host_registry,
            self.typing.config,
        );

        let runtime_layout = runtime_bridge::runtime_layout_from_sem(&candidate.sem);
        if let Err(error) = runtime_layout.validate() {
            return Err(RewindError::RuntimeValidationFailed { error });
        }

        let mut new_artifacts = candidate;
        new_artifacts.pristine_source = self.pristine_source.clone();
        new_artifacts.mutations = self.mutations[..retained_count].to_vec();
        // Preserve the stable id counter across rewind. Dropped ids are
        // permanently retired; the next apply_mutation gets
        // self.next_mutation_id, not a reused id.
        new_artifacts.next_mutation_id = self.next_mutation_id;

        // Log-driven retag: every retained session method gets its origin
        // restored from the retained log entry. Same machinery as the
        // apply path; the rebuild has no idea which methods came from
        // mutations without the log.
        for entry in &new_artifacts.mutations {
            let idx = entry.outcome.inserted_method.0 as usize;
            new_artifacts.sem.methods[idx].origin = sem::MethodOrigin::Session {
                sequence: entry.mutation_id,
            };
        }

        let dropped: Vec<AppliedMutation> = self.mutations[retained_count..].to_vec();
        *self = new_artifacts;

        Ok(RewindOutcome {
            retained_count,
            dropped,
            recompute_strategy: RecomputeStrategy::FullCandidateRebuild,
        })
    }

    /// Replays this session's mutation log against a new base source,
    /// returning the resulting ephemeral session and a per-entry report.
    ///
    /// Replay is prefix replay: log entries are applied in order, using
    /// each entry's *original* mutation id (so the ephemeral session's
    /// [`sem::MethodOrigin::Session`] markers match the original log),
    /// and the loop stops at the first failure. The returned
    /// [`ReplayOutcome`] carries the successfully applied entries, the
    /// failure (if any), and the ephemeral artifacts the caller may
    /// adopt.
    ///
    /// This method does not mutate `self`. The original session's log,
    /// pristine source, and artifacts are byte-for-byte unchanged
    /// regardless of replay outcome.
    ///
    /// `replay_against` deliberately does not silently skip failed entries.
    /// Skip-failed replay would change the meaning of a transcript
    /// ("this log applies modulo these gaps"); it can land later as an
    /// explicit named mode.
    #[must_use]
    pub fn replay_against(&self, new_source: &SourceFile) -> ReplayOutcome {
        let mut ephemeral = compile_file_with_registries(
            new_source,
            RuntimeMode::CompileOnly,
            self.grammar_registry,
            &self.host_registry,
            self.typing.config,
        );

        let mut applied: Vec<AppliedMutation> = Vec::new();
        let mut failed: Option<ReplayFailure> = None;

        for (log_index, entry) in self.mutations.iter().enumerate() {
            let request = entry.request.clone();
            match ephemeral.apply_session_mutation_with_id(request.clone(), entry.mutation_id) {
                Ok(_outcome) => {
                    // The ephemeral session's mutations vector just gained
                    // the new entry; mirror it into `applied` so callers
                    // do not need to dig into artifacts.mutations().
                    if let Some(last) = ephemeral.mutations.last() {
                        applied.push(last.clone());
                    }
                }
                Err(cause) => {
                    failed = Some(ReplayFailure {
                        mutation_id: entry.mutation_id,
                        log_index,
                        request,
                        cause,
                    });
                    break;
                }
            }
        }

        ReplayOutcome {
            applied,
            failed,
            artifacts: ephemeral,
        }
    }
}

/// Composes session source by appending each retained mutation's source onto
/// the pristine base. The format mirrors what `apply_add_method` produced when
/// it appended in place: a blank line between definitions plus a trailing
/// newline. The helper is the source-side seed for log-driven rebuilds;
/// rewind reuses it once the log has been truncated.
fn compose_source_from_log<'a>(
    pristine: &str,
    requests: impl IntoIterator<Item = &'a SessionMutation>,
) -> String {
    let mut composed = String::from(pristine);
    for request in requests {
        match request {
            SessionMutation::AddMethod { method_source, .. } => {
                composed.push_str("\n\n");
                composed.push_str(method_source.trim());
                composed.push('\n');
            }
        }
    }
    composed
}

/// Diff between two compiled source versions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionDiff {
    /// Definitions present in both versions with equal fingerprints.
    pub preserved: Vec<DefinitionReport>,
    /// Added, removed, or modified definitions.
    pub changed: Vec<DefinitionChange>,
    /// Query artifacts affected by the detected edit shape.
    pub affected: Vec<ArtifactReport>,
}

impl SessionDiff {
    /// Renders this diff as text.
    #[must_use]
    pub fn dump(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "preserved definitions:");
        for definition in &self.preserved {
            let _ = writeln!(
                out,
                "  {} {:?} {}",
                definition.id, definition.kind, definition.name
            );
        }
        let _ = writeln!(out, "changed definitions:");
        for change in &self.changed {
            let _ = writeln!(
                out,
                "  {:?} {} {:?} {}",
                change.change, change.definition.id, change.definition.kind, change.definition.name
            );
        }
        let _ = writeln!(out, "affected artifacts:");
        for report in &self.affected {
            let _ = writeln!(out, "  {} {}", report.key, artifact_label(report.artifact));
        }
        out
    }
}

/// Stable definition report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefinitionReport {
    /// Stable definition id.
    pub id: DefinitionId,
    /// Definition kind.
    pub kind: DefinitionKind,
    /// Display name.
    pub name: String,
}

/// Kind of definition change.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DefinitionChangeKind {
    /// Definition was added.
    Added,
    /// Definition was removed.
    Removed,
    /// Definition kept its id but changed content.
    Modified,
}

/// Changed definition report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefinitionChange {
    /// Definition report.
    pub definition: DefinitionReport,
    /// Change kind.
    pub change: DefinitionChangeKind,
}

/// In-memory compiler session.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CompilerSession {
    files: Vec<SourceFile>,
    grammar_registry: GrammarFrontendRegistry,
    typing_config: typing::TypingConfig,
}

impl CompilerSession {
    /// Creates an empty session.
    #[must_use]
    pub fn new() -> Self {
        Self::with_grammar_registry(GrammarFrontendRegistry::builtin())
    }

    /// Creates an empty session with an explicit grammar frontend registry.
    #[must_use]
    pub const fn with_grammar_registry(grammar_registry: GrammarFrontendRegistry) -> Self {
        Self {
            files: Vec::new(),
            grammar_registry,
            typing_config: typing::TypingConfig::soft(),
        }
    }

    /// Returns the grammar frontend registry owned by this session.
    #[must_use]
    pub const fn grammar_registry(&self) -> GrammarFrontendRegistry {
        self.grammar_registry
    }

    /// Returns the soft typing configuration used by this session's compiles.
    #[must_use]
    pub const fn typing_config(&self) -> typing::TypingConfig {
        self.typing_config
    }

    /// Sets the soft typing configuration used by future compiles.
    pub fn set_typing_config(&mut self, config: typing::TypingConfig) {
        self.typing_config = config;
    }

    /// Adds a named source file and returns its id.
    pub fn add_source(&mut self, name: &str, source: &str) -> SourceFileId {
        let file = SourceFile::named(name, source);
        let id = file.id;
        self.files.push(file);
        id
    }

    /// Adds a named source file with an explicit grammar environment.
    pub fn add_source_with_grammar(
        &mut self,
        name: &str,
        source: &str,
        grammar_environment: GrammarEnvironmentSelection,
    ) -> SourceFileId {
        let file = SourceFile::named_with_grammar(name, source, grammar_environment);
        let id = file.id;
        self.files.push(file);
        id
    }

    /// Adds a source file with an explicit id.
    pub fn add_source_with_id(&mut self, id: SourceFileId, name: &str, source: &str) {
        self.files.push(SourceFile::with_id(id, name, source));
    }

    /// Adds a source file with an explicit id and grammar environment.
    pub fn add_source_with_id_and_grammar(
        &mut self,
        id: SourceFileId,
        name: &str,
        source: &str,
        grammar_environment: GrammarEnvironmentSelection,
    ) {
        self.files.push(SourceFile::with_id_and_grammar(
            id,
            name,
            source,
            grammar_environment,
        ));
    }

    /// Changes one source file's grammar environment.
    pub fn set_grammar_environment(
        &mut self,
        id: SourceFileId,
        grammar_environment: GrammarEnvironmentSelection,
    ) -> Option<()> {
        let file = self.files.iter_mut().find(|file| file.id == id)?;
        file.grammar_environment = grammar_environment;
        Some(())
    }

    /// Returns a source file by id.
    #[must_use]
    pub fn source(&self, id: SourceFileId) -> Option<&SourceFile> {
        self.files.iter().find(|file| file.id == id)
    }

    /// Compiles one source by id.
    #[must_use]
    pub fn compile(&self, id: SourceFileId, mode: RuntimeMode) -> Option<SessionArtifacts> {
        self.source(id).map(|file| {
            compile_file_with_grammar_registry(
                file,
                mode,
                self.grammar_registry,
                self.typing_config,
            )
        })
    }

    /// Compiles one source by id with explicit host-backed metadata.
    #[must_use]
    pub fn compile_with_host_registry(
        &self,
        id: SourceFileId,
        mode: RuntimeMode,
        registry: &sem::HostMetaRegistry,
    ) -> Option<SessionArtifacts> {
        self.source(id).map(|file| {
            compile_file_with_registries(
                file,
                mode,
                self.grammar_registry,
                registry,
                self.typing_config,
            )
        })
    }

    /// Compiles a named source without storing it in the session.
    #[must_use]
    pub fn compile_source(name: &str, source: &str, mode: RuntimeMode) -> SessionArtifacts {
        compile_file(&SourceFile::named(name, source), mode)
    }

    /// Compiles a named source with an explicit grammar frontend registry
    /// without storing it in a session.
    #[must_use]
    pub fn compile_source_with_registry(
        name: &str,
        source: &str,
        mode: RuntimeMode,
        grammar_registry: GrammarFrontendRegistry,
    ) -> SessionArtifacts {
        compile_file_with_grammar_registry(
            &SourceFile::named(name, source),
            mode,
            grammar_registry,
            typing::TypingConfig::default(),
        )
    }

    /// Compiles a named source with an explicit grammar environment without
    /// storing it in the session.
    #[must_use]
    pub fn compile_source_with_grammar(
        name: &str,
        source: &str,
        mode: RuntimeMode,
        grammar_environment: GrammarEnvironmentSelection,
    ) -> SessionArtifacts {
        compile_file(
            &SourceFile::named_with_grammar(name, source, grammar_environment),
            mode,
        )
    }

    /// Compiles a named source with host-backed metaobject declarations.
    #[must_use]
    pub fn compile_source_with_host_registry(
        name: &str,
        source: &str,
        mode: RuntimeMode,
        registry: &sem::HostMetaRegistry,
    ) -> SessionArtifacts {
        compile_file_with_host_registry(&SourceFile::named(name, source), mode, registry)
    }

    /// Compiles a named source with host-backed metadata and an explicit
    /// grammar environment.
    #[must_use]
    pub fn compile_source_with_grammar_and_host_registry(
        name: &str,
        source: &str,
        mode: RuntimeMode,
        grammar_environment: GrammarEnvironmentSelection,
        registry: &sem::HostMetaRegistry,
    ) -> SessionArtifacts {
        compile_file_with_host_registry(
            &SourceFile::named_with_grammar(name, source, grammar_environment),
            mode,
            registry,
        )
    }

    /// Compiles source with an explicit source file id without storing it.
    #[must_use]
    pub fn compile_source_with_id(
        id: SourceFileId,
        name: &str,
        source: &str,
        mode: RuntimeMode,
    ) -> SessionArtifacts {
        compile_file(&SourceFile::with_id(id, name, source), mode)
    }

    /// Compiles source with an explicit source file id and grammar environment
    /// without storing it.
    #[must_use]
    pub fn compile_source_with_id_and_grammar(
        id: SourceFileId,
        name: &str,
        source: &str,
        mode: RuntimeMode,
        grammar_environment: GrammarEnvironmentSelection,
    ) -> SessionArtifacts {
        compile_file(
            &SourceFile::with_id_and_grammar(id, name, source, grammar_environment),
            mode,
        )
    }

    /// Compiles source with an explicit source file id and host-backed registry.
    #[must_use]
    pub fn compile_source_with_id_and_host_registry(
        id: SourceFileId,
        name: &str,
        source: &str,
        mode: RuntimeMode,
        registry: &sem::HostMetaRegistry,
    ) -> SessionArtifacts {
        compile_file_with_host_registry(&SourceFile::with_id(id, name, source), mode, registry)
    }

    /// Compiles source with an explicit source file id, grammar environment,
    /// and host-backed registry.
    #[must_use]
    pub fn compile_source_with_id_grammar_and_host_registry(
        id: SourceFileId,
        name: &str,
        source: &str,
        mode: RuntimeMode,
        grammar_environment: GrammarEnvironmentSelection,
        registry: &sem::HostMetaRegistry,
    ) -> SessionArtifacts {
        compile_file_with_host_registry(
            &SourceFile::with_id_and_grammar(id, name, source, grammar_environment),
            mode,
            registry,
        )
    }

    /// Diffs two source versions.
    #[must_use]
    pub fn diff(before: &SessionArtifacts, after: &SessionArtifacts) -> SessionDiff {
        diff_artifacts(before, after)
    }
}

/// Incremental compiler session with an in-memory artifact cache.
///
/// This session owns source edit state and cache reporting. It reuses the
/// grammar frontend, AST lowering, semantic lowering, executable IR lowering,
/// tape emission, and runtime execution stages.
#[derive(Clone, Debug, Default)]
pub struct IncrementalCompilerSession {
    files: Vec<CachedSourceFile>,
    grammar_registry: GrammarFrontendRegistry,
    typing_config: typing::TypingConfig,
}

impl IncrementalCompilerSession {
    /// Creates an empty incremental session.
    #[must_use]
    pub fn new() -> Self {
        Self::with_grammar_registry(GrammarFrontendRegistry::builtin())
    }

    /// Creates an empty incremental session with an explicit grammar frontend registry.
    #[must_use]
    pub const fn with_grammar_registry(grammar_registry: GrammarFrontendRegistry) -> Self {
        Self {
            files: Vec::new(),
            grammar_registry,
            typing_config: typing::TypingConfig::soft(),
        }
    }

    /// Returns the grammar frontend registry owned by this session.
    #[must_use]
    pub const fn grammar_registry(&self) -> GrammarFrontendRegistry {
        self.grammar_registry
    }

    /// Returns the soft typing configuration used by this session's compiles.
    #[must_use]
    pub const fn typing_config(&self) -> typing::TypingConfig {
        self.typing_config
    }

    /// Sets the soft typing configuration used by future compiles.
    ///
    /// A later compile of an unchanged source re-derives typing diagnostics
    /// from cached decisions; nothing else is recompiled.
    pub fn set_typing_config(&mut self, config: typing::TypingConfig) {
        self.typing_config = config;
    }

    /// Adds a named source file and returns its stable id.
    pub fn add_source(&mut self, name: &str, source: &str) -> SourceFileId {
        let file = SourceFile::named(name, source);
        let id = file.id;
        self.files.push(CachedSourceFile::new(file));
        id
    }

    /// Adds a named source file with an explicit grammar environment.
    pub fn add_source_with_grammar(
        &mut self,
        name: &str,
        source: &str,
        grammar_environment: GrammarEnvironmentSelection,
    ) -> SourceFileId {
        let file = SourceFile::named_with_grammar(name, source, grammar_environment);
        let id = file.id;
        self.files.push(CachedSourceFile::new(file));
        id
    }

    /// Adds a source file with an explicit stable id.
    pub fn add_source_with_id(&mut self, id: SourceFileId, name: &str, source: &str) {
        self.files
            .push(CachedSourceFile::new(SourceFile::with_id(id, name, source)));
    }

    /// Adds a source file with an explicit stable id and grammar environment.
    pub fn add_source_with_id_and_grammar(
        &mut self,
        id: SourceFileId,
        name: &str,
        source: &str,
        grammar_environment: GrammarEnvironmentSelection,
    ) {
        self.files
            .push(CachedSourceFile::new(SourceFile::with_id_and_grammar(
                id,
                name,
                source,
                grammar_environment,
            )));
    }

    /// Edits a source file's text while preserving its stable id and name.
    pub fn edit_source(&mut self, id: SourceFileId, source: &str) -> Option<()> {
        let file = self.file_mut(id)?;
        file.source.source = source.to_string();
        Some(())
    }

    /// Changes a source file's grammar environment while preserving its text.
    pub fn set_grammar_environment(
        &mut self,
        id: SourceFileId,
        grammar_environment: GrammarEnvironmentSelection,
    ) -> Option<()> {
        let file = self.file_mut(id)?;
        file.source.grammar_environment = grammar_environment;
        Some(())
    }

    /// Returns a source file by id.
    #[must_use]
    pub fn source(&self, id: SourceFileId) -> Option<&SourceFile> {
        self.files
            .iter()
            .find(|file| file.source.id == id)
            .map(|file| &file.source)
    }

    /// Compiles one source file through the incremental cache.
    pub fn compile(&mut self, id: SourceFileId, mode: RuntimeMode) -> Option<IncrementalUpdate> {
        let index = self.files.iter().position(|file| file.source.id == id)?;
        Some(self.compile_index(index, mode))
    }

    /// Edits and compiles one source file through the incremental cache.
    pub fn edit_and_compile(
        &mut self,
        id: SourceFileId,
        source: &str,
        mode: RuntimeMode,
    ) -> Option<IncrementalUpdate> {
        self.edit_source(id, source)?;
        self.compile(id, mode)
    }

    /// Changes a source file's grammar environment and recompiles it.
    pub fn set_grammar_environment_and_compile(
        &mut self,
        id: SourceFileId,
        grammar_environment: GrammarEnvironmentSelection,
        mode: RuntimeMode,
    ) -> Option<IncrementalUpdate> {
        self.set_grammar_environment(id, grammar_environment)?;
        self.compile(id, mode)
    }

    fn file_mut(&mut self, id: SourceFileId) -> Option<&mut CachedSourceFile> {
        self.files.iter_mut().find(|file| file.source.id == id)
    }

    fn compile_index(&mut self, index: usize, mode: RuntimeMode) -> IncrementalUpdate {
        let source = self.files[index].source.clone();
        let previous_artifacts = self.files[index].artifacts.clone();
        let previous_cache = self.files[index].cache.clone();
        let previous_source = previous_artifacts
            .as_ref()
            .map(|artifacts| artifacts.source.clone());
        let previous_typing_config = previous_artifacts
            .as_ref()
            .map(|artifacts| artifacts.typing.config);
        let artifacts = self.files[index].artifacts_for(
            &source,
            mode,
            self.grammar_registry,
            self.typing_config,
        );
        let diff = previous_artifacts
            .as_ref()
            .map(|before| CompilerSession::diff(before, &artifacts));
        let reports = cache_reports(
            previous_cache.as_ref(),
            diff.as_ref(),
            previous_source.as_ref(),
            previous_typing_config,
            &artifacts,
        );
        let cache = ArtifactCache::from_update(&artifacts, &reports);
        let revision = self.files[index].revision.saturating_add(1);

        self.files[index].source = source.clone();
        self.files[index].artifacts = Some(artifacts.clone());
        self.files[index].cache = Some(cache);
        self.files[index].revision = revision;

        IncrementalUpdate {
            source,
            artifacts,
            diff,
            reports,
            revision,
        }
    }
}

#[derive(Clone, Debug)]
struct CachedSourceFile {
    source: SourceFile,
    artifacts: Option<SessionArtifacts>,
    cache: Option<ArtifactCache>,
    revision: u64,
}

impl CachedSourceFile {
    const fn new(source: SourceFile) -> Self {
        Self {
            source,
            artifacts: None,
            cache: None,
            revision: 0,
        }
    }

    fn artifacts_for(
        &self,
        source: &SourceFile,
        mode: RuntimeMode,
        grammar_registry: GrammarFrontendRegistry,
        typing_config: typing::TypingConfig,
    ) -> SessionArtifacts {
        if let Some(previous) = &self.artifacts
            && previous.source == *source
            && previous.grammar_registry == grammar_registry
        {
            if previous.typing.config == typing_config {
                return artifacts_for_mode(previous, mode);
            }
            // Only the reporting policy changed: re-derive typing diagnostics
            // from the recorded decisions instead of recompiling.
            return artifacts_for_mode(&previous.with_typing_config(typing_config), mode);
        }
        compile_file_with_grammar_registry(source, mode, grammar_registry, typing_config)
    }
}

fn artifacts_for_mode(previous: &SessionArtifacts, mode: RuntimeMode) -> SessionArtifacts {
    let mut artifacts = previous.clone();
    let mut timings = ArtifactTimingCollector::default();
    let (runtime_result, runtime_trace) = match mode {
        RuntimeMode::CompileOnly => (None, None),
        RuntimeMode::RunMain => (
            previous.runtime_result.clone().or_else(|| {
                let (result, elapsed_nanos) = measure_elapsed(|| {
                    previous
                        .tape
                        .as_ref()
                        .map(|tape| run_main(&previous.sem, &previous.host_registry, tape))
                });
                timings.record(QueryArtifact::RuntimeResult, elapsed_nanos);
                result
            }),
            None,
        ),
        RuntimeMode::RunMainTraced { detail } => {
            let ((result, trace), elapsed_nanos) = measure_elapsed(|| {
                previous.tape.as_ref().map_or((None, None), |tape| {
                    let (result, trace) = run_main_with_trace(
                        &previous.sem,
                        &previous.host_registry,
                        tape,
                        Some(detail),
                    );
                    (Some(result), trace)
                })
            });
            timings.record(QueryArtifact::RuntimeResult, elapsed_nanos);
            (result, trace)
        }
    };
    artifacts.runtime_result = runtime_result;
    artifacts.runtime_trace = runtime_trace;
    artifacts.artifact_metrics = build_artifact_metrics(&artifacts, &timings);
    artifacts
}

/// Result of compiling one file through [`IncrementalCompilerSession`].
#[derive(Clone, Debug)]
pub struct IncrementalUpdate {
    /// Source file after this update.
    pub source: SourceFile,
    /// Current compilation artifacts.
    pub artifacts: SessionArtifacts,
    /// Semantic diff from the previous cached snapshot, if any.
    pub diff: Option<SessionDiff>,
    /// Cache report for every artifact known to this update's graph.
    pub reports: Vec<CacheArtifactReport>,
    /// Monotonic per-file session revision.
    pub revision: u64,
}

impl IncrementalUpdate {
    /// Returns dependency edges for this update's artifact graph.
    #[must_use]
    pub fn dependency_edges(&self) -> &[ArtifactEdge] {
        self.artifacts.invalidation.artifact_edges()
    }

    /// Returns stable artifact reports for this update.
    #[must_use]
    pub fn artifact_reports(&self) -> Vec<ArtifactReport> {
        self.artifacts.artifact_reports()
    }

    /// Dumps cache status, counters, and semantic edit impact.
    #[must_use]
    pub fn dump_cache_report(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "incremental report for {} ({}) revision {} grammar {}",
            self.source.name,
            self.source.id,
            self.revision,
            self.source.grammar_environment.as_str()
        );
        let _ = writeln!(out, "summary:");
        let _ = writeln!(out, "  reused {}", self.reused_count());
        let _ = writeln!(out, "  recomputed {}", self.recomputed_count());
        let _ = writeln!(out, "  invalidated {}", self.invalidated_count());
        let _ = writeln!(out, "  affected {}", self.affected_count());
        let _ = writeln!(out, "artifacts:");
        for report in &self.reports {
            let _ = writeln!(
                out,
                "  {:?}{} {} invalidated={} affected={} work={} ticks={} ns={}",
                report.action,
                if report.action == CacheAction::Reused {
                    "    "
                } else {
                    ""
                },
                artifact_label(report.artifact),
                report.invalidated,
                report.affected,
                report.metrics.work_units,
                report.metrics.elapsed_ticks,
                report.metrics.elapsed_nanos
            );
        }
        let _ = writeln!(out, "dependencies:");
        for edge in self.dependency_edges() {
            let _ = writeln!(
                out,
                "  {} <- {}",
                artifact_label(edge.dependent),
                artifact_label(edge.dependency)
            );
        }
        if let Some(diff) = &self.diff {
            out.push_str("diff:\n");
            out.push_str(&diff.dump());
        }
        out
    }

    /// Number of artifacts reused by this update.
    #[must_use]
    pub fn reused_count(&self) -> usize {
        self.reports
            .iter()
            .filter(|report| report.action == CacheAction::Reused)
            .count()
    }

    /// Number of artifacts recomputed by this update.
    #[must_use]
    pub fn recomputed_count(&self) -> usize {
        self.reports
            .iter()
            .filter(|report| report.action == CacheAction::Recomputed)
            .count()
    }

    /// Number of previously cached artifacts invalidated by this update.
    #[must_use]
    pub fn invalidated_count(&self) -> usize {
        self.reports
            .iter()
            .filter(|report| report.invalidated)
            .count()
    }

    /// Number of artifacts marked semantically affected by this update.
    #[must_use]
    pub fn affected_count(&self) -> usize {
        self.reports.iter().filter(|report| report.affected).count()
    }
}

/// Cache action for one artifact in an incremental update.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CacheAction {
    /// A previous cache entry was valid for this artifact.
    Reused,
    /// The artifact was produced for this update.
    Recomputed,
}

/// Per-artifact work counters and optional wall-clock timing.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ArtifactMetrics {
    /// Logical work units charged for the artifact action.
    pub work_units: u64,
    /// Logical elapsed ticks charged for the artifact action.
    pub elapsed_ticks: u64,
    /// Observed wall-clock duration in nanoseconds when `std` timing is enabled.
    ///
    /// Foundational `no_std` builds expose this field as `0`.
    pub elapsed_nanos: u64,
}

/// Stable artifact report with work and timing metrics.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ArtifactMetricsReport {
    /// Query artifact.
    pub artifact: QueryArtifact,
    /// Stable artifact key.
    pub key: ArtifactKey,
    /// Work and timing metrics for the artifact.
    pub metrics: ArtifactMetrics,
}

/// Cache status for one artifact.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CacheArtifactReport {
    /// Query artifact.
    pub artifact: QueryArtifact,
    /// Stable artifact key.
    pub key: ArtifactKey,
    /// Cache action.
    pub action: CacheAction,
    /// Whether a previous entry existed and was replaced.
    pub invalidated: bool,
    /// Whether semantic edit analysis marked this artifact affected.
    pub affected: bool,
    /// Deterministic counters for this artifact action.
    pub metrics: ArtifactMetrics,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct ArtifactMetricEntry {
    artifact: QueryArtifact,
    metrics: ArtifactMetrics,
}

/// Stable backing key for one dispatch-site record.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct DispatchSiteKey(
    /// Stable raw identifier.
    pub u64,
);

impl DispatchSiteKey {
    /// Returns the raw key bits.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Display for DispatchSiteKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "dsk{:016x}", self.0)
    }
}

/// Kind of source definition enclosing a dispatch site.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DispatchSiteOwnerKind {
    /// The dispatch site is inside a direct function body.
    Function,
    /// The dispatch site is inside a method body.
    Method,
}

impl DispatchSiteOwnerKind {
    /// Returns the stable text label for this owner kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
        }
    }
}

/// Source definition enclosing a dispatch site.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchSiteOwner {
    /// Owner kind.
    pub kind: DispatchSiteOwnerKind,
    /// Stable owner definition id.
    pub definition: DefinitionId,
    /// Stable owner display name.
    pub name: String,
}

/// One method considered by a dispatch-site explanation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchMethodCandidate {
    /// Method id.
    pub method: sem::MethodId,
    /// Stable method definition id.
    pub definition: DefinitionId,
    /// Method specializer class.
    pub specializer: sem::ClassId,
    /// Method specializers in generic parameter order.
    pub specializers: Vec<sem::MethodSpecializer>,
    /// Distance from the static exact class to the specializer, if known.
    pub distance: Option<usize>,
    /// Per-argument distances from exact static classes to specializers.
    pub distances: Vec<Option<usize>>,
    /// Whether this is the statically selected method.
    pub selected: bool,
    /// Whether this candidate can only be selected by runtime class dispatch.
    pub runtime_only: bool,
}

/// One rejected method in a dispatch-site explanation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchMethodRejection {
    /// Method id.
    pub method: sem::MethodId,
    /// Stable method definition id.
    pub definition: DefinitionId,
    /// Method specializer class.
    pub specializer: sem::ClassId,
    /// Method specializers in generic parameter order.
    pub specializers: Vec<sem::MethodSpecializer>,
    /// Stable rejection reason.
    pub reason: String,
}

/// Inspectable record for one semantic generic call site.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchSiteRecord {
    /// Friendly selector such as `dispatch:d0`.
    pub id: String,
    /// Stable backing key derived from source, owner, call-site, and generic.
    pub key: DispatchSiteKey,
    /// Zero-based dispatch-site index in module traversal order.
    pub index: usize,
    /// Enclosing function or method.
    pub owner: DispatchSiteOwner,
    /// Source span for the semantic generic call.
    pub span: Span,
    /// Generic function id.
    pub generic: sem::GenericFunctionId,
    /// Stable generic definition id.
    pub generic_definition: DefinitionId,
    /// Static kind of the first dispatch argument, when present.
    pub static_argument_kind: Option<sem::ValueKind>,
    /// Static kind of each dispatch argument.
    pub static_argument_kinds: Vec<sem::ValueKind>,
    /// Exact static runtime class, when the call argument is an exact
    /// construction/cast expression.
    pub exact_static_class: Option<sem::ClassId>,
    /// Exact static runtime classes for dispatch arguments known at compile time.
    pub exact_static_classes: Vec<Option<sem::ClassId>>,
    /// Whether the runtime class is still needed to choose the method.
    pub runtime_needed: bool,
    /// Statically selected method, when known.
    pub selected_method: Option<sem::MethodId>,
    /// Applicable methods in dispatcher specificity order.
    pub applicable: Vec<DispatchMethodCandidate>,
    /// Rejected methods and reasons.
    pub rejected: Vec<DispatchMethodRejection>,
    /// Lowered dispatcher function name.
    pub lowered_dispatcher: String,
    /// Related query artifact reports.
    pub related_artifacts: Vec<ArtifactReport>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct ArtifactFingerprint(u64);

#[derive(Clone, Debug, Eq, PartialEq)]
struct CachedArtifact {
    artifact: QueryArtifact,
    key: ArtifactKey,
    fingerprint: ArtifactFingerprint,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ArtifactCache {
    entries: Vec<CachedArtifact>,
}

impl ArtifactCache {
    fn from_update(artifacts: &SessionArtifacts, reports: &[CacheArtifactReport]) -> Self {
        Self {
            entries: reports
                .iter()
                .map(|report| CachedArtifact {
                    artifact: report.artifact,
                    key: report.key,
                    fingerprint: artifact_fingerprint(report.artifact, artifacts),
                })
                .collect(),
        }
    }

    fn get(&self, key: ArtifactKey) -> Option<&CachedArtifact> {
        self.entries.iter().find(|entry| entry.key == key)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ArtifactTimingCollector {
    entries: Vec<(QueryArtifact, u64)>,
}

impl ArtifactTimingCollector {
    fn record(&mut self, artifact: QueryArtifact, elapsed_nanos: u64) {
        self.entries.push((artifact, elapsed_nanos));
    }

    fn elapsed_nanos(&self, artifact: QueryArtifact) -> Option<u64> {
        self.entries
            .iter()
            .rev()
            .find_map(|(entry, elapsed)| (*entry == artifact).then_some(*elapsed))
    }
}

fn build_artifact_metrics(
    artifacts: &SessionArtifacts,
    timings: &ArtifactTimingCollector,
) -> Vec<ArtifactMetricEntry> {
    artifacts
        .artifact_reports()
        .into_iter()
        .map(|report| {
            let work_units = artifact_work_units(report.artifact, artifacts);
            let elapsed_nanos = timings.elapsed_nanos(report.artifact).unwrap_or_else(|| {
                let ((), elapsed_nanos) = measure_elapsed(|| {
                    let _ = artifact_fingerprint(report.artifact, artifacts);
                });
                elapsed_nanos
            });
            ArtifactMetricEntry {
                artifact: report.artifact,
                metrics: ArtifactMetrics {
                    work_units,
                    elapsed_ticks: 1,
                    elapsed_nanos,
                },
            }
        })
        .collect()
}

fn cache_reports(
    previous: Option<&ArtifactCache>,
    diff: Option<&SessionDiff>,
    previous_source: Option<&SourceFile>,
    previous_typing_config: Option<typing::TypingConfig>,
    artifacts: &SessionArtifacts,
) -> Vec<CacheArtifactReport> {
    let typing_config_changed =
        previous_typing_config.is_some_and(|config| config != artifacts.typing.config);
    let source_changed =
        previous_source.is_some_and(|source| source.source != artifacts.source.source);
    let grammar_changed = previous_source
        .is_some_and(|source| source.grammar_environment != artifacts.source.grammar_environment);
    let affected = diff.map_or(&[][..], |diff| diff.affected.as_slice());
    artifacts
        .artifact_reports()
        .into_iter()
        .map(|report| {
            let ((fingerprint, work_units), elapsed_nanos) = measure_elapsed(|| {
                (
                    artifact_fingerprint(report.artifact, artifacts),
                    artifact_work_units(report.artifact, artifacts),
                )
            });
            let previous = previous.and_then(|cache| cache.get(report.key));
            let forced_by_grammar = grammar_changed && artifact_depends_on_grammar(report.artifact);
            let action = if !forced_by_grammar
                && previous.is_some_and(|entry| entry.fingerprint == fingerprint)
            {
                CacheAction::Reused
            } else {
                CacheAction::Recomputed
            };
            let sem_affected = affected.iter().any(|item| item.key == report.key);
            let retyped =
                typing_config_changed && artifact_depends_on_typing_config(report.artifact);
            let affected = sem_affected || forced_by_grammar || retyped;
            let invalidated = previous.is_some()
                && action == CacheAction::Recomputed
                && (source_changed || grammar_changed || sem_affected || retyped);
            let metrics = match action {
                CacheAction::Reused => ArtifactMetrics::default(),
                CacheAction::Recomputed => ArtifactMetrics {
                    work_units,
                    elapsed_ticks: 1,
                    elapsed_nanos,
                },
            };
            CacheArtifactReport {
                artifact: report.artifact,
                key: report.key,
                action,
                invalidated,
                affected,
                metrics,
            }
        })
        .collect()
}

fn artifact_depends_on_grammar(artifact: QueryArtifact) -> bool {
    !matches!(
        artifact,
        QueryArtifact::Source | QueryArtifact::TypingConfig
    )
}

fn artifact_depends_on_typing_config(artifact: QueryArtifact) -> bool {
    matches!(
        artifact,
        QueryArtifact::TypingConfig | QueryArtifact::Typing | QueryArtifact::Diagnostics
    )
}

fn artifact_fingerprint(
    artifact: QueryArtifact,
    artifacts: &SessionArtifacts,
) -> ArtifactFingerprint {
    let mut builder = StableIdBuilder::new(0x4341_4952_4e5f_4341);
    builder.mix_u64(artifact.key(artifacts.source.id).raw());
    match artifact {
        QueryArtifact::Source | QueryArtifact::Parse => {
            builder.mix_str(&artifacts.source.source);
        }
        QueryArtifact::EnvironmentResolution => {
            builder.mix_str(&artifacts.grammar_parse.environment.name);
            builder.mix_u64(artifacts.grammar_parse.environment.id.raw());
        }
        QueryArtifact::GrammarStack => {
            for grammar in &artifacts.grammar_parse.grammar_stack.grammars {
                builder.mix_u64(grammar.raw());
            }
        }
        QueryArtifact::GrammarParse => {
            builder.mix_str(&dump_parse_trace(&artifacts.grammar_parse));
            builder.mix_str(&artifacts.source.source);
        }
        QueryArtifact::Ast => {
            builder.mix_str(&dump_ast(&artifacts.ast));
        }
        QueryArtifact::Bind => {
            builder.mix_str(&dump_sem(&artifacts.sem));
        }
        QueryArtifact::Diagnostics => {
            mix_diagnostics(&mut builder, &artifacts.diagnostics);
        }
        QueryArtifact::MethodLookup(method) => {
            builder.mix_str(&fingerprint_method_lookup(&artifacts.sem, method));
        }
        QueryArtifact::MethodLowering(method) | QueryArtifact::MethodTape(method) => {
            builder.mix_str(&fingerprint_method_body(&artifacts.sem, method));
        }
        QueryArtifact::GenericDispatcher(generic) => {
            builder.mix_str(&fingerprint_dispatcher(&artifacts.sem, generic));
        }
        QueryArtifact::LoweredModule => {
            builder.mix_str(&fingerprint_lowered(artifacts));
        }
        QueryArtifact::TapeProgram => {
            builder.mix_str(&artifacts.dump_stage(DumpStage::Tape));
        }
        QueryArtifact::RuntimeResult => {
            builder.mix_str(&format!("{:?}", artifacts.runtime_result));
        }
        QueryArtifact::TypingConfig => {
            builder.mix_str(&format!("{:?}", artifacts.typing.config));
        }
        QueryArtifact::DefinitionTyping(definition) => {
            // Spans are left out so edits elsewhere in the file that only
            // shift offsets keep this definition's typing reusable.
            if let Some(typed) = artifacts
                .typing
                .definitions
                .iter()
                .find(|typed| typed.definition == definition)
            {
                for decision in &typed.decisions {
                    builder.mix_str(&format!(
                        "{} {} {} {} {:?} {:?} {:?}",
                        decision.index,
                        decision.check.as_str(),
                        decision.verdict.as_str(),
                        decision.rule.as_str(),
                        decision.expected,
                        decision.actual,
                        decision.selected_method
                    ));
                }
            }
        }
        QueryArtifact::Typing => {
            builder.mix_str(&format!(
                "{:?} {:?}",
                artifacts.typing.status, artifacts.typing.config
            ));
            for typed in &artifacts.typing.definitions {
                builder.mix_u64(typed.definition.raw());
                builder.mix_u64(
                    artifact_fingerprint(
                        QueryArtifact::DefinitionTyping(typed.definition),
                        artifacts,
                    )
                    .0,
                );
            }
            mix_diagnostics(&mut builder, &artifacts.typing.diagnostics);
        }
    }
    ArtifactFingerprint(builder.finish())
}

fn mix_diagnostics(builder: &mut StableIdBuilder, diagnostics: &[Diagnostic]) {
    builder.mix_u64(u64::try_from(diagnostics.len()).unwrap_or(u64::MAX));
    for diagnostic in diagnostics {
        builder.mix_str(diagnostic.code.as_str());
        builder.mix_str(diagnostic.severity.as_str());
        builder.mix_u64(u64::try_from(diagnostic.span.start).unwrap_or(u64::MAX));
        builder.mix_u64(u64::try_from(diagnostic.span.end).unwrap_or(u64::MAX));
        builder.mix_u64(u64::try_from(diagnostic.args.len()).unwrap_or(u64::MAX));
        for arg in &diagnostic.args {
            builder.mix_str(&arg.name);
            builder.mix_str(&arg.value.render());
        }
        builder.mix_u64(u64::try_from(diagnostic.labels.len()).unwrap_or(u64::MAX));
        for label in &diagnostic.labels {
            builder.mix_u64(u64::try_from(label.span.start).unwrap_or(u64::MAX));
            builder.mix_u64(u64::try_from(label.span.end).unwrap_or(u64::MAX));
            if let Some(message) = &label.message {
                builder.mix_str(message);
            }
        }
    }
}

fn fingerprint_method_lookup(module: &sem::Module, method: DefinitionId) -> String {
    let Some(method) = module.methods.iter().find(|item| item.definition == method) else {
        return "missing-method".into();
    };
    let generic = module
        .generics
        .get(method.generic.0 as usize)
        .map_or(0, |generic| generic.definition.raw());
    let specializers = method
        .specializers
        .iter()
        .map(|specializer| {
            specializer.map_or(0, |class| {
                module
                    .classes
                    .get(class.0 as usize)
                    .map_or(0, |class| class.definition.raw())
            })
        })
        .map(|definition| definition.to_string())
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "method-lookup:{}:{}:{}",
        method.definition.raw(),
        generic,
        specializers
    )
}

fn fingerprint_method_body(module: &sem::Module, method: DefinitionId) -> String {
    let Some(method) = module.methods.iter().find(|item| item.definition == method) else {
        return "missing-method-body".into();
    };
    let mut out = fingerprint_method_lookup(module, method.definition);
    out.push(':');
    out.push_str(&fingerprint_sem_block(module, &method.body));
    out
}

fn fingerprint_dispatcher(module: &sem::Module, generic: DefinitionId) -> String {
    let Some(generic) = module
        .generics
        .iter()
        .find(|item| item.definition == generic)
    else {
        return "missing-dispatcher".into();
    };
    let mut out = format!(
        "dispatcher:{}:{}:{}:{}:{}:",
        generic.definition.raw(),
        generic.name,
        generic.arity,
        generic
            .param_kinds
            .iter()
            .map(|kind| kind.map_or_else(
                || "untyped".into(),
                |kind| sem_kind_fingerprint(module, kind)
            ))
            .collect::<Vec<_>>()
            .join(","),
        sem_kind_list_fingerprint(module, &generic.result_kinds)
    );
    let mut methods: Vec<_> = module
        .methods
        .iter()
        .filter(|method| method.generic == generic.id)
        .collect();
    methods.sort_by(|left, right| {
        module
            .class_depth(right.specializer)
            .cmp(&module.class_depth(left.specializer))
            .then_with(|| left.id.0.cmp(&right.id.0))
    });
    for method in methods {
        let _ = write!(out, "{}:", method.definition.raw());
        for specializer in &method.specializers {
            match specializer {
                Some(class) => {
                    let definition = module
                        .classes
                        .get(class.0 as usize)
                        .map_or(0, |class| class.definition.raw());
                    let _ = write!(out, "{definition}");
                }
                None => out.push_str("object"),
            }
            out.push(',');
        }
        out.push('[');
        for class in &module.classes {
            let tuple =
                core::iter::repeat_n(class.id, method.specializers.len()).collect::<Vec<_>>();
            if module.method_applies_to_classes(method, &tuple) {
                let _ = write!(out, "{},", class.definition.raw());
            }
        }
        let _ = write!(out, "];");
    }
    out
}

fn fingerprint_lowered(artifacts: &SessionArtifacts) -> String {
    let Some(lowered) = &artifacts.lowered else {
        return format!("{:?}", artifacts.lower_error);
    };
    strip_span_comments(&lowered.dump())
}

fn strip_span_comments(input: &str) -> String {
    let mut out = String::new();
    for line in input.lines() {
        if let Some((prefix, _)) = line.split_once(" ; span ") {
            out.push_str(prefix);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

fn artifact_work_units(artifact: QueryArtifact, artifacts: &SessionArtifacts) -> u64 {
    match artifact {
        QueryArtifact::Source => usize_to_u64(artifacts.source.source.len()),
        QueryArtifact::EnvironmentResolution => 1,
        QueryArtifact::GrammarStack => {
            usize_to_u64(artifacts.grammar_parse.grammar_stack.grammars.len())
        }
        QueryArtifact::GrammarParse => {
            usize_to_u64(dump_parse_trace(&artifacts.grammar_parse).len())
                .saturating_add(usize_to_u64(artifacts.syntax.tokens.len()))
        }
        QueryArtifact::Parse => usize_to_u64(artifacts.source.source.len())
            .saturating_add(usize_to_u64(artifacts.syntax.tokens.len())),
        QueryArtifact::Ast => usize_to_u64(dump_ast(&artifacts.ast).len()),
        QueryArtifact::Bind => usize_to_u64(dump_sem(&artifacts.sem).len()),
        QueryArtifact::Diagnostics => artifacts
            .diagnostics
            .iter()
            .map(|diagnostic| {
                usize_to_u64(diagnostic.code.as_str().len())
                    .saturating_add(usize_to_u64(diagnostic.args.len()))
                    .saturating_add(usize_to_u64(diagnostic.labels.len()))
                    .saturating_add(1)
            })
            .sum(),
        QueryArtifact::MethodLookup(method) => {
            usize_to_u64(fingerprint_method_lookup(&artifacts.sem, method).len())
        }
        QueryArtifact::MethodLowering(method) | QueryArtifact::MethodTape(method) => {
            usize_to_u64(fingerprint_method_body(&artifacts.sem, method).len())
        }
        QueryArtifact::GenericDispatcher(generic) => {
            usize_to_u64(fingerprint_dispatcher(&artifacts.sem, generic).len())
        }
        QueryArtifact::LoweredModule => artifacts.lowered.as_ref().map_or(1, |lowered| {
            usize_to_u64(lowered.functions.len()).saturating_add(
                lowered
                    .functions
                    .iter()
                    .map(|function| usize_to_u64(function.instructions.len()))
                    .sum(),
            )
        }),
        QueryArtifact::TapeProgram => usize_to_u64(artifacts.dump_stage(DumpStage::Tape).len()),
        QueryArtifact::RuntimeResult | QueryArtifact::TypingConfig => 1,
        QueryArtifact::DefinitionTyping(definition) => artifacts
            .typing
            .definitions
            .iter()
            .find(|typed| typed.definition == definition)
            .map_or(1, |typed| {
                usize_to_u64(typed.decisions.len()).saturating_add(1)
            }),
        QueryArtifact::Typing => usize_to_u64(artifacts.typing.definitions.len())
            .saturating_add(usize_to_u64(artifacts.typing.diagnostics.len())),
    }
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(feature = "std")]
fn measure_elapsed<T>(work: impl FnOnce() -> T) -> (T, u64) {
    let start = std::time::Instant::now();
    let value = work();
    (value, duration_nanos_u64(start.elapsed()))
}

#[cfg(not(feature = "std"))]
fn measure_elapsed<T>(work: impl FnOnce() -> T) -> (T, u64) {
    (work(), 0)
}

#[cfg(feature = "std")]
fn duration_nanos_u64(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn collect_dispatch_sites(artifacts: &SessionArtifacts) -> Vec<DispatchSiteRecord> {
    let mut sites = Vec::new();
    for function in &artifacts.sem.functions {
        let owner = DispatchSiteOwner {
            kind: DispatchSiteOwnerKind::Function,
            definition: function.definition,
            name: function.name.clone(),
        };
        let mut owner_index = 0;
        collect_dispatch_sites_in_block(
            artifacts,
            owner,
            &function.body,
            &mut owner_index,
            &mut sites,
        );
    }
    for method in &artifacts.sem.methods {
        let generic_name = artifacts
            .sem
            .generics
            .get(method.generic.0 as usize)
            .map_or("<?>", |generic| generic.name.as_str());
        let owner = DispatchSiteOwner {
            kind: DispatchSiteOwnerKind::Method,
            definition: method.definition,
            name: format!(
                "{generic_name}{}",
                sem_specializer_vector_label(&artifacts.sem, &method.specializers)
            ),
        };
        let mut owner_index = 0;
        collect_dispatch_sites_in_block(
            artifacts,
            owner,
            &method.body,
            &mut owner_index,
            &mut sites,
        );
    }
    sites
}

fn collect_dispatch_sites_in_block(
    artifacts: &SessionArtifacts,
    owner: DispatchSiteOwner,
    block: &sem::Block,
    owner_index: &mut usize,
    sites: &mut Vec<DispatchSiteRecord>,
) {
    for binding in &block.lets {
        collect_dispatch_sites_in_expr(
            artifacts,
            owner.clone(),
            &binding.value,
            owner_index,
            sites,
        );
    }
    collect_dispatch_sites_in_expr(artifacts, owner, &block.result, owner_index, sites);
}

fn collect_dispatch_sites_in_expr(
    artifacts: &SessionArtifacts,
    owner: DispatchSiteOwner,
    expr: &sem::Expr,
    owner_index: &mut usize,
    sites: &mut Vec<DispatchSiteRecord>,
) {
    match expr {
        sem::Expr::DirectCall { args, .. } | sem::Expr::HostCall { args, .. } => {
            for arg in args {
                collect_dispatch_sites_in_expr(artifacts, owner.clone(), arg, owner_index, sites);
            }
        }
        sem::Expr::CallableCall { callable, args, .. } => {
            collect_dispatch_sites_in_expr(artifacts, owner.clone(), callable, owner_index, sites);
            for arg in args {
                collect_dispatch_sites_in_expr(artifacts, owner.clone(), arg, owner_index, sites);
            }
        }
        sem::Expr::GenericCall {
            generic,
            args,
            span,
        } => {
            let site = make_dispatch_site(
                artifacts,
                owner.clone(),
                *owner_index,
                sites.len(),
                *generic,
                args,
                *span,
            );
            *owner_index = owner_index.saturating_add(1);
            sites.push(site);
            for arg in args {
                collect_dispatch_sites_in_expr(artifacts, owner.clone(), arg, owner_index, sites);
            }
        }
        sem::Expr::Signal { condition, .. }
        | sem::Expr::ClassOf {
            object: condition, ..
        } => {
            collect_dispatch_sites_in_expr(artifacts, owner, condition, owner_index, sites);
        }
        sem::Expr::InstanceOf { value, .. } | sem::Expr::Cast { value, .. } => {
            collect_dispatch_sites_in_expr(artifacts, owner, value, owner_index, sites);
        }
        sem::Expr::MakeObject { class, fields, .. } => {
            collect_dispatch_sites_in_expr(artifacts, owner.clone(), class, owner_index, sites);
            for field in fields {
                collect_dispatch_sites_in_expr(
                    artifacts,
                    owner.clone(),
                    &field.value,
                    owner_index,
                    sites,
                );
            }
        }
        sem::Expr::AccessorCall { receiver, .. } => {
            collect_dispatch_sites_in_expr(artifacts, owner, receiver, owner_index, sites);
        }
        sem::Expr::SlotWrite {
            receiver, value, ..
        } => {
            collect_dispatch_sites_in_expr(artifacts, owner.clone(), receiver, owner_index, sites);
            collect_dispatch_sites_in_expr(artifacts, owner, value, owner_index, sites);
        }
        sem::Expr::Negate { value, .. } | sem::Expr::Convert { value, .. } => {
            collect_dispatch_sites_in_expr(artifacts, owner, value, owner_index, sites);
        }
        sem::Expr::FloatCall { args, .. } => {
            for arg in args {
                collect_dispatch_sites_in_expr(artifacts, owner.clone(), arg, owner_index, sites);
            }
        }
        sem::Expr::Binary { lhs, rhs, .. }
        | sem::Expr::Compare { lhs, rhs, .. }
        | sem::Expr::MixedCompare { lhs, rhs, .. } => {
            collect_dispatch_sites_in_expr(artifacts, owner.clone(), lhs, owner_index, sites);
            collect_dispatch_sites_in_expr(artifacts, owner, rhs, owner_index, sites);
        }
        sem::Expr::ValueEqual { lhs, rhs, .. } => {
            collect_dispatch_sites_in_expr(artifacts, owner.clone(), lhs, owner_index, sites);
            collect_dispatch_sites_in_expr(artifacts, owner, rhs, owner_index, sites);
        }
        sem::Expr::VectorLiteral { elements, .. }
        | sem::Expr::Values {
            values: elements, ..
        } => {
            for element in elements {
                collect_dispatch_sites_in_expr(
                    artifacts,
                    owner.clone(),
                    element,
                    owner_index,
                    sites,
                );
            }
        }
        sem::Expr::ConstantRef { constant, .. } => {
            if let Some(constant) = artifacts.sem.constants.get(constant.0 as usize) {
                collect_dispatch_sites_in_expr(
                    artifacts,
                    owner,
                    &constant.value,
                    owner_index,
                    sites,
                );
            }
        }
        sem::Expr::IntrinsicCall { args, .. } => {
            for arg in args {
                collect_dispatch_sites_in_expr(artifacts, owner.clone(), arg, owner_index, sites);
            }
        }
        sem::Expr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            collect_dispatch_sites_in_expr(artifacts, owner.clone(), condition, owner_index, sites);
            collect_dispatch_sites_in_expr(
                artifacts,
                owner.clone(),
                then_branch,
                owner_index,
                sites,
            );
            collect_dispatch_sites_in_expr(artifacts, owner, else_branch, owner_index, sites);
        }
        sem::Expr::Handle { body, restart, .. } => {
            collect_dispatch_sites_in_expr(artifacts, owner.clone(), body, owner_index, sites);
            collect_dispatch_sites_in_expr(artifacts, owner, &restart.body, owner_index, sites);
        }
        sem::Expr::Block { body, cleanup, .. } => {
            collect_dispatch_sites_in_block(artifacts, owner.clone(), body, owner_index, sites);
            if let Some(cleanup) = cleanup {
                collect_dispatch_sites_in_block(artifacts, owner, cleanup, owner_index, sites);
            }
        }
        sem::Expr::ConstI64 { .. }
        | sem::Expr::ConstF64 { .. }
        | sem::Expr::ConstBool { .. }
        | sem::Expr::StringLiteral { .. }
        | sem::Expr::SymbolLiteral { .. }
        | sem::Expr::Local { .. }
        | sem::Expr::Capture { .. }
        | sem::Expr::ClassObject { .. }
        | sem::Expr::KnownCallable { .. }
        | sem::Expr::Closure { .. }
        | sem::Expr::GenericFunctionObject { .. }
        | sem::Expr::Error { .. } => {}
    }
}

fn make_dispatch_site(
    artifacts: &SessionArtifacts,
    owner: DispatchSiteOwner,
    owner_index: usize,
    global_index: usize,
    generic_id: sem::GenericFunctionId,
    args: &[sem::Expr],
    span: Span,
) -> DispatchSiteRecord {
    let module = &artifacts.sem;
    let generic_definition = module.generics.get(generic_id.0 as usize).map_or(
        DefinitionId::new(module.source_file, DefinitionKind::Generic, "<missing>", ""),
        |generic| generic.definition,
    );
    let static_argument_kinds = args
        .iter()
        .map(|arg| module.expr_kind(arg))
        .collect::<Vec<_>>();
    let exact_static_classes = args
        .iter()
        .map(|arg| exact_static_class(module, arg))
        .collect::<Vec<_>>();
    let static_argument_kind = static_argument_kinds.first().copied();
    let exact_static_class = exact_static_classes.first().copied().flatten();
    let runtime_needed = exact_static_classes.iter().any(Option::is_none)
        && static_argument_kinds.iter().any(|kind| {
            matches!(kind, sem::ValueKind::Any) || module.value_kind_class(*kind).is_some()
        });
    let (applicable, rejected, selected_method) = dispatch_site_methods(
        artifacts,
        generic_id,
        &static_argument_kinds,
        &exact_static_classes,
    );
    let lowered_dispatcher = module.generics.get(generic_id.0 as usize).map_or_else(
        || format!("g{}#dispatch", generic_id.0),
        |generic| format!("{}#dispatch", generic.name),
    );
    DispatchSiteRecord {
        id: format!("dispatch:d{global_index}"),
        key: dispatch_site_key(
            artifacts.source.id,
            owner.definition,
            owner_index,
            generic_definition,
        ),
        index: global_index,
        owner,
        span,
        generic: generic_id,
        generic_definition,
        static_argument_kind,
        static_argument_kinds,
        exact_static_class,
        exact_static_classes,
        runtime_needed,
        selected_method,
        applicable,
        rejected,
        lowered_dispatcher,
        related_artifacts: dispatch_site_artifacts(artifacts, generic_id),
    }
}

fn dispatch_site_key(
    source: SourceFileId,
    owner: DefinitionId,
    owner_index: usize,
    generic: DefinitionId,
) -> DispatchSiteKey {
    let mut builder = StableIdBuilder::new(0x4341_4952_4453_5054);
    builder.mix_u64(source.raw());
    builder.mix_u64(owner.raw());
    builder.mix_u64(usize_to_u64(owner_index));
    builder.mix_u64(generic.raw());
    DispatchSiteKey(builder.finish())
}

fn dispatch_site_methods(
    artifacts: &SessionArtifacts,
    generic_id: sem::GenericFunctionId,
    static_kinds: &[sem::ValueKind],
    exact_classes: &[Option<sem::ClassId>],
) -> (
    Vec<DispatchMethodCandidate>,
    Vec<DispatchMethodRejection>,
    Option<sem::MethodId>,
) {
    let module = &artifacts.sem;
    let methods = dispatch_methods(module, generic_id);
    let exact_tuple = exact_classes.iter().copied().collect::<Option<Vec<_>>>();

    if let (Some(actuals), Some(lowered)) = (exact_tuple.as_ref(), artifacts.lowered.as_ref())
        && let Ok(lookup) = lowered.runtime.dispatch_lookup(generic_id, actuals)
    {
        return dispatch_site_methods_from_runtime_lookup(module, lookup);
    }

    let mut applicable = Vec::new();
    let mut rejected = Vec::new();
    let selected_method = exact_tuple.as_ref().and_then(|actuals| {
        match module.select_method_for_classes(generic_id, actuals) {
            sem::MethodSelection::Selected(method) => Some(method),
            sem::MethodSelection::None | sem::MethodSelection::Ambiguous => None,
        }
    });

    for method in methods {
        if let Some(actuals) = &exact_tuple {
            if module.method_applies_to_classes(method, actuals) {
                applicable.push(DispatchMethodCandidate {
                    method: method.id,
                    definition: method.definition,
                    specializer: method.specializer,
                    specializers: method.specializers.clone(),
                    distance: first_specializer_distance(module, method, actuals),
                    distances: specializer_distances(module, method, actuals),
                    selected: selected_method == Some(method.id),
                    runtime_only: false,
                });
            } else {
                rejected.push(DispatchMethodRejection {
                    method: method.id,
                    definition: method.definition,
                    specializer: method.specializer,
                    specializers: method.specializers.clone(),
                    reason: exact_tuple_rejection_reason(module, actuals, method),
                });
            }
            continue;
        }

        if method_may_apply_to_static_kinds(module, method, static_kinds) {
            applicable.push(DispatchMethodCandidate {
                method: method.id,
                definition: method.definition,
                specializer: method.specializer,
                specializers: method.specializers.clone(),
                distance: None,
                distances: Vec::new(),
                selected: false,
                runtime_only: true,
            });
        } else {
            rejected.push(DispatchMethodRejection {
                method: method.id,
                definition: method.definition,
                specializer: method.specializer,
                specializers: method.specializers.clone(),
                reason: static_tuple_rejection_reason(module, static_kinds, method),
            });
        }
    }

    (applicable, rejected, selected_method)
}

fn dispatch_site_methods_from_runtime_lookup(
    module: &sem::Module,
    lookup: runtime::RuntimeDispatchLookup,
) -> (
    Vec<DispatchMethodCandidate>,
    Vec<DispatchMethodRejection>,
    Option<sem::MethodId>,
) {
    let selected_method = lookup.selected_method();
    let applicable = lookup
        .applicable
        .into_iter()
        .filter_map(|candidate| {
            let method = module.methods.get(candidate.method.0 as usize)?;
            Some(DispatchMethodCandidate {
                method: candidate.method,
                definition: method.definition,
                specializer: method.specializer,
                specializers: method.specializers.clone(),
                distance: candidate.distances.first().copied(),
                distances: candidate.distances.into_iter().map(Some).collect(),
                selected: candidate.selected,
                runtime_only: false,
            })
        })
        .collect();
    let rejected = lookup
        .rejected
        .into_iter()
        .filter_map(|rejected| {
            let method = module.methods.get(rejected.method.0 as usize)?;
            Some(DispatchMethodRejection {
                method: rejected.method,
                definition: method.definition,
                specializer: method.specializer,
                specializers: method.specializers.clone(),
                reason: runtime_dispatch_rejection_reason_label(module, rejected.reason),
            })
        })
        .collect();
    (applicable, rejected, selected_method)
}

fn runtime_dispatch_rejection_reason_label(
    module: &sem::Module,
    reason: runtime::RuntimeDispatchRejectionReason,
) -> String {
    match reason {
        runtime::RuntimeDispatchRejectionReason::ArityMismatch { expected, actual } => {
            format!("arity mismatch: expected {expected} argument(s), got {actual}")
        }
        runtime::RuntimeDispatchRejectionReason::MissingSpecializer { argument } => {
            format!("argument {} has no runtime specializer", argument + 1)
        }
        runtime::RuntimeDispatchRejectionReason::ArgumentNotInstance {
            argument,
            actual,
            specializer,
        } => format!(
            "argument {} {}",
            argument + 1,
            exact_class_rejection_reason(module, actual, specializer)
        ),
    }
}

fn first_specializer_distance(
    module: &sem::Module,
    method: &sem::Method,
    actuals: &[sem::ClassId],
) -> Option<usize> {
    method
        .specializers
        .first()
        .copied()
        .flatten()
        .zip(actuals.first().copied())
        .and_then(|(specializer, actual)| module.inheritance_distance(actual, specializer))
}

fn specializer_distances(
    module: &sem::Module,
    method: &sem::Method,
    actuals: &[sem::ClassId],
) -> Vec<Option<usize>> {
    method
        .specializers
        .iter()
        .zip(actuals)
        .map(|(specializer, actual)| {
            specializer.and_then(|specializer| module.inheritance_distance(*actual, specializer))
        })
        .collect()
}

fn method_may_apply_to_static_kinds(
    module: &sem::Module,
    method: &sem::Method,
    static_kinds: &[sem::ValueKind],
) -> bool {
    method.specializers.len() == static_kinds.len()
        && method
            .specializers
            .iter()
            .zip(static_kinds)
            .all(|(specializer, kind)| {
                let Some(specializer) = specializer else {
                    return true;
                };
                match *kind {
                    sem::ValueKind::Any | sem::ValueKind::Error => true,
                    kind => module.value_kind_class(kind).is_some_and(|static_class| {
                        classes_overlap(module, static_class, *specializer)
                    }),
                }
            })
}

fn exact_class_rejection_reason(
    module: &sem::Module,
    actual: sem::ClassId,
    specializer: sem::ClassId,
) -> String {
    if module.is_same_or_subclass(specializer, actual) {
        format!(
            "requires subclass {} but exact class is {}",
            class_label(module, specializer),
            class_label(module, actual)
        )
    } else {
        format!("disjoint from exact class {}", class_label(module, actual))
    }
}

fn exact_tuple_rejection_reason(
    module: &sem::Module,
    actuals: &[sem::ClassId],
    method: &sem::Method,
) -> String {
    method
        .specializers
        .iter()
        .zip(actuals)
        .enumerate()
        .find_map(|(index, (specializer, actual))| {
            let specializer = (*specializer)?;
            (!module.is_same_or_subclass(*actual, specializer)).then(|| {
                format!(
                    "argument {} {}",
                    index + 1,
                    exact_class_rejection_reason(module, *actual, specializer)
                )
            })
        })
        .unwrap_or_else(|| "not applicable to exact argument tuple".into())
}

fn static_tuple_rejection_reason(
    module: &sem::Module,
    static_kinds: &[sem::ValueKind],
    method: &sem::Method,
) -> String {
    method
        .specializers
        .iter()
        .zip(static_kinds)
        .enumerate()
        .find_map(|(index, (specializer, kind))| {
            let specializer = (*specializer)?;
            match *kind {
                sem::ValueKind::Any | sem::ValueKind::Error => None,
                kind => {
                    let static_class = module.value_kind_class(kind)?;
                    (!classes_overlap(module, static_class, specializer)).then(|| {
                        format!(
                            "argument {} disjoint from static argument {}",
                            index + 1,
                            class_label(module, static_class)
                        )
                    })
                }
            }
        })
        .unwrap_or_else(|| "contract mismatch: dispatch argument tuple".into())
}

fn classes_overlap(module: &sem::Module, left: sem::ClassId, right: sem::ClassId) -> bool {
    module.is_same_or_subclass(left, right) || module.is_same_or_subclass(right, left)
}

fn dispatch_site_artifacts(
    artifacts: &SessionArtifacts,
    generic_id: sem::GenericFunctionId,
) -> Vec<ArtifactReport> {
    let mut reports = Vec::new();
    for method in artifacts
        .sem
        .methods
        .iter()
        .filter(|method| method.generic == generic_id)
    {
        push_artifact_report(
            &mut reports,
            artifacts,
            QueryArtifact::MethodLookup(method.definition),
        );
    }
    if let Some(generic) = artifacts.sem.generics.get(generic_id.0 as usize) {
        push_artifact_report(
            &mut reports,
            artifacts,
            QueryArtifact::GenericDispatcher(generic.definition),
        );
    }
    push_artifact_report(&mut reports, artifacts, QueryArtifact::LoweredModule);
    push_artifact_report(&mut reports, artifacts, QueryArtifact::TapeProgram);
    reports
}

fn push_artifact_report(
    reports: &mut Vec<ArtifactReport>,
    artifacts: &SessionArtifacts,
    artifact: QueryArtifact,
) {
    let key = artifacts.invalidation.key_for(artifact);
    if reports.iter().all(|report| report.key != key) {
        reports.push(ArtifactReport { artifact, key });
    }
}

fn dump_dispatch_sites(artifacts: &SessionArtifacts) -> String {
    let sites = artifacts.dispatch_sites();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "dispatch sites for {} ({})",
        artifacts.source.name, artifacts.source.id
    );
    if sites.is_empty() {
        out.push_str("  none\n");
        return out;
    }
    for site in &sites {
        let generic = generic_label(&artifacts.sem, site.generic);
        let static_kinds = sem_kind_vector_label(&artifacts.sem, &site.static_argument_kinds);
        let selected = site.selected_method.map_or_else(
            || "runtime".into(),
            |method| method_label(&artifacts.sem, method),
        );
        let _ = writeln!(
            out,
            "  {} key={} owner={} {} span={}..{} generic={} static-args={} runtime-needed={} selected={}",
            site.id,
            site.key,
            site.owner.kind.as_str(),
            site.owner.name,
            site.span.start,
            site.span.end,
            generic,
            static_kinds,
            site.runtime_needed,
            selected
        );
    }
    out
}

fn explain_dispatch_site(artifacts: &SessionArtifacts, site: &DispatchSiteRecord) -> String {
    let module = &artifacts.sem;
    let mut out = String::new();
    let _ = writeln!(out, "dispatch {}", site.id);
    let _ = writeln!(out, "  key: {}", site.key);
    let _ = writeln!(
        out,
        "  owner: {} {} definition={}",
        site.owner.kind.as_str(),
        site.owner.name,
        site.owner.definition
    );
    let _ = writeln!(
        out,
        "  source: {} span={}..{}",
        artifacts.source.name, site.span.start, site.span.end
    );
    let _ = writeln!(
        out,
        "  generic: {} definition={} contract={}",
        generic_label(module, site.generic),
        site.generic_definition,
        generic_contract_label(module, site.generic)
    );
    let static_kinds = sem_kind_vector_label(module, &site.static_argument_kinds);
    let exact_classes = exact_class_vector_label(module, &site.exact_static_classes);
    let selected = site.selected_method.map_or_else(
        || "runtime-needed".into(),
        |method| method_label(module, method),
    );
    let _ = writeln!(out, "  arguments:");
    let _ = writeln!(out, "    static-kinds: {static_kinds}");
    let _ = writeln!(out, "    exact-classes: {exact_classes}");
    let _ = writeln!(out, "    runtime-needed: {}", site.runtime_needed);
    let _ = writeln!(out, "    selected: {selected}");
    let _ = writeln!(out, "  applicable methods:");
    if site.applicable.is_empty() {
        out.push_str("    none\n");
    } else {
        for candidate in &site.applicable {
            let selection = if candidate.selected {
                "selected"
            } else if candidate.runtime_only {
                "runtime-only"
            } else {
                "shadowed"
            };
            let distance = distance_vector_label(&candidate.distances);
            let _ = writeln!(
                out,
                "    {} definition={} specializers={} distances={} {}",
                method_label(module, candidate.method),
                candidate.definition,
                sem_specializer_vector_label(module, &candidate.specializers),
                distance,
                selection
            );
        }
    }
    let _ = writeln!(out, "  rejected methods:");
    if site.rejected.is_empty() {
        out.push_str("    none\n");
    } else {
        for rejected in &site.rejected {
            let _ = writeln!(
                out,
                "    {} definition={} specializers={} reason={}",
                method_label(module, rejected.method),
                rejected.definition,
                sem_specializer_vector_label(module, &rejected.specializers),
                rejected.reason
            );
        }
    }
    let _ = writeln!(out, "  lowering:");
    let _ = writeln!(out, "    dispatcher: {}", site.lowered_dispatcher);
    let _ = writeln!(out, "    order:");
    for method in dispatch_methods(module, site.generic) {
        let _ = writeln!(
            out,
            "      {} specializers={} applies-to={}",
            method_label(module, method.id),
            sem_specializer_vector_label(module, &method.specializers),
            sem_dispatch_applicability_vector_label(module, &method.specializers)
        );
    }
    let _ = writeln!(out, "  related artifacts:");
    for report in &site.related_artifacts {
        let _ = writeln!(
            out,
            "    {} {}",
            report.key,
            artifact_label(report.artifact)
        );
    }
    out
}

fn exact_static_class(module: &sem::Module, expr: &sem::Expr) -> Option<sem::ClassId> {
    match expr {
        sem::Expr::ConstI64 { .. } => module.builtin_value_class(sem::BuiltinValueClass::Integer),
        sem::Expr::ConstF64 { .. } => {
            module.builtin_value_class(sem::BuiltinValueClass::DoubleFloat)
        }
        sem::Expr::MakeObject { class_id, .. } => Some(*class_id),
        sem::Expr::StringLiteral { class, .. }
        | sem::Expr::SymbolLiteral { class, .. }
        | sem::Expr::VectorLiteral { class, .. } => Some(*class),
        sem::Expr::ConstantRef { constant, .. } => module
            .constants
            .get(constant.0 as usize)
            .and_then(|constant| exact_static_class(module, &constant.value)),
        sem::Expr::CallableCall { result_kinds, .. } => match result_kinds.as_slice() {
            [kind] => module.value_kind_class(*kind),
            _ => None,
        },
        sem::Expr::Cast { value, .. } => exact_static_class(module, value),
        sem::Expr::If {
            then_branch,
            else_branch,
            ..
        } => {
            let then_class = exact_static_class(module, then_branch)?;
            let else_class = exact_static_class(module, else_branch)?;
            (then_class == else_class).then_some(then_class)
        }
        sem::Expr::Block { body, .. } => exact_static_class(module, &body.result),
        _ => None,
    }
}

fn class_label(module: &sem::Module, class_id: sem::ClassId) -> String {
    module.classes.get(class_id.0 as usize).map_or_else(
        || format!("c{}:<missing>", class_id.0),
        |class| format!("c{}:{}", class.id.0, class.name),
    )
}

fn generic_label(module: &sem::Module, generic_id: sem::GenericFunctionId) -> String {
    module.generics.get(generic_id.0 as usize).map_or_else(
        || format!("g{}:<missing>", generic_id.0),
        |generic| format!("g{}:{}", generic.id.0, generic.name),
    )
}

fn method_label(module: &sem::Module, method_id: sem::MethodId) -> String {
    module.methods.get(method_id.0 as usize).map_or_else(
        || format!("m{}:<missing>", method_id.0),
        |method| {
            let generic = module
                .generics
                .get(method.generic.0 as usize)
                .map_or("<?>", |generic| generic.name.as_str());
            format!(
                "m{}:{}{}",
                method.id.0,
                generic,
                sem_specializer_vector_label(module, &method.specializers)
            )
        },
    )
}

fn generic_contract_label(module: &sem::Module, generic_id: sem::GenericFunctionId) -> String {
    module.generics.get(generic_id.0 as usize).map_or_else(
        || "missing".into(),
        |generic| {
            let params = generic
                .params
                .iter()
                .zip(&generic.param_kinds)
                .map(|(name, kind)| match kind {
                    Some(kind) => format!("{name}:{}", sem_kind_label(module, *kind)),
                    None => format!("{name}:untyped"),
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "{}({params}) -> {}",
                generic.name,
                sem_kind_list_label(module, &generic.result_kinds)
            )
        },
    )
}

fn compile_file(file: &SourceFile, mode: RuntimeMode) -> SessionArtifacts {
    compile_file_with_grammar_registry(
        file,
        mode,
        GrammarFrontendRegistry::builtin(),
        typing::TypingConfig::default(),
    )
}

fn compile_file_with_grammar_registry(
    file: &SourceFile,
    mode: RuntimeMode,
    grammar_registry: GrammarFrontendRegistry,
    typing_config: typing::TypingConfig,
) -> SessionArtifacts {
    compile_file_with_registries(
        file,
        mode,
        grammar_registry,
        &sem::HostMetaRegistry::new(),
        typing_config,
    )
}

fn compile_file_with_host_registry(
    file: &SourceFile,
    mode: RuntimeMode,
    registry: &sem::HostMetaRegistry,
) -> SessionArtifacts {
    compile_file_with_registries(
        file,
        mode,
        GrammarFrontendRegistry::builtin(),
        registry,
        typing::TypingConfig::default(),
    )
}

pub(crate) fn compile_file_with_registries(
    file: &SourceFile,
    mode: RuntimeMode,
    grammar_registry: GrammarFrontendRegistry,
    registry: &sem::HostMetaRegistry,
    typing_config: typing::TypingConfig,
) -> SessionArtifacts {
    let mut timings = ArtifactTimingCollector::default();
    let (source, elapsed_nanos) = measure_elapsed(|| file.clone());
    timings.record(QueryArtifact::Source, elapsed_nanos);
    let (grammar_environment, elapsed_nanos) = measure_elapsed(|| file.grammar_environment.clone());
    timings.record(QueryArtifact::EnvironmentResolution, elapsed_nanos);
    let (grammar_parse, elapsed_nanos) = measure_elapsed(|| {
        grammar_frontend::parse_file_with_registry(
            file.id,
            &file.source,
            grammar_environment,
            &grammar_registry,
        )
    });
    timings.record(QueryArtifact::GrammarParse, elapsed_nanos);
    let (_, elapsed_nanos) = measure_elapsed(|| grammar_parse.grammar_stack.grammars.len());
    timings.record(QueryArtifact::GrammarStack, elapsed_nanos);
    let (syntax, elapsed_nanos) = measure_elapsed(|| grammar_parse.syntax.clone());
    timings.record(QueryArtifact::Parse, elapsed_nanos);
    let (ast, elapsed_nanos) =
        measure_elapsed(|| ast::lower_with_frontend_forms(&syntax, &grammar_parse.staged_regions));
    timings.record(QueryArtifact::Ast, elapsed_nanos);
    let (sem, elapsed_nanos) = measure_elapsed(|| sem::lower_with_host_registry(&ast, registry));
    timings.record(QueryArtifact::Bind, elapsed_nanos);
    let (typing, elapsed_nanos) = measure_elapsed(|| typing::analyze(&sem, typing_config));
    timings.record(QueryArtifact::Typing, elapsed_nanos);
    let (diagnostics, elapsed_nanos) = measure_elapsed(|| aggregate_diagnostics(&sem, &typing));
    timings.record(QueryArtifact::Diagnostics, elapsed_nanos);
    let invalidation = QueryInvalidation::for_module(&sem);

    let ((lowered, lower_error), elapsed_nanos) = measure_elapsed(|| match lower::lower(&sem) {
        Ok(module) => (Some(module), None),
        Err(error) => (None, Some(error)),
    });
    timings.record(QueryArtifact::LoweredModule, elapsed_nanos);
    let ((tape, tape_error), elapsed_nanos) = measure_elapsed(|| match &lowered {
        Some(lowered) => match tape::emit(lowered) {
            Ok(module) => (Some(module), None),
            Err(error) => (None, Some(error)),
        },
        None => (None, None),
    });
    timings.record(QueryArtifact::TapeProgram, elapsed_nanos);
    let ((runtime_result, runtime_trace), elapsed_nanos) = measure_elapsed(|| match mode {
        RuntimeMode::CompileOnly => (None, None),
        RuntimeMode::RunMain => (
            tape.as_ref().map(|tape| run_main(&sem, registry, tape)),
            None,
        ),
        RuntimeMode::RunMainTraced { detail } => tape.as_ref().map_or((None, None), |tape| {
            let (result, trace) = run_main_with_trace(&sem, registry, tape, Some(detail));
            (Some(result), trace)
        }),
    });
    timings.record(QueryArtifact::RuntimeResult, elapsed_nanos);

    let pristine_source = source.source.clone();
    let mut artifacts = SessionArtifacts {
        source,
        grammar_registry,
        host_registry: registry.clone(),
        syntax,
        grammar_parse,
        ast,
        sem,
        lowered,
        lower_error,
        tape,
        tape_error,
        diagnostics,
        typing,
        runtime_result,
        runtime_trace,
        mutations: Vec::new(),
        pristine_source,
        next_mutation_id: 0,
        invalidation,
        artifact_metrics: Vec::new(),
    };
    artifacts.artifact_metrics = build_artifact_metrics(&artifacts, &timings);
    artifacts
}

/// Aggregates every compile stage's diagnostics into the session's single
/// diagnostic list: semantic diagnostics first, then soft typing findings.
fn aggregate_diagnostics(sem: &sem::Module, typing: &typing::TypingArtifact) -> Vec<Diagnostic> {
    sem.diagnostics
        .iter()
        .chain(&typing.diagnostics)
        .cloned()
        .collect()
}

fn run_main(
    module: &sem::Module,
    host_registry: &sem::HostMetaRegistry,
    tape: &tape::TapeModule,
) -> RuntimeResult {
    run_main_with_trace(module, host_registry, tape, None).0
}

fn run_main_with_trace(
    module: &sem::Module,
    host_registry: &sem::HostMetaRegistry,
    tape: &tape::TapeModule,
    trace: Option<ExecutionTraceDetail>,
) -> (RuntimeResult, Option<ExecutionTrace>) {
    let mut runtime = InterlokoHost::new(tape.runtime.clone());
    match call_function(
        module,
        host_registry,
        tape,
        &mut runtime,
        "main",
        &[],
        trace,
        None,
    ) {
        Ok(outcome) => (RuntimeResult::Returned(outcome.value), outcome.trace),
        Err(error) => runtime_result_for_main_error(module, error),
    }
}

fn runtime_result_for_main_error(
    module: &sem::Module,
    error: FunctionCallError,
) -> (RuntimeResult, Option<ExecutionTrace>) {
    match error {
        FunctionCallError::TapeUnavailable | FunctionCallError::MissingFunction { .. } => {
            (RuntimeResult::MissingMain, None)
        }
        FunctionCallError::ArgumentCountMismatch { .. }
        | FunctionCallError::ArgumentTypeMismatch { .. }
        | FunctionCallError::ArgumentMaterializationFailed { .. } => {
            (RuntimeResult::MainHasParameters, None)
        }
        FunctionCallError::Trap { trap, trace } => (RuntimeResult::Trap { trap: *trap }, trace),
        FunctionCallError::UnhandledCondition { class, trace } => {
            (RuntimeResult::UnhandledCondition { class }, trace)
        }
        FunctionCallError::HostFunctionFailed {
            name,
            failure,
            trace,
        } => (RuntimeResult::HostFunctionFailed { name, failure }, trace),
        FunctionCallError::UnsupportedResult { declared, .. } => {
            (RuntimeResult::UnsupportedResult { declared }, None)
        }
        FunctionCallError::ResultMaterializationFailed { name } => {
            let declared = module
                .functions
                .iter()
                .find(|function| function.name == name)
                .map_or_else(
                    || "<unknown>".into(),
                    |function| sem_kind_list_label(module, &function.result_kinds),
                );
            (RuntimeResult::UnsupportedResult { declared }, None)
        }
        // `main` runs on a fresh host with the artifacts' own layout, so its
        // turn always opens and closes.
        FunctionCallError::TurnFailed { .. } | FunctionCallError::RuntimeLayoutChanged => (
            RuntimeResult::UnsupportedResult {
                declared: error.to_string(),
            },
            None,
        ),
    }
}

/// Runs one call as one turn of `runtime`: allocations go to its nursery,
/// and the turn is finished when the call returns or aborted when it fails.
#[expect(
    clippy::too_many_arguments,
    reason = "internal call plumbing shared by the public call entry points"
)]
fn call_function<'a>(
    module: &sem::Module,
    host_registry: &'a sem::HostMetaRegistry,
    tape: &tape::TapeModule,
    runtime: &mut InterlokoHost,
    function_name: &str,
    arguments: &[FunctionValue],
    trace: Option<ExecutionTraceDetail>,
    handler: Option<&'a mut dyn HostFunctionHandler>,
) -> Result<FunctionCallOutcome, FunctionCallError> {
    runtime
        .begin_turn()
        .map_err(|error| FunctionCallError::TurnFailed { error })?;
    let body = run_turn_body(
        module,
        host_registry,
        tape,
        &mut *runtime,
        function_name,
        arguments,
        trace,
        handler.map(|handler| -> &mut dyn HostFunctionHandler { handler }),
    );
    match body {
        Ok((value, trace, output)) => match runtime.finish_turn(&output, &mut []) {
            Ok(turn) => Ok(FunctionCallOutcome { value, trace, turn }),
            // A refused evacuation leaves the turn open; abort it so the
            // image is as the call found it and the next call can begin.
            Err(error) => {
                runtime
                    .abort_turn()
                    .map_err(|error| FunctionCallError::TurnFailed { error })?;
                Err(FunctionCallError::TurnFailed { error })
            }
        },
        Err(error) => {
            runtime
                .abort_turn()
                .map_err(|error| FunctionCallError::TurnFailed { error })?;
            Err(error)
        }
    }
}

/// Returns the call's materialized result, its trace, and the raw VM
/// results, which stay valid until the turn ends.
#[expect(
    clippy::too_many_arguments,
    reason = "internal call plumbing shared by the public call entry points"
)]
fn run_turn_body<'a>(
    module: &sem::Module,
    host_registry: &'a sem::HostMetaRegistry,
    tape: &tape::TapeModule,
    runtime: &'a mut InterlokoHost,
    function_name: &str,
    arguments: &[FunctionValue],
    trace: Option<ExecutionTraceDetail>,
    handler: Option<&'a mut dyn HostFunctionHandler>,
) -> Result<(FunctionValue, Option<ExecutionTrace>, Vec<Value>), FunctionCallError> {
    let function = module
        .functions
        .iter()
        .find(|function| function.name == function_name)
        .ok_or_else(|| FunctionCallError::MissingFunction {
            name: function_name.to_string(),
        })?;
    if function.param_kinds.len() != arguments.len() {
        return Err(FunctionCallError::ArgumentCountMismatch {
            name: function_name.to_string(),
            expected: function.param_kinds.len(),
            actual: arguments.len(),
        });
    }
    let entry = tape
        .functions
        .get(function.id.0 as usize)
        .copied()
        .ok_or_else(|| FunctionCallError::MissingFunction {
            name: function_name.to_string(),
        })?;

    let host = SharedFunctionCallHost::new(runtime, host_registry.functions(), handler);
    let mut runtime_arguments = Vec::with_capacity(arguments.len());
    for (index, (argument, expected)) in arguments
        .iter()
        .zip(function.param_kinds.iter().copied())
        .enumerate()
    {
        let value = match (argument, expected) {
            (FunctionValue::Integer(value), sem::ValueKind::I64) => Value::I64(*value),
            (FunctionValue::Float(value), sem::ValueKind::F64) => Value::F64(*value),
            (FunctionValue::Boolean(value), sem::ValueKind::Bool) => Value::Bool(*value),
            (FunctionValue::String(value), sem::ValueKind::String(_)) => host
                .make_string(value)
                .map_err(|_| FunctionCallError::ArgumentMaterializationFailed {
                    index,
                    kind: argument.kind(),
                })?,
            _ => {
                return Err(FunctionCallError::ArgumentTypeMismatch {
                    name: function_name.to_string(),
                    index,
                    expected: sem_kind_label(module, expected),
                    actual: argument.kind(),
                });
            }
        };
        runtime_arguments.push(value);
    }

    let mut vm = Vm::new(host.clone(), Limits::default());
    // A fresh VM has room for a unit, and the program imports no tags.
    let unit = vm
        .install(Arc::clone(&tape.program))
        .expect("a fresh VM installs a program without tag imports");
    let mut capture = trace.map(|detail| TraceCapture::new(&tape.program, detail));
    let mask = capture.as_ref().map_or(TraceMask::NONE, TraceCapture::mask);
    let sink = capture.as_mut().map(TraceCapture::sink);
    let entry = FuncRef::new(unit, entry);
    let output = match vm.run(entry, &runtime_arguments, mask, sink) {
        Ok(output) => output,
        Err(trap) => {
            if let Some((name, failure)) = host.failure() {
                return Err(FunctionCallError::HostFunctionFailed {
                    name,
                    failure,
                    trace: capture.map(TraceCapture::finish),
                });
            }
            if let Some(ControlFault::UnhandledCondition { class }) = host.control_fault() {
                let class = tape
                    .runtime
                    .class(class)
                    .map_or_else(|| format!("class#{}", class.0), |class| class.name.clone());
                return Err(FunctionCallError::UnhandledCondition {
                    class,
                    trace: capture.map(TraceCapture::finish),
                });
            }
            return Err(FunctionCallError::Trap {
                trap: alloc::boxed::Box::new(trap),
                trace: capture.map(TraceCapture::finish),
            });
        }
    };

    let value = match (function.result_kinds.as_slice(), output.as_slice()) {
        ([sem::ValueKind::I64], [Value::I64(value)]) => FunctionValue::Integer(*value),
        ([sem::ValueKind::F64], [Value::F64(value)]) => FunctionValue::Float(*value),
        ([sem::ValueKind::Bool], [Value::Bool(value)]) => FunctionValue::Boolean(*value),
        ([sem::ValueKind::String(_)], [value]) => {
            FunctionValue::String(host.string_text(value).map_err(|_| {
                FunctionCallError::ResultMaterializationFailed {
                    name: function_name.to_string(),
                }
            })?)
        }
        _ => {
            return Err(FunctionCallError::UnsupportedResult {
                name: function_name.to_string(),
                declared: sem_kind_list_label(module, &function.result_kinds),
            });
        }
    };

    drop(vm);
    Ok((value, capture.map(TraceCapture::finish), output))
}

#[derive(Clone)]
struct SharedFunctionCallHost<'a>(Rc<RefCell<FunctionCallHost<'a>>>);

impl<'a> SharedFunctionCallHost<'a> {
    fn new(
        runtime: &'a mut InterlokoHost,
        functions: &'a [sem::HostFunction],
        handler: Option<&'a mut dyn HostFunctionHandler>,
    ) -> Self {
        Self(Rc::new(RefCell::new(FunctionCallHost {
            runtime,
            functions,
            handler,
            failure: None,
        })))
    }

    fn make_string(&self, value: &str) -> Result<Value, runtime::RuntimeError> {
        self.0.borrow_mut().runtime.make_string(value)
    }

    fn string_text(&self, value: &Value) -> Result<String, runtime::RuntimeError> {
        self.0
            .borrow()
            .runtime
            .string_text(value)
            .map(ToString::to_string)
    }

    fn failure(&self) -> Option<(String, HostFunctionFailure)> {
        self.0.borrow().failure.clone()
    }

    fn control_fault(&self) -> Option<ControlFault> {
        self.0.borrow().runtime.control().fault()
    }
}

impl Host for SharedFunctionCallHost<'_> {
    fn call(
        &mut self,
        symbol: &str,
        sig_hash: SigHash,
        args: &[ValueRef<'_>],
        rets: &mut [Value],
        context: HostContext<'_, '_>,
    ) -> Result<u64, HostError> {
        self.0
            .borrow_mut()
            .call(symbol, sig_hash, args, rets, context)
    }
}

struct FunctionCallHost<'a> {
    runtime: &'a mut InterlokoHost,
    functions: &'a [sem::HostFunction],
    handler: Option<&'a mut dyn HostFunctionHandler>,
    failure: Option<(String, HostFunctionFailure)>,
}

impl FunctionCallHost<'_> {
    fn fail(&mut self, name: &str, message: impl Into<String>) -> HostError {
        self.failure = Some((name.to_string(), HostFunctionFailure::new(message)));
        HostError::Failed
    }

    fn host_argument(
        &self,
        kind: sem::HostValueKind,
        value: ValueRef<'_>,
    ) -> Result<FunctionValue, HostFunctionFailure> {
        match (kind, value) {
            (sem::HostValueKind::Integer, ValueRef::I64(value)) => {
                Ok(FunctionValue::Integer(value))
            }
            (sem::HostValueKind::Float, ValueRef::F64(value)) => Ok(FunctionValue::Float(value)),
            (sem::HostValueKind::Boolean, ValueRef::Bool(value)) => {
                Ok(FunctionValue::Boolean(value))
            }
            (sem::HostValueKind::String, ValueRef::Obj(value)) => self
                .runtime
                .string_text(&Value::Obj(value))
                .map(|value| FunctionValue::String(value.to_string()))
                .map_err(|_| {
                    HostFunctionFailure::new("string argument is not an Interloko string")
                }),
            _ => Err(HostFunctionFailure::new(
                "runtime argument does not match the declared scalar kind",
            )),
        }
    }

    fn host_result(
        &mut self,
        kind: sem::HostValueKind,
        value: FunctionValue,
    ) -> Result<Value, HostFunctionFailure> {
        match (kind, value) {
            (sem::HostValueKind::Integer, FunctionValue::Integer(value)) => Ok(Value::I64(value)),
            (sem::HostValueKind::Float, FunctionValue::Float(value)) => Ok(Value::F64(value)),
            (sem::HostValueKind::Boolean, FunctionValue::Boolean(value)) => Ok(Value::Bool(value)),
            (sem::HostValueKind::String, FunctionValue::String(value)) => self
                .runtime
                .make_string(value)
                .map_err(|_| HostFunctionFailure::new("string result could not enter the runtime")),
            (expected, actual) => Err(HostFunctionFailure::new(format!(
                "handler returned {}, expected {}",
                actual.kind(),
                host_value_kind_label(expected)
            ))),
        }
    }
}

impl Host for FunctionCallHost<'_> {
    fn call(
        &mut self,
        symbol: &str,
        sig_hash: SigHash,
        args: &[ValueRef<'_>],
        rets: &mut [Value],
        context: HostContext<'_, '_>,
    ) -> Result<u64, HostError> {
        let Some(function) = self
            .functions
            .iter()
            .find(|function| function.name == symbol)
            .cloned()
        else {
            return self.runtime.call(symbol, sig_hash, args, rets, context);
        };
        if self.failure.is_some() {
            return Err(HostError::Failed);
        }
        if args.len() != function.param_kinds.len() || rets.len() != 1 {
            return Err(self.fail(
                symbol,
                "runtime host-call arity disagrees with its declaration",
            ));
        }
        let mut arguments = Vec::with_capacity(args.len());
        for (kind, value) in function
            .param_kinds
            .iter()
            .copied()
            .zip(args.iter().copied())
        {
            match self.host_argument(kind, value) {
                Ok(value) => arguments.push(value),
                Err(failure) => {
                    self.failure = Some((symbol.to_string(), failure));
                    return Err(HostError::Failed);
                }
            }
        }
        let Some(handler) = self.handler.as_deref_mut() else {
            return Err(self.fail(symbol, "no host-function handler was provided"));
        };
        let result = match handler.call(&function, &arguments) {
            Ok(result) => result,
            Err(failure) => {
                self.failure = Some((symbol.to_string(), failure));
                return Err(HostError::Failed);
            }
        };
        match self.host_result(function.result_kind, result) {
            Ok(result) => rets[0] = result,
            Err(failure) => {
                self.failure = Some((symbol.to_string(), failure));
                return Err(HostError::Failed);
            }
        }
        Ok(0)
    }
}

const fn host_value_kind_label(kind: sem::HostValueKind) -> &'static str {
    match kind {
        sem::HostValueKind::Integer => "integer",
        sem::HostValueKind::Float => "float",
        sem::HostValueKind::Boolean => "boolean",
        sem::HostValueKind::String => "string",
    }
}

fn dump_syntax(tree: &SyntaxTree) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "syntax {} ({})", tree.source_file, tree.source.len());
    dump_cst_node(&mut out, tree, &tree.root, 0);
    out
}

fn collect_top_level_forms(tree: &SyntaxTree) -> Vec<TopLevelFormSpan> {
    tree.root
        .children
        .iter()
        .filter_map(|child| match child {
            CstElement::Node(node) => top_level_form_kind(node.kind).map(|kind| (kind, node)),
            CstElement::Token(_) => None,
        })
        .enumerate()
        .map(|(index, (kind, node))| TopLevelFormSpan {
            index,
            kind,
            span: node.span,
        })
        .collect()
}

fn top_level_form_kind(kind: crate::syntax::CstKind) -> Option<TopLevelFormKind> {
    match kind {
        crate::syntax::CstKind::Class => Some(TopLevelFormKind::Class),
        crate::syntax::CstKind::Generic => Some(TopLevelFormKind::Generic),
        crate::syntax::CstKind::Method => Some(TopLevelFormKind::Method),
        crate::syntax::CstKind::Function => Some(TopLevelFormKind::Function),
        crate::syntax::CstKind::Constant => Some(TopLevelFormKind::Constant),
        _ => None,
    }
}

fn collect_source_forms(artifacts: &SessionArtifacts) -> Vec<SourceFormProvenance> {
    let objects = artifacts.environment_objects();
    let mut forms = collect_base_source_forms(artifacts, &objects);
    forms.extend(collect_staged_source_forms(artifacts, &objects));
    forms.sort_by_key(|form| {
        (
            form.span.start,
            form.span.end,
            source_form_origin_order(&form.origin),
            form.id.clone(),
        )
    });
    forms
}

fn collect_base_source_forms(
    artifacts: &SessionArtifacts,
    objects: &[EnvironmentObject],
) -> Vec<SourceFormProvenance> {
    artifacts
        .top_level_forms()
        .into_iter()
        .map(|form| SourceFormProvenance {
            id: format!("form:f{}", form.index),
            kind: source_form_kind_from_top_level(form.kind),
            title: source_form_kind_from_top_level(form.kind)
                .as_str()
                .to_string(),
            span: form.span,
            origin: SourceFormOrigin::BaseCst,
            activation: activation_for_span(&artifacts.grammar_parse, form.span)
                .map(|activation| activation.id),
            grammar: activation_for_span(&artifacts.grammar_parse, form.span)
                .map(|activation| activation.grammar),
            produced_object_ids: produced_object_ids_for_span(objects, form.span),
            doc_comments: leading_doc_comments_for_span(&artifacts.syntax, form.span),
            rule_applications: grammar_rule_applications_touching_span(
                &artifacts.grammar_parse,
                form.span,
            ),
            frontend_form: None,
        })
        .collect()
}

fn collect_staged_source_forms(
    artifacts: &SessionArtifacts,
    objects: &[EnvironmentObject],
) -> Vec<SourceFormProvenance> {
    let mut forms = Vec::new();
    for region in &artifacts.grammar_parse.staged_regions {
        for (index, form) in region.forms.iter().enumerate() {
            forms.push(SourceFormProvenance {
                id: format!("staged-form:{}/f{}", region.object_slug, index),
                kind: source_form_kind_from_frontend(form),
                title: source_form_title_from_frontend(form),
                span: form.source_span,
                origin: SourceFormOrigin::StagedGrammar {
                    region: region.object_slug.clone(),
                    form_index: index,
                },
                activation: Some(region.activation),
                grammar: Some(region.grammar),
                produced_object_ids: produced_object_ids_for_span(objects, form.source_span),
                doc_comments: Vec::new(),
                rule_applications: grammar_rule_applications_touching_list(
                    &region.rule_applications,
                    form.source_span,
                ),
                frontend_form: Some(form.clone()),
            });
        }
    }
    forms
}

fn source_form_kind_from_top_level(kind: TopLevelFormKind) -> SourceFormKind {
    match kind {
        TopLevelFormKind::Class => SourceFormKind::Class,
        TopLevelFormKind::Generic => SourceFormKind::Generic,
        TopLevelFormKind::Method => SourceFormKind::Method,
        TopLevelFormKind::Function => SourceFormKind::Function,
        TopLevelFormKind::Constant => SourceFormKind::Constant,
    }
}

fn source_form_kind_from_frontend(form: &FrontendForm) -> SourceFormKind {
    match form.kind {
        FrontendFormKind::Class(_) => SourceFormKind::FrontendClass,
        FrontendFormKind::Function(_) => SourceFormKind::FrontendFunction,
    }
}

fn source_form_title_from_frontend(form: &FrontendForm) -> String {
    match &form.kind {
        FrontendFormKind::Class(class) => format!("frontend class {}", class.name),
        FrontendFormKind::Function(function) => format!("frontend function {}", function.name),
    }
}

fn leading_doc_comments_for_span(tree: &SyntaxTree, span: Span) -> Vec<String> {
    let mut index = tree
        .tokens
        .partition_point(|token| token.span.start < span.start);
    let mut docs = Vec::new();
    let mut saw_doc = false;

    while index > 0 {
        index -= 1;
        let token = tree.tokens[index];
        let text = token.text(&tree.source);
        match token.kind {
            TokenKind::DocComment => {
                if !is_line_leading_comment(&tree.source, token.span.start) {
                    break;
                }
                saw_doc = true;
                docs.push(normalize_doc_comment_text(text));
            }
            TokenKind::Whitespace => {
                if whitespace_has_blank_line(text) {
                    break;
                }
            }
            TokenKind::Comment => break,
            kind if kind.is_trivia() => {
                if saw_doc {
                    break;
                }
            }
            _ => break,
        }
    }

    docs.reverse();
    docs
}

fn normalize_doc_comment_text(text: &str) -> String {
    let text = text.strip_suffix('\r').unwrap_or(text);
    let without_marker = text.strip_prefix("///").unwrap_or(text);
    without_marker
        .strip_prefix(' ')
        .unwrap_or(without_marker)
        .to_string()
}

fn whitespace_has_blank_line(text: &str) -> bool {
    text.bytes().filter(|byte| *byte == b'\n').count() >= 2
}

fn is_line_leading_comment(source: &str, start: usize) -> bool {
    let line_start = source[..start].rfind('\n').map_or(0, |index| index + 1);
    source[line_start..start].chars().all(char::is_whitespace)
}

fn source_form_origin_order(origin: &SourceFormOrigin) -> usize {
    match origin {
        SourceFormOrigin::BaseCst => 0,
        SourceFormOrigin::StagedGrammar { .. } => 1,
    }
}

fn activation_for_span(report: &GrammarParseReport, span: Span) -> Option<&GrammarActivation> {
    report
        .activations
        .iter()
        .filter(|activation| span_contains(activation.span, span))
        .min_by_key(|activation| {
            (
                activation.span.end.saturating_sub(activation.span.start),
                activation.span.start,
            )
        })
}

fn produced_object_ids_for_span(objects: &[EnvironmentObject], span: Span) -> Vec<String> {
    let mut ids = objects
        .iter()
        .filter(|object| {
            object.origin == EnvironmentOrigin::SourceAuthored
                && object
                    .source
                    .span
                    .is_some_and(|object_span| span_contains(span, object_span))
        })
        .map(|object| object.id.to_string())
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

fn collect_syntax_nodes_containing_span(
    node: &CstNode,
    span: Span,
    depth: usize,
    out: &mut Vec<SyntaxNodeSummary>,
) {
    if !span_contains(node.span, span) {
        return;
    }
    out.push(SyntaxNodeSummary {
        id: node.id,
        kind: node.kind,
        span: node.span,
        depth,
    });
    for child in &node.children {
        if let CstElement::Node(child) = child {
            collect_syntax_nodes_containing_span(child, span, depth + 1, out);
        }
    }
}

fn span_contains(outer: Span, inner: Span) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

fn span_intersects_query(item: Span, query: Span) -> bool {
    if query.is_empty() {
        item.start <= query.start && query.start < item.end
    } else {
        item.start < query.end && query.start < item.end
    }
}

fn span_touches_query(item: Span, query: Span) -> bool {
    if query.is_empty() {
        item.start <= query.start && query.start <= item.end
    } else if item.is_empty() {
        query.start <= item.start && item.start <= query.end
    } else {
        item.start < query.end && query.start < item.end
    }
}

fn grammar_rule_applications_touching_span(
    report: &GrammarParseReport,
    span: Span,
) -> Vec<GrammarRuleApplication> {
    grammar_rule_applications_touching_list(&report.provenance.applications, span)
}

fn grammar_rule_applications_touching_list(
    source: &[GrammarRuleApplication],
    span: Span,
) -> Vec<GrammarRuleApplication> {
    let mut applications = source
        .iter()
        .filter(|application| span_touches_query(application.span, span))
        .cloned()
        .collect::<Vec<_>>();
    applications.sort_by_key(|application| {
        (
            application.span.end.saturating_sub(application.span.start),
            application.span.start,
        )
    });
    applications
}

fn dump_cst_node(out: &mut String, tree: &SyntaxTree, node: &CstNode, indent: usize) {
    indent_line(out, indent);
    let _ = writeln!(
        out,
        "{:?} {} span={}..{}",
        node.kind, node.id, node.span.start, node.span.end
    );
    for child in &node.children {
        match child {
            CstElement::Node(child) => dump_cst_node(out, tree, child, indent + 2),
            CstElement::Token(index) => {
                let token = tree.tokens[*index];
                if token.kind.is_trivia() {
                    continue;
                }
                indent_line(out, indent + 2);
                let _ = writeln!(
                    out,
                    "token {:?} {:?} span={}..{}",
                    token.kind,
                    token.text(&tree.source),
                    token.span.start,
                    token.span.end
                );
            }
        }
    }
}

fn dump_ast(module: &ast::Module) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "ast {}", module.source_file);
    for class in &module.classes {
        let superclass = class.superclass.as_deref().unwrap_or("<object>");
        let _ = writeln!(
            out,
            "class {} {} {} superclass={superclass}",
            class.id, class.definition, class.name
        );
        for slot in &class.slots {
            let type_name = slot.type_name.as_deref().unwrap_or("<object>");
            let init_keyword = slot.init_keyword.as_deref().unwrap_or(&slot.name);
            let required = if slot.required_init_keyword {
                " required"
            } else {
                ""
            };
            let _ = writeln!(
                out,
                "  {} {} {} {} :: {} init-keyword: {}:{} allocation={} mutability={}",
                ast_slot_mutability_prefix(slot.mutability),
                slot.id,
                slot.definition,
                slot.name,
                type_name,
                init_keyword,
                required,
                ast_slot_allocation_label(slot.allocation),
                ast_slot_mutability_label(slot.mutability)
            );
            if let Some(init_value) = &slot.init_value {
                indent_line(&mut out, 4);
                let _ = writeln!(out, "init-value");
                dump_ast_expr(&mut out, init_value, 6);
            }
        }
    }
    for generic in &module.generics {
        let _ = writeln!(
            out,
            "generic {} {} {}({})",
            generic.id,
            generic.definition,
            generic.name,
            generic
                .params
                .iter()
                .map(dump_generic_param)
                .collect::<Vec<_>>()
                .join(", ")
        );
        if let Some(return_type) = &generic.return_type {
            indent_line(&mut out, 2);
            let _ = writeln!(out, "returns {}", dump_result_annotation(return_type));
        }
    }
    for method in &module.methods {
        let params = method
            .params
            .iter()
            .map(|param| format!("{} :: {}", param.name, param.class_name))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            out,
            "method {} {} {}({params})",
            method.id, method.definition, method.name
        );
        if let Some(return_type) = &method.return_type {
            indent_line(&mut out, 2);
            let _ = writeln!(out, "returns {}", dump_result_annotation(return_type));
        }
        dump_ast_block(&mut out, &method.body, 2);
    }
    for function in &module.functions {
        let _ = writeln!(
            out,
            "function {} {} {}({})",
            function.id,
            function.definition,
            function.name,
            function
                .params
                .iter()
                .map(dump_function_param)
                .collect::<Vec<_>>()
                .join(", ")
        );
        if let Some(return_type) = &function.return_type {
            indent_line(&mut out, 2);
            let _ = writeln!(out, "returns {}", dump_result_annotation(return_type));
        }
        dump_ast_block(&mut out, &function.body, 2);
    }
    out
}

fn dump_generic_param(param: &ast::GenericParam) -> String {
    param.type_annotation.as_ref().map_or_else(
        || param.name.clone(),
        |annotation| format!("{} :: {}", param.name, dump_type_annotation(annotation)),
    )
}

fn dump_function_param(param: &ast::FunctionParam) -> String {
    param.type_annotation.as_ref().map_or_else(
        || param.name.clone(),
        |annotation| format!("{} :: {}", param.name, dump_type_annotation(annotation)),
    )
}

fn dump_type_annotation(annotation: &ast::TypeAnnotation) -> String {
    if let Some(callable) = &annotation.callable {
        let params = callable
            .params
            .iter()
            .map(dump_type_annotation)
            .collect::<Vec<_>>()
            .join(", ");
        return format!(
            "callable({params}) => {}",
            dump_result_annotation(&callable.results)
        );
    }
    if annotation.class_object {
        format!("class {}", annotation.name)
    } else {
        annotation.name.clone()
    }
}

fn dump_result_annotation(annotation: &ast::ResultAnnotation) -> String {
    match annotation.values.as_slice() {
        [] => "()".into(),
        [value] => dump_type_annotation(value),
        values => {
            let values = values
                .iter()
                .map(dump_type_annotation)
                .collect::<Vec<_>>()
                .join(", ");
            format!("({values})")
        }
    }
}

fn ast_slot_allocation_label(allocation: ast::SlotAllocation) -> &'static str {
    match allocation {
        ast::SlotAllocation::Instance => "instance",
    }
}

fn ast_slot_mutability_prefix(mutability: ast::SlotMutability) -> &'static str {
    match mutability {
        ast::SlotMutability::Mutable => "slot",
        ast::SlotMutability::Constant => "constant slot",
    }
}

fn ast_slot_mutability_label(mutability: ast::SlotMutability) -> &'static str {
    match mutability {
        ast::SlotMutability::Mutable => "mutable",
        ast::SlotMutability::Constant => "constant",
    }
}

fn dump_ast_block(out: &mut String, block: &ast::Block, indent: usize) {
    indent_line(out, indent);
    let _ = writeln!(out, "block {}", block.id);
    for binding in &block.lets {
        indent_line(out, indent + 2);
        let _ = writeln!(
            out,
            "let {} {} {}",
            binding.id, binding.definition, binding.name
        );
        dump_ast_expr(out, &binding.value, indent + 4);
    }
    indent_line(out, indent + 2);
    let _ = writeln!(out, "result");
    dump_ast_expr(out, &block.result, indent + 4);
}

fn dump_ast_expr(out: &mut String, expr: &ast::Expr, indent: usize) {
    indent_line(out, indent);
    match expr {
        ast::Expr::Number { id, value, .. } => {
            let _ = writeln!(out, "{id} number {value}");
        }
        ast::Expr::Float { id, value, .. } => {
            let _ = writeln!(out, "{id} float {value}");
        }
        ast::Expr::Bool { id, value, .. } => {
            let _ = writeln!(out, "{id} bool {value}");
        }
        ast::Expr::String { id, value, .. } => {
            let _ = writeln!(out, "{id} string {:?}", value);
        }
        ast::Expr::Symbol { id, name, .. } => {
            let _ = writeln!(out, "{id} symbol #{name}");
        }
        ast::Expr::Vector { id, elements, .. } => {
            let _ = writeln!(out, "{id} vector elements={}", elements.len());
            for element in elements {
                dump_ast_expr(out, element, indent + 2);
            }
        }
        ast::Expr::Function {
            id,
            params,
            return_type,
            body,
            ..
        } => {
            let return_type = return_type
                .as_ref()
                .map_or_else(|| "inferred".into(), dump_result_annotation);
            let _ = writeln!(
                out,
                "{id} function params={} return={return_type}",
                params.len()
            );
            dump_ast_block(out, body, indent + 2);
        }
        ast::Expr::Name { id, name, .. } => {
            let _ = writeln!(out, "{id} name {name}");
        }
        ast::Expr::ClassName { id, class_name, .. } => {
            let _ = writeln!(out, "{id} class-name {class_name}");
        }
        ast::Expr::InstanceOf {
            id,
            value,
            class_name,
            ..
        } => {
            let _ = writeln!(out, "{id} instance? {class_name}");
            dump_ast_expr(out, value, indent + 2);
        }
        ast::Expr::Cast {
            id,
            class_name,
            value,
            ..
        } => {
            let _ = writeln!(out, "{id} as {class_name}");
            dump_ast_expr(out, value, indent + 2);
        }
        ast::Expr::Call {
            id, callee, args, ..
        } => {
            let _ = writeln!(out, "{id} call {callee}");
            for arg in args {
                dump_ast_expr(out, arg, indent + 2);
            }
        }
        ast::Expr::Signal { id, condition, .. } => {
            let _ = writeln!(out, "{id} signal");
            dump_ast_expr(out, condition, indent + 2);
        }
        ast::Expr::MakeObject {
            id, class, fields, ..
        } => {
            let _ = writeln!(out, "{id} make");
            dump_ast_expr(out, class, indent + 2);
            for field in fields {
                indent_line(out, indent + 2);
                let _ = writeln!(out, "field {} {}", field.id, field.name);
                dump_ast_expr(out, &field.value, indent + 4);
            }
        }
        ast::Expr::SlotRead {
            id, receiver, slot, ..
        } => {
            let _ = writeln!(out, "{id} slot {slot}");
            dump_ast_expr(out, receiver, indent + 2);
        }
        ast::Expr::SlotAssign {
            id,
            receiver,
            slot,
            value,
            ..
        } => {
            let _ = writeln!(out, "{id} slot-assign {slot}");
            dump_ast_expr(out, receiver, indent + 2);
            dump_ast_expr(out, value, indent + 2);
        }
        ast::Expr::Binary {
            id, op, lhs, rhs, ..
        } => {
            let _ = writeln!(out, "{id} binary {op:?}");
            dump_ast_expr(out, lhs, indent + 2);
            dump_ast_expr(out, rhs, indent + 2);
        }
        ast::Expr::UnaryMinus { id, expr, .. } => {
            let _ = writeln!(out, "{id} unary Minus");
            dump_ast_expr(out, expr, indent + 2);
        }
        ast::Expr::Compare {
            id, op, lhs, rhs, ..
        } => {
            let _ = writeln!(out, "{id} compare {op:?}");
            dump_ast_expr(out, lhs, indent + 2);
            dump_ast_expr(out, rhs, indent + 2);
        }
        ast::Expr::If {
            id,
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            let _ = writeln!(out, "{id} if");
            indent_line(out, indent + 2);
            let _ = writeln!(out, "condition");
            dump_ast_expr(out, condition, indent + 4);
            indent_line(out, indent + 2);
            let _ = writeln!(out, "then");
            dump_ast_expr(out, then_branch, indent + 4);
            indent_line(out, indent + 2);
            let _ = writeln!(out, "else");
            dump_ast_expr(out, else_branch, indent + 4);
        }
        ast::Expr::Handle {
            id, body, restart, ..
        } => {
            let _ = writeln!(out, "{id} handle");
            indent_line(out, indent + 2);
            let _ = writeln!(out, "body");
            dump_ast_expr(out, body, indent + 4);
            indent_line(out, indent + 2);
            if let Some(condition_name) = &restart.condition_name {
                let _ = writeln!(
                    out,
                    "restart {} {condition_name} :: {}",
                    restart.id, restart.class_name
                );
            } else {
                let _ = writeln!(out, "restart {} {}", restart.id, restart.class_name);
            }
            dump_ast_expr(out, &restart.body, indent + 4);
        }
        ast::Expr::Block {
            id, body, cleanup, ..
        } => {
            let _ = writeln!(out, "{id} block-expr");
            indent_line(out, indent + 2);
            let _ = writeln!(out, "body");
            dump_ast_block(out, body, indent + 4);
            if let Some(cleanup) = cleanup {
                indent_line(out, indent + 2);
                let _ = writeln!(out, "cleanup");
                dump_ast_block(out, cleanup, indent + 4);
            }
        }
        ast::Expr::Error { id, .. } => {
            let _ = writeln!(out, "{id} error");
        }
    }
}

fn dump_sem(module: &sem::Module) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "sem {}", module.source_file);
    for class in &module.classes {
        let superclass = sem_superclass_label(module, class.superclass);
        let superclass_chain = sem_superclass_chain_label(module, class.id);
        let _ = writeln!(
            out,
            "class c{} {} {} origin={} superclass={superclass} chain={superclass_chain} finalized={} direct-slots={} effective-slots={} init-plan={}",
            class.id.0,
            class.definition,
            class.name,
            sem_class_origin_label(class.origin),
            class.finalized,
            class.direct_slots.len(),
            class.effective_slots.len(),
            sem_initialization_plan_summary(module, class)
        );
        for slot in &class.direct_slots {
            let _ = writeln!(
                out,
                "  direct slot s{} {} {} :: {} origin={} accessor {} init-keyword: {}:{} init-value={} allocation={} mutability={} runtime={}",
                slot.id.0,
                slot.definition,
                slot.name,
                slot.type_name,
                sem_slot_origin_label(slot.origin),
                slot.accessor.definition,
                slot.init_keyword,
                if slot.required_init_keyword {
                    " required"
                } else {
                    ""
                },
                sem_slot_init_value_label(slot),
                sem_slot_allocation_label(slot.allocation),
                sem_slot_mutability_label(slot.mutability),
                sem_runtime_repr(slot.kind)
            );
        }
        for slot in &class.effective_slots {
            let source = if slot.source_class == class.id {
                "direct"
            } else {
                "inherited"
            };
            let _ = writeln!(
                out,
                "  effective slot s{} {} {} :: {} source={} c{}.s{} origin={} accessor {} reads c{}.s{} init-keyword: {}:{} init-value={} allocation={} mutability={} runtime={}",
                slot.id.0,
                slot.definition,
                slot.name,
                slot.type_name,
                source,
                slot.source_class.0,
                slot.source_slot.0,
                sem_slot_origin_label(slot.origin),
                slot.accessor.definition,
                slot.accessor.class.0,
                slot.accessor.slot.0,
                slot.init_keyword,
                if slot.required_init_keyword {
                    " required"
                } else {
                    ""
                },
                sem_effective_slot_init_value_label(slot),
                sem_slot_allocation_label(slot.allocation),
                sem_slot_mutability_label(slot.mutability),
                sem_runtime_repr(slot.kind)
            );
        }
        let _ = writeln!(out, "  initialization plan");
        for step in &class.initialization_plan.steps {
            let _ = writeln!(
                out,
                "    step s{} {}",
                step.slot.0,
                sem_initialization_step_detail(module, class, step)
            );
        }
    }
    for function in &module.host_functions {
        let _ = writeln!(
            out,
            "host-function h{} {}({}) result={}",
            function.id.0,
            function.name,
            sem_kind_list_label(module, &function.param_kinds),
            sem_kind_label(module, function.result_kind)
        );
    }
    for generic in &module.generics {
        let params = generic
            .params
            .iter()
            .zip(&generic.param_kinds)
            .map(|(name, kind)| match kind {
                Some(kind) => format!("{name}:{}", sem_kind_label(module, *kind)),
                None => format!("{name}:untyped"),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let return_annotation = generic.return_annotation.as_ref().map_or_else(
            || "inferred".to_string(),
            |kinds| sem_kind_list_label(module, kinds),
        );
        let _ = writeln!(
            out,
            "generic g{} {} {}({params}) result={} return={return_annotation} dispatch-params={} origin={}",
            generic.id.0,
            generic.definition,
            generic.name,
            sem_kind_list_label(module, &generic.result_kinds),
            dispatch_params_label(&generic.dispatch_params),
            sem_generic_origin_label(generic.origin)
        );
    }
    for method in &module.methods {
        let return_annotation = method.return_annotation.as_ref().map_or_else(
            || "inferred".to_string(),
            |kinds| sem_kind_list_label(module, kinds),
        );
        let _ = writeln!(
            out,
            "method m{} {} generic=g{} specializers={} result={} return={return_annotation} origin={}",
            method.id.0,
            method.definition,
            method.generic.0,
            sem_specializer_vector_label(module, &method.specializers),
            sem_kind_list_label(module, &method.result_kinds),
            sem_method_origin_label(method.origin)
        );
        dump_sem_locals(module, &mut out, &method.locals, 2);
        dump_sem_block(&mut out, &method.body, 2);
    }
    for constant in &module.constants {
        let _ = writeln!(
            out,
            "constant k{} {} {} result={}",
            constant.id.0,
            constant.definition,
            constant.name,
            sem_kind_label(module, constant.result_kind)
        );
        dump_sem_expr(&mut out, &constant.value, 2);
    }
    for closure in &module.closures {
        let params = closure
            .params
            .iter()
            .zip(&closure.param_kinds)
            .map(|(local, kind)| {
                let name = closure
                    .locals
                    .get(local.0 as usize)
                    .map_or("?", |local| local.name.as_str());
                format!("{name}:{}", sem_kind_label(module, *kind))
            })
            .collect::<Vec<_>>()
            .join(", ");
        let return_annotation = closure.return_annotation.as_ref().map_or_else(
            || "inferred".to_string(),
            |kinds| sem_kind_list_label(module, kinds),
        );
        let _ = writeln!(
            out,
            "closure c{} ({params}) captures={} result={} return={return_annotation}",
            closure.id.0,
            closure.captures.len(),
            sem_kind_list_label(module, &closure.result_kinds)
        );
        for capture in &closure.captures {
            let _ = writeln!(
                out,
                "  capture c{} l{} {} {}",
                capture.id.0,
                capture.source.0,
                capture.name,
                sem_kind_label(module, capture.kind)
            );
        }
        dump_sem_locals(module, &mut out, &closure.locals, 2);
        dump_sem_block(&mut out, &closure.body, 2);
    }
    for function in &module.functions {
        let params = function
            .params
            .iter()
            .zip(&function.param_kinds)
            .map(|(local, kind)| {
                let name = function
                    .locals
                    .get(local.0 as usize)
                    .map_or("?", |local| local.name.as_str());
                format!("{name}:{}", sem_kind_label(module, *kind))
            })
            .collect::<Vec<_>>()
            .join(", ");
        let return_annotation = function.return_annotation.as_ref().map_or_else(
            || "inferred".to_string(),
            |kinds| sem_kind_list_label(module, kinds),
        );
        let _ = writeln!(
            out,
            "function f{} {} {}({params}) result={} return={return_annotation}",
            function.id.0,
            function.definition,
            function.name,
            sem_kind_list_label(module, &function.result_kinds)
        );
        dump_sem_locals(module, &mut out, &function.locals, 2);
        dump_sem_block(&mut out, &function.body, 2);
    }
    out
}

fn dump_mop(artifacts: &SessionArtifacts) -> String {
    let module = &artifacts.sem;
    let runtime = runtime_bridge::runtime_layout_from_sem(module);
    let mut out = String::new();
    let _ = writeln!(out, "mop {}", module.source_file);

    let _ = writeln!(out, "language environments:");
    let environment = &artifacts.grammar_parse.environment;
    let _ = writeln!(
        out,
        "  {} {} stack={}",
        environment.id,
        environment.name,
        grammar_stack_label(&artifacts.grammar_parse)
    );

    let _ = writeln!(out, "grammars:");
    for grammar in &artifacts.grammar_parse.grammars {
        let extends = if grammar.extends.is_empty() {
            "[]".to_string()
        } else {
            let mut out = String::from("[");
            for (index, id) in grammar.extends.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                let _ = write!(out, "{id}");
            }
            out.push(']');
            out
        };
        let _ = writeln!(
            out,
            "  {} {} extends={} direct-rules={} effective-rules={}",
            grammar.id,
            grammar.name,
            extends,
            grammar.direct_rules.len(),
            grammar.effective_rules.len()
        );
    }

    let _ = writeln!(out, "grammar rules:");
    for rule in &artifacts.grammar_parse.rules {
        let _ = writeln!(
            out,
            "  {} {} grammar={} origin={} location={} expr=\"{}\" data={}",
            rule.id,
            rule.name,
            rule.grammar,
            rule.projection.origin,
            rule.projection.location,
            rule.expression,
            rule.expression_data
        );
    }

    let _ = writeln!(out, "effective grammar rules:");
    for rule in &artifacts.grammar_parse.effective_rules {
        let overridden = rule
            .overridden_rule
            .map_or_else(|| "none".to_string(), |rule| rule.to_string());
        let _ = writeln!(
            out,
            "  {} {} grammar={} selected={} selected-grammar={} resolution={} overridden={}",
            rule.id,
            rule.name,
            rule.grammar,
            rule.selected_rule,
            rule.selected_rule_grammar,
            rule.resolution.as_str(),
            overridden
        );
    }

    let _ = writeln!(out, "grammar activations:");
    for activation in &artifacts.grammar_parse.activations {
        let _ = writeln!(
            out,
            "  {} environment={} grammar={} span={}..{} root-rule={} reason=\"{}\"",
            activation.id,
            activation.environment,
            activation.grammar,
            activation.span.start,
            activation.span.end,
            activation.root_rule,
            activation.reason
        );
    }

    let _ = writeln!(out, "staged regions:");
    for region in &artifacts.grammar_parse.staged_regions {
        let _ = writeln!(
            out,
            "  {} adapter={} activation={} grammar={} driver={} oracle={} directive={}..{} span={}..{} forms={} materialized={:?} data={}",
            grammar_frontend::staged_parse_region_object_id(region),
            region.adapter,
            region.activation,
            region.grammar,
            region.driver.as_str(),
            region.oracle.as_str(),
            region.activation_directive.start,
            region.activation_directive.end,
            region.source_span.start,
            region.source_span.end,
            region.forms_data,
            region.materialized_source,
            region.data
        );
    }

    let _ = writeln!(out, "grammar diagnostics:");
    for fact in &artifacts.grammar_parse.grammar_diagnostics {
        let _ = writeln!(
            out,
            "  d{} code={} grammar={} activation={} rule={} rule-name={} context={} application-span={} span={}..{} unexpected={} expected={}",
            fact.diagnostic_index,
            fact.code.as_str(),
            fact.grammar,
            fact.activation,
            fact.rule,
            fact.rule_name,
            fact.context.as_str(),
            render_optional_span(fact.application_span),
            fact.span.start,
            fact.span.end,
            grammar_unexpected_label(fact.unexpected_fragment.as_deref()),
            grammar_expected_label(fact.expected_fragment.as_deref())
        );
    }

    let _ = writeln!(out, "classes:");
    for class in &module.classes {
        let runtime_class = runtime.class(class.id);
        let runtime_kind =
            runtime_class.map_or("missing", |class| runtime_object_kind_label(class.kind));
        let superclass = sem_superclass_label(module, class.superclass);
        let superclass_chain = sem_superclass_chain_label(module, class.id);
        let _ = writeln!(
            out,
            "  c{} {} {} origin={} superclass={superclass} chain={superclass_chain} finalized={} direct-slots={} effective-slots={} init-plan={} runtime={runtime_kind}",
            class.id.0,
            class.definition,
            class.name,
            sem_class_origin_label(class.origin),
            class.finalized,
            class.direct_slots.len(),
            class.effective_slots.len(),
            sem_initialization_plan_summary(module, class)
        );
        let _ = writeln!(out, "    direct-slots:");
        for slot in &class.direct_slots {
            let _ = writeln!(
                out,
                "      slot s{} {} {} :: {} kind={} origin={} init-keyword: {}:{} init-value={} allocation={} mutability={} runtime={}",
                slot.id.0,
                slot.definition,
                slot.name,
                slot.type_name,
                sem_kind_label(module, slot.kind),
                sem_slot_origin_label(slot.origin),
                slot.init_keyword,
                if slot.required_init_keyword {
                    " required"
                } else {
                    ""
                },
                sem_slot_init_value_label(slot),
                sem_slot_allocation_label(slot.allocation),
                sem_slot_mutability_label(slot.mutability),
                sem_runtime_repr(slot.kind)
            );
            let _ = writeln!(
                out,
                "      accessor {} {}(c{}) -> {} reads c{}.s{}",
                slot.accessor.definition,
                slot.accessor.name,
                slot.accessor.class.0,
                sem_kind_label(module, slot.accessor.result_kind),
                slot.accessor.class.0,
                slot.accessor.slot.0
            );
        }
        let _ = writeln!(out, "    effective-slots:");
        for slot in &class.effective_slots {
            let runtime_slot = runtime_class.and_then(|class| class.slots.get(slot.id.0 as usize));
            let storage = runtime_slot.map_or("missing", |slot| {
                if slot.host_backed {
                    "host-backed"
                } else {
                    "native"
                }
            });
            let source = if slot.source_class == class.id {
                "direct"
            } else {
                "inherited"
            };
            let _ = writeln!(
                out,
                "      slot s{} {} {} :: {} kind={} source={} c{}.s{} origin={} storage={} init-keyword: {}:{} init-value={} allocation={} mutability={} runtime={}",
                slot.id.0,
                slot.definition,
                slot.name,
                slot.type_name,
                sem_kind_label(module, slot.kind),
                source,
                slot.source_class.0,
                slot.source_slot.0,
                sem_slot_origin_label(slot.origin),
                storage,
                slot.init_keyword,
                if slot.required_init_keyword {
                    " required"
                } else {
                    ""
                },
                sem_effective_slot_init_value_label(slot),
                sem_slot_allocation_label(slot.allocation),
                sem_slot_mutability_label(slot.mutability),
                sem_runtime_repr(slot.kind)
            );
            let _ = writeln!(
                out,
                "      accessor {} {}(c{}) -> {} reads c{}.s{}",
                slot.accessor.definition,
                slot.accessor.name,
                slot.accessor.class.0,
                sem_kind_label(module, slot.accessor.result_kind),
                slot.accessor.class.0,
                slot.accessor.slot.0
            );
        }
        let _ = writeln!(out, "    initialization-plan:");
        let _ = writeln!(
            out,
            "      accepted-keywords={}",
            sem_initialization_keywords(class)
        );
        let _ = writeln!(out, "      initialize-methods=deferred");
        for step in &class.initialization_plan.steps {
            let _ = writeln!(
                out,
                "      step s{} {}",
                step.slot.0,
                sem_initialization_step_detail(module, class, step)
            );
        }
    }

    let _ = writeln!(out, "generics:");
    for generic in &module.generics {
        let params = generic
            .params
            .iter()
            .zip(&generic.param_kinds)
            .map(|(name, kind)| match kind {
                Some(kind) => format!("{name}:{}", sem_kind_label(module, *kind)),
                None => format!("{name}:untyped"),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            out,
            "  g{} {} {}({params}) -> {} dispatch-params={} origin={}",
            generic.id.0,
            generic.definition,
            generic.name,
            sem_kind_list_label(module, &generic.result_kinds),
            dispatch_params_label(&generic.dispatch_params),
            sem_generic_origin_label(generic.origin)
        );
        for method in module
            .methods
            .iter()
            .filter(|method| method.generic == generic.id)
        {
            let _ = writeln!(
                out,
                "    method m{} {} specializers={} specializer-origins={} method-origin={} -> {} applicable={}",
                method.id.0,
                method.definition,
                sem_specializer_vector_label(module, &method.specializers),
                sem_specializer_origin_vector_label(module, &method.specializers),
                sem_method_origin_label(method.origin),
                sem_kind_list_label(module, &method.result_kinds),
                sem_dispatch_applicability_vector_label(module, &method.specializers)
            );
        }
    }

    out.push_str(&dump_mop_dispatch_table(module));
    out.push_str(&dump_mop_runtime_layout(artifacts));

    out
}

fn dump_mop_dispatch_table(module: &sem::Module) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "dispatch table:");
    for generic in &module.generics {
        let _ = writeln!(out, "  g{} {}", generic.id.0, generic.name);
        for method in dispatch_methods(module, generic.id) {
            let _ = writeln!(
                out,
                "    m{} specializers={} specializer-origins={} method-origin={} applies-to={}",
                method.id.0,
                sem_specializer_vector_label(module, &method.specializers),
                sem_specializer_origin_vector_label(module, &method.specializers),
                sem_method_origin_label(method.origin),
                sem_dispatch_applicability_vector_label(module, &method.specializers)
            );
        }
    }
    out
}

fn dump_mop_runtime_layout(artifacts: &SessionArtifacts) -> String {
    let module = &artifacts.sem;
    let runtime = runtime_bridge::runtime_layout_from_sem(module);
    let mut out = String::new();
    let _ = writeln!(out, "runtime layout:");
    for (index, class) in runtime.classes().iter().enumerate() {
        let sem_class = module.classes.get(index);
        let superclass = sem_superclass_label(module, class.superclass);
        let _ = writeln!(
            out,
            "  c{index} {} kind={} superclass={superclass}",
            class.name,
            runtime_object_kind_label(class.kind)
        );
        for (slot_index, slot) in class.slots.iter().enumerate() {
            let capability = slot.capability();
            let source = sem_class
                .and_then(|sem_class| {
                    sem_class
                        .effective_slots
                        .get(slot_index)
                        .map(|effective| sem_effective_slot_source_label(sem_class, effective))
                })
                .unwrap_or_else(|| "source=missing".into());
            let _ = writeln!(
                out,
                "    s{slot_index} {} kind={} storage={} readable={} writable={} write-rejection={} {source} runtime={}",
                slot.name,
                runtime_kind_label(module, slot.kind),
                runtime_slot_storage_label(capability.storage),
                capability.readable,
                capability.writable,
                runtime_slot_write_rejection_label(capability.write_rejection),
                runtime_repr(slot.kind)
            );
        }
    }
    out
}

fn runtime_object_kind_label(kind: HostObjectKind) -> &'static str {
    match kind {
        HostObjectKind::Native => "native",
        HostObjectKind::BuiltinValue { .. } => "builtin-value",
        HostObjectKind::HostBacked => "host-backed",
    }
}

fn runtime_slot_storage_label(storage: runtime::RuntimeSlotStorage) -> &'static str {
    match storage {
        runtime::RuntimeSlotStorage::Native => "native",
        runtime::RuntimeSlotStorage::HostBacked => "host-backed",
    }
}

fn runtime_slot_write_rejection_label(
    rejection: Option<runtime::RuntimeSlotWriteRejection>,
) -> &'static str {
    match rejection {
        None => "none",
        Some(runtime::RuntimeSlotWriteRejection::ConstantSlot) => "constant-slot",
        Some(runtime::RuntimeSlotWriteRejection::HostBackedSlot) => "host-backed-slot",
        Some(runtime::RuntimeSlotWriteRejection::NonNativeObject) => "non-native-object",
    }
}

fn dump_grammar_registry(registry: GrammarFrontendRegistry) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "grammar registry");
    let _ = writeln!(out, "environments:");
    for provider in registry.environment_providers() {
        let records = provider.records();
        let _ = write!(
            out,
            "  {} {} discovers-staged-regions={}",
            provider.environment_slug(),
            provider.environment_name(),
            provider.discovers_staged_regions()
        );
        let _ = write!(out, " stack=");
        for (index, grammar_id) in records.environment.grammar_stack.iter().enumerate() {
            if index > 0 {
                out.push_str(" -> ");
            }
            let name = records
                .grammars
                .iter()
                .find(|grammar| grammar.id == *grammar_id)
                .map_or("<unknown>", |grammar| grammar.name.as_str());
            let _ = write!(out, "{grammar_id}:{name}");
        }
        out.push('\n');
    }
    let _ = writeln!(out, "staged adapters:");
    for adapter in registry.staged_adapters() {
        let _ = writeln!(
            out,
            "  {} {} grammar={} root-rule={} provenance-rules={}",
            adapter.adapter_name(),
            adapter.grammar_name(),
            adapter.grammar_slug(),
            adapter.root_rule(),
            adapter.provenance_rule_names().join(",")
        );
    }
    out
}

fn explain_grammar_environment(artifacts: &SessionArtifacts, object_id: &str) -> Option<String> {
    if object_id == "grammar-registry:session" {
        return Some(artifacts.dump_grammar_registry());
    }
    if object_id
        .strip_prefix("registered-grammar-environment:")
        .is_some()
    {
        return dump_registered_grammar_environment_object(artifacts, object_id);
    }
    if grammar_frontend::language_environment_object_id(&artifacts.grammar_parse.environment)
        == object_id
    {
        return Some(render_environment_grammar_list(
            &artifacts.grammar_parse.environment,
            &artifacts.grammar_parse.grammars,
        ));
    }
    let grammar = grammar_for_object(&artifacts.grammar_parse, object_id)?;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "grammar {}",
        grammar_frontend::grammar_object_id(grammar)
    );
    let _ = writeln!(out, "name {}", grammar.name);
    let _ = writeln!(
        out,
        "extends {}",
        render_grammar_extends(&artifacts.grammar_parse, grammar)
    );
    let _ = writeln!(out, "direct-rules {}", grammar.direct_rules.len());
    let _ = writeln!(out, "effective-rules {}", grammar.effective_rules.len());
    out.push_str(&render_grammar_activation_summary(
        &artifacts.grammar_parse,
        grammar,
    ));
    Some(out)
}

fn explain_direct_grammar_rules(artifacts: &SessionArtifacts, object_id: &str) -> Option<String> {
    let grammar = grammar_for_object(&artifacts.grammar_parse, object_id)?;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "direct grammar rules {}",
        grammar_frontend::grammar_object_id(grammar)
    );
    for rule_id in &grammar.direct_rules {
        let label = artifacts
            .grammar_parse
            .rules
            .iter()
            .find(|rule| rule.id == *rule_id)
            .map_or_else(|| rule_id.to_string(), render_direct_grammar_rule_row);
        let _ = writeln!(out, "  {label}");
    }
    Some(out)
}

fn explain_effective_grammar_rules(
    artifacts: &SessionArtifacts,
    object_id: &str,
) -> Option<String> {
    let grammar = grammar_for_object(&artifacts.grammar_parse, object_id)?;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "effective grammar rules {}",
        grammar_frontend::grammar_object_id(grammar)
    );
    for rule_id in &grammar.effective_rules {
        let label = artifacts
            .grammar_parse
            .effective_rules
            .iter()
            .find(|rule| rule.id == *rule_id)
            .map_or_else(|| rule_id.to_string(), render_effective_grammar_rule_row);
        let _ = writeln!(out, "  {label}");
    }
    Some(out)
}

fn explain_grammar_activations(artifacts: &SessionArtifacts, object_id: &str) -> Option<String> {
    let mut out = String::new();
    if grammar_frontend::language_environment_object_id(&artifacts.grammar_parse.environment)
        == object_id
    {
        let _ = writeln!(out, "grammar activations {object_id}");
        for activation in &artifacts.grammar_parse.activations {
            let _ = writeln!(out, "  {}", render_grammar_activation_row(activation));
        }
        return Some(out);
    }

    let grammar = grammar_for_object(&artifacts.grammar_parse, object_id)?;
    let _ = writeln!(
        out,
        "grammar activations {}",
        grammar_frontend::grammar_object_id(grammar)
    );
    for activation in artifacts
        .grammar_parse
        .activations
        .iter()
        .filter(|activation| activation.grammar == grammar.id)
    {
        let _ = writeln!(out, "  {}", render_grammar_activation_row(activation));
    }
    Some(out)
}

fn render_environment_grammar_list(
    environment: &LanguageEnvironment,
    grammars: &[grammar_frontend::Grammar],
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "language environment grammars {}",
        grammar_frontend::language_environment_object_id(environment)
    );
    for (index, grammar_id) in environment.grammar_stack.iter().enumerate() {
        if let Some(grammar) = grammars.iter().find(|grammar| grammar.id == *grammar_id) {
            let active = if index + 1 == environment.grammar_stack.len() {
                "active"
            } else {
                "base"
            };
            let _ = writeln!(
                out,
                "  {index}: {} {} role={active} extends={}",
                grammar_frontend::grammar_object_id(grammar),
                grammar.name,
                render_grammar_extends_for_list(grammars, grammar)
            );
        } else {
            let _ = writeln!(out, "  {index}: {grammar_id} <unknown>");
        }
    }
    out
}

fn grammar_for_object<'a>(
    report: &'a GrammarParseReport,
    object_id: &str,
) -> Option<&'a grammar_frontend::Grammar> {
    report
        .grammars
        .iter()
        .find(|grammar| grammar_frontend::grammar_object_id(grammar) == object_id)
}

fn render_grammar_extends(
    report: &GrammarParseReport,
    grammar: &grammar_frontend::Grammar,
) -> String {
    render_grammar_extends_for_list(&report.grammars, grammar)
}

fn render_grammar_extends_for_list(
    grammars: &[grammar_frontend::Grammar],
    grammar: &grammar_frontend::Grammar,
) -> String {
    if grammar.extends.is_empty() {
        return "none".into();
    }
    grammar
        .extends
        .iter()
        .map(|parent_id| {
            grammars
                .iter()
                .find(|parent| parent.id == *parent_id)
                .map_or_else(
                    || parent_id.to_string(),
                    |parent| format!("{}:{}", parent.id, parent.name),
                )
        })
        .collect::<Vec<_>>()
        .join(" -> ")
}

fn render_direct_grammar_rule_row(rule: &GrammarRule) -> String {
    format!(
        "{} {} expression=\"{}\" origin={}",
        grammar_frontend::grammar_rule_object_id(rule),
        rule.name,
        rule.expression,
        rule.projection.origin
    )
}

fn render_effective_grammar_rule_row(rule: &grammar_frontend::EffectiveGrammarRule) -> String {
    format!(
        "{} {} selected={} from={} resolution={}",
        grammar_frontend::effective_grammar_rule_object_id(rule),
        rule.name,
        rule.selected_rule,
        rule.selected_rule_grammar,
        rule.resolution.as_str()
    )
}

fn explain_grammar_expression(artifacts: &SessionArtifacts, id: &str) -> Option<String> {
    artifacts
        .grammar_parse
        .rules
        .iter()
        .find(|rule| grammar_frontend::grammar_rule_object_id(rule) == id)
        .map(|rule| {
            let mut out = String::new();
            let _ = writeln!(
                out,
                "grammar expression {}",
                grammar_frontend::grammar_rule_object_id(rule)
            );
            let _ = writeln!(out, "rule {}", rule.name);
            let _ = writeln!(out, "text {}", rule.expression);
            let _ = writeln!(
                out,
                "data {}",
                grammar_frontend::rule_expression_source_data(&rule.expression)
            );
            out
        })
}

fn explain_grammar_action(artifacts: &SessionArtifacts, id: &str) -> Option<String> {
    let rule = grammar_action_rule(artifacts, id)?;
    let action = rule.action.as_ref()?;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "grammar action {}",
        grammar_frontend::grammar_action_object_id(rule)
            .unwrap_or_else(|| grammar_frontend::grammar_rule_object_id(rule))
    );
    let _ = writeln!(
        out,
        "rule {}",
        grammar_frontend::grammar_rule_object_id(rule)
    );
    let _ = writeln!(out, "name {}", action.name);
    let _ = writeln!(out, "args {}", action.args.len());
    for arg in &action.args {
        let _ = writeln!(
            out,
            "  {} {}",
            arg.name,
            grammar_action_term_label(&arg.value)
        );
    }
    let _ = writeln!(
        out,
        "data {}",
        grammar_frontend::grammar_action_source_data(action)
    );
    out.push_str(&explain_grammar_action_output(artifacts, id).unwrap_or_default());
    Some(out)
}

fn explain_grammar_action_captures(artifacts: &SessionArtifacts, id: &str) -> Option<String> {
    let rule = grammar_action_rule(artifacts, id)?;
    let action = rule.action.as_ref()?;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "grammar action captures {}",
        grammar_frontend::grammar_action_object_id(rule)
            .unwrap_or_else(|| grammar_frontend::grammar_rule_object_id(rule))
    );
    let mut any = false;
    for arg in &action.args {
        match &arg.value {
            grammar_frontend::GrammarActionTerm::Capture { rule } => {
                any = true;
                let _ = writeln!(out, "  {} captures first {}", arg.name, rule);
            }
            grammar_frontend::GrammarActionTerm::CaptureAll { rule } => {
                any = true;
                let _ = writeln!(out, "  {} captures all {}", arg.name, rule);
            }
            grammar_frontend::GrammarActionTerm::Literal(_)
            | grammar_frontend::GrammarActionTerm::Bool(_) => {}
            _ => {
                any = true;
                let _ = writeln!(out, "  {} unknown-action-term", arg.name);
            }
        }
    }
    if !any {
        let _ = writeln!(out, "  <none>");
    }
    Some(out)
}

fn explain_grammar_action_output(artifacts: &SessionArtifacts, id: &str) -> Option<String> {
    let rule = grammar_action_rule(artifacts, id)?;
    let action = rule.action.as_ref()?;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "grammar action output {}",
        grammar_frontend::grammar_action_object_id(rule)
            .unwrap_or_else(|| grammar_frontend::grammar_rule_object_id(rule))
    );
    let _ = writeln!(out, "action {}", action.name);
    let _ = writeln!(out, "produces {}", grammar_action_output_label(action.name));
    let _ = writeln!(out, "rule-expression {}", rule.expression);
    Some(out)
}

fn grammar_action_rule<'a>(artifacts: &'a SessionArtifacts, id: &str) -> Option<&'a GrammarRule> {
    artifacts.grammar_parse.rules.iter().find(|rule| {
        rule.action.is_some()
            && (grammar_frontend::grammar_action_object_id(rule).as_deref() == Some(id)
                || grammar_frontend::grammar_rule_object_id(rule) == id)
    })
}

fn grammar_action_term_label(term: &grammar_frontend::GrammarActionTerm) -> String {
    match term {
        grammar_frontend::GrammarActionTerm::Capture { rule } => format!("capture({rule})"),
        grammar_frontend::GrammarActionTerm::CaptureAll { rule } => {
            format!("capture-all({rule})")
        }
        grammar_frontend::GrammarActionTerm::Literal(value) => format!("literal({value:?})"),
        grammar_frontend::GrammarActionTerm::Bool(value) => value.to_string(),
        _ => "unknown-action-term".to_string(),
    }
}

fn grammar_action_output_label(action: &str) -> &'static str {
    match action {
        "emit-class" => "frontend class form",
        "emit-slot" => "frontend slot",
        "emit-function" => "frontend function form",
        "emit-integer" | "emit-name" | "emit-call" | "fold-binary" => "frontend expression",
        "emit-argument-list" | "emit-argument-tail" => "frontend expression list",
        "emit-binary-tail" => "binary-tail helper",
        _ => "unknown structural value",
    }
}

fn render_grammar_activation_summary(
    report: &GrammarParseReport,
    grammar: &grammar_frontend::Grammar,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "activations:");
    let mut any = false;
    for activation in report
        .activations
        .iter()
        .filter(|activation| activation.grammar == grammar.id)
    {
        any = true;
        let _ = writeln!(out, "  {}", render_grammar_activation_row(activation));
    }
    if !any {
        let _ = writeln!(out, "  <none>");
    }
    out
}

fn render_grammar_activation_row(activation: &GrammarActivation) -> String {
    format!(
        "{} grammar={} span={}..{} root-rule={} reason=\"{}\"",
        grammar_frontend::grammar_activation_object_id(activation),
        activation.grammar_slug,
        activation.span.start,
        activation.span.end,
        activation.root_rule,
        activation.reason
    )
}

fn frontend_emission_generated_label(
    artifacts: &SessionArtifacts,
    emission: &grammar_frontend::GrammarExtensionEmission,
) -> Option<String> {
    let form = artifacts
        .grammar_parse
        .staged_regions
        .iter()
        .find(|region| {
            region.adapter == emission.adapter
                && region.activation == emission.activation
                && region.object_slug == emission.object_slug
        })?
        .forms
        .get(emission.form_index)?;
    match &form.kind {
        grammar_frontend::FrontendFormKind::Function(function) => artifacts
            .sem
            .functions
            .iter()
            .find(|semantic_function| semantic_function.name == function.name)
            .map(|semantic_function| format!("function:f{}", semantic_function.id.0)),
        grammar_frontend::FrontendFormKind::Class(class) => artifacts
            .sem
            .classes
            .iter()
            .find(|semantic_class| semantic_class.name == class.name)
            .map(|semantic_class| format!("class:c{}", semantic_class.id.0)),
    }
}

fn dump_grammar_extensions(artifacts: &SessionArtifacts) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "grammar extensions");
    let _ = writeln!(out, "registered:");
    for descriptor in artifacts.grammar_registry.extension_descriptors() {
        let _ = writeln!(
            out,
            "  {} {} grammar={} capabilities={} emits={} activates={:?} region={:?} end={:?}",
            grammar_frontend::grammar_extension_object_id(&descriptor),
            descriptor.grammar_name,
            descriptor.grammar_slug,
            render_extension_capabilities(&descriptor.capabilities),
            descriptor.emitted_form_kinds.join(","),
            descriptor.activation_directive,
            descriptor.region_start_directive,
            descriptor.region_end_directive
        );
        let _ = writeln!(
            out,
            "    accepts={:?} root-rule={} provenance={}",
            descriptor.accepted_region,
            descriptor.root_rule,
            descriptor.provenance_rule_names.join(",")
        );
    }
    let _ = writeln!(out, "activations:");
    for activation in &artifacts.grammar_parse.extension_activations {
        let _ = writeln!(
            out,
            "  {} adapter={} activation={} grammar={} directive={}..{} marker={}..{} end={}..{} span={}..{} capabilities={} emitted-forms={}",
            grammar_frontend::grammar_extension_activation_object_id(activation),
            activation.adapter,
            activation.activation,
            activation.grammar,
            activation.activation_directive.start,
            activation.activation_directive.end,
            activation.start_marker.start,
            activation.start_marker.end,
            activation.end_marker.start,
            activation.end_marker.end,
            activation.source_span.start,
            activation.source_span.end,
            render_extension_capabilities(&activation.capabilities),
            activation.emitted_form_count
        );
    }
    let _ = writeln!(out, "emissions:");
    for emission in &artifacts.grammar_parse.extension_emissions {
        let generated = frontend_emission_generated_label(artifacts, emission)
            .unwrap_or_else(|| "none".to_string());
        let _ = writeln!(
            out,
            "  {} adapter={} activation={} kind={} span={}..{} generated={} data={}",
            grammar_frontend::grammar_extension_emission_object_id(emission),
            emission.adapter,
            emission.activation,
            emission.form_kind,
            emission.source_span.start,
            emission.source_span.end,
            generated,
            emission.data
        );
    }
    let _ = writeln!(out, "provenance:");
    for provenance in &artifacts.grammar_parse.extension_provenance {
        let _ = writeln!(
            out,
            "  {} adapter={} activation={} grammar={} rule={} rule-name={} nodes={}",
            grammar_frontend::grammar_extension_provenance_object_id(provenance),
            provenance.adapter,
            provenance.activation,
            provenance.grammar,
            provenance.rule,
            provenance.rule_name,
            provenance.node_count
        );
    }
    out
}

fn explain_grammar_extension(artifacts: &SessionArtifacts, id: &str) -> Option<String> {
    artifacts
        .grammar_registry
        .extension_descriptors()
        .into_iter()
        .find(|descriptor| grammar_frontend::grammar_extension_object_id(descriptor) == id)
        .or_else(|| {
            id.strip_prefix("staged-grammar-adapter:")
                .and_then(|name| artifacts.grammar_registry.extension_descriptor(name))
        })
        .map(explain_grammar_extension_descriptor)
        .or_else(|| {
            artifacts
                .grammar_parse
                .extension_activations
                .iter()
                .find(|activation| {
                    grammar_frontend::grammar_extension_activation_object_id(activation) == id
                })
                .map(explain_grammar_extension_activation)
        })
        .or_else(|| {
            artifacts
                .grammar_parse
                .extension_emissions
                .iter()
                .find(|emission| {
                    grammar_frontend::grammar_extension_emission_object_id(emission) == id
                })
                .map(|emission| explain_grammar_extension_emission(artifacts, emission))
        })
        .or_else(|| {
            artifacts
                .grammar_parse
                .extension_provenance
                .iter()
                .find(|provenance| {
                    grammar_frontend::grammar_extension_provenance_object_id(provenance) == id
                })
                .map(explain_grammar_extension_provenance)
        })
}

fn explain_grammar_extension_descriptor(
    descriptor: grammar_frontend::GrammarExtensionDescriptor,
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "grammar extension {}",
        grammar_frontend::grammar_extension_object_id(&descriptor)
    );
    let _ = writeln!(out, "adapter {}", descriptor.adapter);
    let _ = writeln!(
        out,
        "grammar {} {}",
        descriptor.grammar_slug, descriptor.grammar_name
    );
    let _ = writeln!(
        out,
        "activation-directive {:?}",
        descriptor.activation_directive
    );
    let _ = writeln!(
        out,
        "region-start-directive {:?}",
        descriptor.region_start_directive
    );
    let _ = writeln!(
        out,
        "region-end-directive {:?}",
        descriptor.region_end_directive
    );
    let _ = writeln!(out, "accepted-region {:?}", descriptor.accepted_region);
    let _ = writeln!(out, "root-rule {}", descriptor.root_rule);
    let _ = writeln!(out, "emits {}", descriptor.emitted_form_kinds.join(","));
    let _ = writeln!(
        out,
        "provenance {}",
        descriptor.provenance_rule_names.join(",")
    );
    let _ = writeln!(
        out,
        "capabilities {}",
        render_extension_capabilities(&descriptor.capabilities)
    );
    out
}

fn explain_grammar_extension_activation(
    activation: &grammar_frontend::GrammarExtensionActivation,
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "grammar extension activation {}",
        grammar_frontend::grammar_extension_activation_object_id(activation)
    );
    let _ = writeln!(out, "adapter {}", activation.adapter);
    let _ = writeln!(out, "activation {}", activation.activation);
    let _ = writeln!(
        out,
        "grammar {} {}",
        activation.grammar, activation.grammar_slug
    );
    let _ = writeln!(
        out,
        "activation-directive {}..{}",
        activation.activation_directive.start, activation.activation_directive.end
    );
    let _ = writeln!(
        out,
        "region-marker {}..{}",
        activation.start_marker.start, activation.start_marker.end
    );
    let _ = writeln!(
        out,
        "region-end {}..{}",
        activation.end_marker.start, activation.end_marker.end
    );
    let _ = writeln!(
        out,
        "source-span {}..{}",
        activation.source_span.start, activation.source_span.end
    );
    let _ = writeln!(
        out,
        "capabilities {}",
        render_extension_capabilities(&activation.capabilities)
    );
    let _ = writeln!(out, "emitted-forms {}", activation.emitted_form_count);
    out
}

fn explain_grammar_extension_emission(
    artifacts: &SessionArtifacts,
    emission: &grammar_frontend::GrammarExtensionEmission,
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "grammar extension emission {}",
        grammar_frontend::grammar_extension_emission_object_id(emission)
    );
    let _ = writeln!(out, "adapter {}", emission.adapter);
    let _ = writeln!(out, "activation {}", emission.activation);
    let _ = writeln!(out, "form-index {}", emission.form_index);
    let _ = writeln!(out, "form-kind {}", emission.form_kind);
    let _ = writeln!(
        out,
        "generated {}",
        frontend_emission_generated_label(artifacts, emission)
            .unwrap_or_else(|| "none".to_string())
    );
    let _ = writeln!(
        out,
        "source-span {}..{}",
        emission.source_span.start, emission.source_span.end
    );
    let _ = writeln!(out, "data {}", emission.data);
    out
}

fn explain_grammar_extension_provenance(
    provenance: &grammar_frontend::GrammarExtensionProvenance,
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "grammar extension provenance {}",
        grammar_frontend::grammar_extension_provenance_object_id(provenance)
    );
    let _ = writeln!(out, "adapter {}", provenance.adapter);
    let _ = writeln!(out, "activation {}", provenance.activation);
    let _ = writeln!(out, "grammar {}", provenance.grammar);
    let _ = writeln!(out, "rule {}", provenance.rule);
    let _ = writeln!(out, "rule-name {}", provenance.rule_name);
    let _ = writeln!(out, "nodes {}", provenance.node_count);
    out
}

fn render_extension_capabilities(
    capabilities: &[grammar_frontend::GrammarExtensionCapability],
) -> String {
    capabilities
        .iter()
        .map(|capability| capability.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

fn dump_grammar_stack(report: &GrammarParseReport) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "grammar stack environment={} ({})",
        report.environment.id, report.environment.name
    );
    for (index, grammar_id) in report.grammar_stack.grammars.iter().enumerate() {
        let name = report
            .grammars
            .iter()
            .find(|grammar| grammar.id == *grammar_id)
            .map_or("<unknown>", |grammar| grammar.name.as_str());
        let _ = writeln!(out, "  {index}: {grammar_id} {name}");
    }
    out
}

fn dump_parse_trace(report: &GrammarParseReport) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "parse trace {} provenance={}",
        report.source_file, report.provenance.id
    );
    let _ = writeln!(
        out,
        "environment {} {}",
        report.environment.id, report.environment.name
    );
    let _ = write!(out, "stack");
    for grammar_id in &report.grammar_stack.grammars {
        let name = report
            .grammars
            .iter()
            .find(|grammar| grammar.id == *grammar_id)
            .map_or("<unknown>", |grammar| grammar.name.as_str());
        let _ = write!(out, " {grammar_id}:{name}");
    }
    out.push('\n');
    let metrics = report.frontend_parse.parse_metrics;
    let _ = writeln!(
        out,
        "parser metrics rule-attempts={} memo-hits={} memo-misses={} memo-entries={}",
        metrics.rule_attempts, metrics.memo_hits, metrics.memo_misses, metrics.memo_entries
    );
    let _ = writeln!(out, "activations:");
    for activation in &report.activations {
        let _ = writeln!(
            out,
            "  {} grammar={} span={}..{} root-rule={} reason=\"{}\"",
            activation.id,
            activation.grammar,
            activation.span.start,
            activation.span.end,
            activation.root_rule,
            activation.reason
        );
    }
    let _ = writeln!(out, "staged regions:");
    for region in &report.staged_regions {
        let _ = writeln!(
            out,
            "  {} adapter={} activation={} grammar={} driver={} oracle={} directive={}..{} span={}..{} forms={}",
            grammar_frontend::staged_parse_region_object_id(region),
            region.adapter,
            region.activation,
            region.grammar,
            region.driver.as_str(),
            region.oracle.as_str(),
            region.activation_directive.start,
            region.activation_directive.end,
            region.source_span.start,
            region.source_span.end,
            region.forms_data
        );
        let _ = writeln!(out, "    materialized={:?}", region.materialized_source);
        let _ = writeln!(out, "    data={}", region.data);
    }
    let _ = writeln!(out, "extension activations:");
    for activation in &report.extension_activations {
        let _ = writeln!(
            out,
            "  {} adapter={} directive={}..{} marker={}..{} span={}..{} capabilities={} emitted-forms={}",
            grammar_frontend::grammar_extension_activation_object_id(activation),
            activation.adapter,
            activation.activation_directive.start,
            activation.activation_directive.end,
            activation.start_marker.start,
            activation.start_marker.end,
            activation.source_span.start,
            activation.source_span.end,
            render_extension_capabilities(&activation.capabilities),
            activation.emitted_form_count
        );
    }
    let _ = writeln!(out, "extension emissions:");
    for emission in &report.extension_emissions {
        let _ = writeln!(
            out,
            "  {} kind={} span={}..{} data={}",
            grammar_frontend::grammar_extension_emission_object_id(emission),
            emission.form_kind,
            emission.source_span.start,
            emission.source_span.end,
            emission.data
        );
    }
    let _ = writeln!(out, "extension provenance:");
    for provenance in &report.extension_provenance {
        let _ = writeln!(
            out,
            "  {} rule={} nodes={}",
            grammar_frontend::grammar_extension_provenance_object_id(provenance),
            provenance.rule_name,
            provenance.node_count
        );
    }
    let _ = writeln!(out, "rule provenance:");
    for rule in &report.provenance.rules {
        if rule.node_count > 0 {
            let _ = writeln!(
                out,
                "  {} {} nodes={}",
                rule.rule, rule.rule_name, rule.node_count
            );
        }
    }
    let _ = writeln!(out, "rule applications:");
    for application in &report.provenance.applications {
        let _ = writeln!(
            out,
            "  activation={} grammar={} rule={} rule-name={} span={}..{}",
            application.activation,
            application.grammar,
            application.rule,
            application.rule_name,
            application.span.start,
            application.span.end
        );
    }
    let _ = writeln!(out, "diagnostics:");
    for fact in &report.grammar_diagnostics {
        let _ = writeln!(
            out,
            "  d{} code={} grammar={} activation={} rule={} rule-name={} context={} application-span={} span={}..{} unexpected={} expected={}",
            fact.diagnostic_index,
            fact.code.as_str(),
            fact.grammar,
            fact.activation,
            fact.rule,
            fact.rule_name,
            fact.context.as_str(),
            render_optional_span(fact.application_span),
            fact.span.start,
            fact.span.end,
            grammar_unexpected_label(fact.unexpected_fragment.as_deref()),
            grammar_expected_label(fact.expected_fragment.as_deref())
        );
    }
    out
}

fn dump_parser_shadow_report(report: &GrammarParseReport) -> String {
    let shadow = grammar_frontend::parser_shadow_report(report);
    let mut out = String::new();
    let _ = writeln!(out, "parser shadow status={}", shadow.status.as_str());
    let _ = writeln!(out, "primary:");
    write_parser_product_summary(&mut out, &shadow.primary);
    let _ = writeln!(out, "oracle:");
    if let Some(oracle) = &shadow.oracle {
        write_parser_product_summary(&mut out, oracle);
    } else {
        let _ = writeln!(out, "  <unavailable>");
    }
    let _ = writeln!(out, "divergences:");
    if shadow.divergences.is_empty() {
        let _ = writeln!(out, "  <none>");
    } else {
        for divergence in &shadow.divergences {
            let _ = writeln!(
                out,
                "  {} primary={} oracle={}",
                divergence.field, divergence.primary, divergence.oracle
            );
        }
    }
    out
}

fn write_parser_product_summary(
    out: &mut String,
    summary: &grammar_frontend::ParserProductSummary,
) {
    let _ = writeln!(out, "  implementation {}", summary.implementation);
    let _ = writeln!(out, "  normalized-nodes {}", summary.cst_node_count);
    let _ = writeln!(out, "  top-level-nodes {}", summary.top_level_node_count);
    let _ = writeln!(
        out,
        "  top-level-signature {}",
        if summary.top_level_signature.is_empty() {
            "<empty>"
        } else {
            summary.top_level_signature.as_str()
        }
    );
    let _ = writeln!(out, "  diagnostics {}", summary.diagnostic_count);
    let _ = writeln!(
        out,
        "  rule-applications {}",
        summary.rule_application_count
    );
}

fn grammar_expected_label(expected: Option<&str>) -> String {
    expected.map_or_else(|| "none".into(), |value| format!("{value:?}"))
}

fn grammar_unexpected_label(unexpected: Option<&str>) -> String {
    unexpected.map_or_else(|| "none".into(), |value| format!("{value:?}"))
}

fn render_optional_span(span: Option<Span>) -> String {
    span.map_or_else(
        || "none".into(),
        |span| format!("{}..{}", span.start, span.end),
    )
}

fn explain_grammar_rule(rule: &GrammarRule) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "grammar rule {}",
        grammar_frontend::grammar_rule_object_id(rule)
    );
    let _ = writeln!(out, "id {}", rule.id);
    let _ = writeln!(out, "name {}", rule.name);
    let _ = writeln!(out, "grammar {}", rule.grammar);
    let _ = writeln!(out, "origin {}", rule.projection.origin);
    let _ = writeln!(out, "location {}", rule.projection.location);
    let _ = writeln!(out, "expression {}", rule.expression);
    let _ = writeln!(out, "source-data {}", rule.expression_data);
    if let Some(span) = rule.projection.source_span {
        let _ = writeln!(out, "source-span {}..{}", span.start, span.end);
    } else {
        let _ = writeln!(out, "source-span none");
    }
    out
}

fn explain_effective_grammar_rule(rule: &grammar_frontend::EffectiveGrammarRule) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "effective grammar rule {}",
        grammar_frontend::effective_grammar_rule_object_id(rule)
    );
    let _ = writeln!(out, "id {}", rule.id);
    let _ = writeln!(out, "name {}", rule.name);
    let _ = writeln!(out, "grammar {}", rule.grammar);
    let _ = writeln!(out, "selected-rule {}", rule.selected_rule);
    let _ = writeln!(out, "selected-rule-grammar {}", rule.selected_rule_grammar);
    let _ = writeln!(out, "resolution {}", rule.resolution.as_str());
    if let Some(overridden_rule) = rule.overridden_rule {
        let _ = writeln!(out, "overridden-rule {overridden_rule}");
    } else {
        let _ = writeln!(out, "overridden-rule none");
    }
    out
}

fn grammar_stack_label(report: &GrammarParseReport) -> String {
    let mut out = String::from("[");
    for (index, grammar_id) in report.grammar_stack.grammars.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let _ = write!(out, "{grammar_id}");
    }
    out.push(']');
    out
}

fn dump_eop(artifacts: &SessionArtifacts) -> String {
    render_environment_objects(&artifacts.source, &artifacts.environment_objects())
}

fn render_environment_objects(source: &SourceFile, objects: &[EnvironmentObject]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "eop {} {}", source.id, source.name);
    let _ = writeln!(out, "objects:");
    for object in objects {
        let _ = writeln!(out, "  object {}", object.id);
        let _ = writeln!(
            out,
            "    kind={} class={} origin={} storage={}",
            object.kind.as_str(),
            object.meta_class,
            object.origin.as_str(),
            object.storage.as_str()
        );
        let _ = writeln!(
            out,
            "    presentation title=\"{}\"",
            object.presentation.title
        );
        let _ = writeln!(out, "    source {}", render_source_facet(&object.source));
        let _ = writeln!(
            out,
            "    documentation {}",
            render_documentation_facet(&object.documentation)
        );
        let _ = writeln!(
            out,
            "    artifacts {}",
            render_artifact_facet(&object.artifacts)
        );
        let _ = writeln!(
            out,
            "    affordances {}",
            render_affordances(&object.affordances)
        );
        if let Some(execution) = &object.execution {
            let _ = writeln!(out, "    execution {}", execution.detail);
        }
        let _ = writeln!(out, "    inspection {}", object.inspection.detail);
    }
    out
}

fn render_source_facet(source: &SourceFacet) -> String {
    let source_file = source.source_file.map_or_else(
        || "<none>".to_string(),
        |source_file| source_file.to_string(),
    );
    let definition = source
        .definition
        .map_or_else(|| "<none>".to_string(), |definition| definition.to_string());
    let span = source.span.map_or_else(
        || "<none>".to_string(),
        |span| format!("{}..{}", span.start, span.end),
    );
    format!("file={source_file} definition={definition} span={span}")
}

fn render_documentation_facet(documentation: &DocumentationFacet) -> String {
    if documentation.lines.is_empty() {
        return "[]".into();
    }
    let mut out = String::new();
    out.push('[');
    for (index, line) in documentation.lines.iter().enumerate() {
        if index > 0 {
            out.push_str(" | ");
        }
        out.push_str(line);
    }
    out.push(']');
    out
}

fn render_artifact_facet(artifacts: &ArtifactFacet) -> String {
    if artifacts.keys.is_empty() {
        return "[]".into();
    }
    let mut out = String::new();
    out.push('[');
    for (index, key) in artifacts.keys.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let _ = write!(out, "{key}");
    }
    out.push(']');
    out
}

fn dump_environment_object_stage(
    artifacts: &SessionArtifacts,
    object: &EnvironmentObject,
    stage: DumpStage,
) -> String {
    match stage {
        DumpStage::Sem => dump_sem_environment_object(artifacts, object),
        DumpStage::Lowered => dump_lowered_environment_object(artifacts, object),
        DumpStage::Tape => dump_tape_environment_object(artifacts, object),
        DumpStage::Mop => dump_mop_environment_object(artifacts, object),
        DumpStage::Eop => dump_environment_object_record(object),
        DumpStage::Typing => object.source.definition.map_or_else(
            || {
                format!(
                    "typing summary {}\nno typed definition\n",
                    object.id.as_str()
                )
            },
            |definition| artifacts.typing.dump_definition(&artifacts.sem, definition),
        ),
        DumpStage::Syntax | DumpStage::Ast => {
            let mut out = String::new();
            let _ = writeln!(
                out,
                "{} summary {}",
                dump_stage_name(stage),
                object.id.as_str()
            );
            let _ = writeln!(out, "kind: {}", object.kind.as_str());
            let _ = writeln!(out, "title: {}", object.presentation.title);
            let _ = writeln!(out, "source {}", render_source_facet(&object.source));
            let _ = writeln!(
                out,
                "documentation {}",
                render_documentation_facet(&object.documentation)
            );
            let _ = writeln!(out, "detail: {}", object.inspection.detail);
            out
        }
        DumpStage::All => {
            let mut out = String::new();
            for stage in [
                DumpStage::Syntax,
                DumpStage::Ast,
                DumpStage::Sem,
                DumpStage::Mop,
                DumpStage::Eop,
                DumpStage::Lowered,
                DumpStage::Tape,
                DumpStage::Typing,
            ] {
                out.push_str(&dump_environment_object_stage(artifacts, object, stage));
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push('\n');
            }
            out
        }
    }
}

fn dump_sem_environment_object(artifacts: &SessionArtifacts, object: &EnvironmentObject) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "sem object {}", object.id.as_str());
    match object.kind {
        EnvironmentKind::Function => {
            if let Some(function) = object_index(object.id.as_str(), "function:f")
                .and_then(|index| artifacts.sem.functions.get(index))
            {
                let _ = writeln!(out, "kind: function");
                let _ = writeln!(out, "name: {}", function.name);
                let _ = writeln!(out, "definition: {}", function.definition);
                let _ = writeln!(
                    out,
                    "result: {}",
                    sem_kind_list_label(&artifacts.sem, &function.result_kinds)
                );
                let _ = writeln!(out, "params:");
                for local_id in &function.params {
                    if let Some(local) = function.locals.get(local_id.0 as usize) {
                        let _ = writeln!(
                            out,
                            "  l{} {}: {}",
                            local.id.0,
                            local.name,
                            sem_kind_label(&artifacts.sem, local.kind)
                        );
                    }
                }
                let _ = writeln!(out, "locals: {}", function.locals.len());
            } else {
                dump_environment_object_summary(object, &mut out);
            }
        }
        EnvironmentKind::Constant => {
            if let Some(constant) = object_index(object.id.as_str(), "constant:k")
                .and_then(|index| artifacts.sem.constants.get(index))
            {
                let _ = writeln!(out, "kind: constant");
                let _ = writeln!(out, "name: {}", constant.name);
                let _ = writeln!(out, "definition: {}", constant.definition);
                let _ = writeln!(
                    out,
                    "result: {}",
                    sem_kind_label(&artifacts.sem, constant.result_kind)
                );
                let _ = writeln!(out, "value:");
                dump_sem_expr(&mut out, &constant.value, 2);
            } else {
                dump_environment_object_summary(object, &mut out);
            }
        }
        EnvironmentKind::Method => {
            if let Some(method) = object_index(object.id.as_str(), "method:m")
                .and_then(|index| artifacts.sem.methods.get(index))
            {
                let generic = artifacts
                    .sem
                    .generics
                    .get(method.generic.0 as usize)
                    .map_or("<unknown>", |generic| generic.name.as_str());
                let specializers = method
                    .specializers
                    .iter()
                    .map(|specializer| match specializer {
                        Some(class) => class_label(&artifacts.sem, *class),
                        None => "<invalid>".to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = writeln!(out, "kind: method");
                let _ = writeln!(out, "generic: g{} {generic}", method.generic.0);
                let _ = writeln!(out, "definition: {}", method.definition);
                let _ = writeln!(out, "specializers: {specializers}");
                let _ = writeln!(
                    out,
                    "result: {}",
                    sem_kind_list_label(&artifacts.sem, &method.result_kinds)
                );
                let _ = writeln!(out, "locals: {}", method.locals.len());
            } else {
                dump_environment_object_summary(object, &mut out);
            }
        }
        EnvironmentKind::Generic => {
            if let Some(generic) = object_index(object.id.as_str(), "generic:g")
                .and_then(|index| artifacts.sem.generics.get(index))
            {
                let _ = writeln!(out, "kind: generic");
                let _ = writeln!(out, "name: {}", generic.name);
                let _ = writeln!(out, "definition: {}", generic.definition);
                let _ = writeln!(out, "arity: {}", generic.arity);
                let _ = writeln!(
                    out,
                    "dispatch-params: {}",
                    generic
                        .dispatch_params
                        .iter()
                        .map(|index| index.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                let _ = writeln!(out, "params:");
                for (name, kind) in generic.params.iter().zip(&generic.param_kinds) {
                    let kind = kind.map_or_else(
                        || "untyped".to_string(),
                        |kind| sem_kind_label(&artifacts.sem, kind),
                    );
                    let _ = writeln!(out, "  {name}: {kind}");
                }
                let _ = writeln!(out, "methods:");
                for method in artifacts
                    .sem
                    .methods
                    .iter()
                    .filter(|method| method.generic == generic.id)
                {
                    let _ = writeln!(out, "  method:m{}", method.id.0);
                }
            } else {
                dump_environment_object_summary(object, &mut out);
            }
        }
        EnvironmentKind::Class => {
            if let Some(class) = object_index(object.id.as_str(), "class:c")
                .and_then(|index| artifacts.sem.classes.get(index))
            {
                let _ = writeln!(out, "kind: class");
                let _ = writeln!(out, "name: {}", class.name);
                let _ = writeln!(out, "definition: {}", class.definition);
                let _ = writeln!(
                    out,
                    "superclass: {}",
                    class.superclass.map_or_else(
                        || "<none>".to_string(),
                        |id| class_label(&artifacts.sem, id)
                    )
                );
                let _ = writeln!(out, "direct-slots: {}", class.direct_slots.len());
                for slot in &class.direct_slots {
                    let _ = writeln!(out, "  s{} {} :: {}", slot.id.0, slot.name, slot.type_name);
                }
                let _ = writeln!(out, "effective-slots: {}", class.effective_slots.len());
                for slot in &class.effective_slots {
                    let _ = writeln!(out, "  s{} {} :: {}", slot.id.0, slot.name, slot.type_name);
                }
            } else {
                dump_environment_object_summary(object, &mut out);
            }
        }
        EnvironmentKind::DispatchSite => {
            if let Some(explanation) = artifacts.explain_dispatch_site(object.id.as_str()) {
                out.push_str(&explanation);
            } else {
                dump_environment_object_summary(object, &mut out);
            }
        }
        _ => dump_environment_object_summary(object, &mut out),
    }
    out
}

fn dump_lowered_environment_object(
    artifacts: &SessionArtifacts,
    object: &EnvironmentObject,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "lowered object {}", object.id.as_str());
    let Some(lowered) = &artifacts.lowered else {
        let _ = writeln!(out, "unavailable: {:?}", artifacts.lower_error);
        return out;
    };
    if let Some(function) = lowered_function_for_object(lowered, object) {
        let _ = writeln!(out, "fn l{} {}", function.id.0, function.name);
        let _ = writeln!(out, "kind: {}", lowered_function_kind_label(function.kind));
        let _ = writeln!(out, "params: {}", function.params.len());
        let _ = writeln!(out, "locals: {}", function.locals.len());
        let _ = writeln!(out, "instructions: {}", function.instructions.len());
    } else {
        dump_environment_object_summary(object, &mut out);
    }
    out
}

fn dump_tape_environment_object(
    artifacts: &SessionArtifacts,
    object: &EnvironmentObject,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "tape object {}", object.id.as_str());
    let Some(lowered) = &artifacts.lowered else {
        let _ = writeln!(out, "lowered unavailable: {:?}", artifacts.lower_error);
        return out;
    };
    let Some(tape) = &artifacts.tape else {
        let _ = writeln!(out, "tape unavailable: {:?}", artifacts.tape_error);
        return out;
    };
    if let Some(function) = lowered_function_for_object(lowered, object) {
        let tape_function = tape
            .functions
            .get(function.id.0 as usize)
            .map_or("missing".to_string(), |_| {
                format!("lowered-order-index {}", function.id.0)
            });
        let _ = writeln!(
            out,
            "lowered-function: l{} {}",
            function.id.0, function.name
        );
        let _ = writeln!(out, "tape-function: {tape_function}");
        let _ = writeln!(out, "instructions: {}", function.instructions.len());
    } else {
        dump_environment_object_summary(object, &mut out);
    }
    out
}

fn dump_mop_environment_object(artifacts: &SessionArtifacts, object: &EnvironmentObject) -> String {
    if let Some(grammar_dump) = dump_grammar_mop_environment_object(artifacts, object) {
        return grammar_dump;
    }

    let mut out = String::new();
    let _ = writeln!(out, "mop object {}", object.id.as_str());
    let _ = writeln!(out, "kind: {}", object.kind.as_str());
    let _ = writeln!(out, "meta-class: {}", object.meta_class);
    let _ = writeln!(out, "origin: {}", object.origin.as_str());
    let _ = writeln!(out, "storage: {}", object.storage.as_str());
    let _ = writeln!(out, "source {}", render_source_facet(&object.source));
    let _ = writeln!(
        out,
        "documentation {}",
        render_documentation_facet(&object.documentation)
    );
    let _ = writeln!(
        out,
        "artifacts {}",
        render_artifact_facet(&object.artifacts)
    );
    let _ = writeln!(out, "inspection: {}", object.inspection.detail);
    out
}

fn dump_grammar_mop_environment_object(
    artifacts: &SessionArtifacts,
    object: &EnvironmentObject,
) -> Option<String> {
    match object.kind {
        EnvironmentKind::GrammarRegistry => Some(artifacts.dump_grammar_registry()),
        EnvironmentKind::RegisteredGrammarEnvironment => {
            dump_registered_grammar_environment_object(artifacts, object.id.as_str())
        }
        EnvironmentKind::StagedGrammarAdapter
        | EnvironmentKind::GrammarExtension
        | EnvironmentKind::GrammarExtensionActivation
        | EnvironmentKind::GrammarExtensionEmission
        | EnvironmentKind::GrammarExtensionProvenance => {
            artifacts.explain_grammar_extension(object.id.as_str())
        }
        EnvironmentKind::LanguageEnvironment => {
            dump_language_environment_object(artifacts, object.id.as_str())
        }
        EnvironmentKind::Grammar => dump_grammar_object(artifacts, object.id.as_str()),
        EnvironmentKind::GrammarPackage => dump_grammar_package_object(artifacts, object),
        EnvironmentKind::GrammarResource => dump_grammar_resource_object(artifacts, object),
        EnvironmentKind::GrammarRule | EnvironmentKind::EffectiveGrammarRule => {
            artifacts.explain_grammar_rule(object.id.as_str())
        }
        EnvironmentKind::GrammarAction => artifacts.explain_grammar_action(object.id.as_str()),
        EnvironmentKind::GrammarActivation => {
            dump_grammar_activation_object(artifacts, object.id.as_str())
        }
        EnvironmentKind::StagedRegion => dump_staged_region_object(artifacts, object.id.as_str()),
        _ => None,
    }
}

fn dump_registered_grammar_environment_object(
    artifacts: &SessionArtifacts,
    object_id: &str,
) -> Option<String> {
    let slug = object_id.strip_prefix("registered-grammar-environment:")?;
    let provider = artifacts
        .grammar_registry
        .environment_providers()
        .iter()
        .find(|provider| provider.environment_slug() == slug)?;
    let records = provider.records();
    let mut out = String::new();
    let _ = writeln!(out, "registered grammar environment {object_id}");
    let _ = writeln!(out, "slug {}", provider.environment_slug());
    let _ = writeln!(out, "name {}", provider.environment_name());
    let _ = writeln!(
        out,
        "discovers-staged-regions {}",
        provider.discovers_staged_regions()
    );
    let _ = writeln!(out, "grammar-stack:");
    for grammar_id in &records.environment.grammar_stack {
        let name = records
            .grammars
            .iter()
            .find(|grammar| grammar.id == *grammar_id)
            .map_or("<unknown>", |grammar| grammar.name.as_str());
        let _ = writeln!(out, "  {grammar_id} {name}");
    }
    Some(out)
}

fn dump_language_environment_object(
    artifacts: &SessionArtifacts,
    object_id: &str,
) -> Option<String> {
    if grammar_frontend::language_environment_object_id(&artifacts.grammar_parse.environment)
        != object_id
    {
        return None;
    }

    let environment = &artifacts.grammar_parse.environment;
    let mut out = String::new();
    let _ = writeln!(out, "language environment {object_id}");
    let _ = writeln!(out, "id {}", environment.id);
    let _ = writeln!(out, "name {}", environment.name);
    let _ = writeln!(
        out,
        "discovers-staged-regions {}",
        !artifacts.grammar_parse.staged_regions.is_empty()
            || environment.slug == grammar_frontend::STAGED_LOKO_ENVIRONMENT_SLUG
            || environment.slug == grammar_frontend::BASE_LOKO_ENVIRONMENT_SLUG
    );
    let _ = writeln!(out, "grammar-stack:");
    for grammar_id in &environment.grammar_stack {
        let name = artifacts
            .grammar_parse
            .grammars
            .iter()
            .find(|grammar| grammar.id == *grammar_id)
            .map_or("<unknown>", |grammar| grammar.name.as_str());
        let _ = writeln!(out, "  {grammar_id} {name}");
    }
    let _ = writeln!(out, "available-grammars:");
    out.push_str(&render_environment_grammar_list(
        environment,
        &artifacts.grammar_parse.grammars,
    ));
    let _ = writeln!(out, "activations:");
    for activation in &artifacts.grammar_parse.activations {
        let _ = writeln!(out, "  {}", render_grammar_activation_row(activation));
    }
    Some(out)
}

fn dump_grammar_object(artifacts: &SessionArtifacts, object_id: &str) -> Option<String> {
    let grammar = artifacts
        .grammar_parse
        .grammars
        .iter()
        .find(|grammar| grammar_frontend::grammar_object_id(grammar) == object_id)?;
    let mut out = String::new();
    let _ = writeln!(out, "grammar {object_id}");
    let _ = writeln!(out, "id {}", grammar.id);
    let _ = writeln!(out, "name {}", grammar.name);
    let _ = writeln!(
        out,
        "extends {}",
        render_grammar_extends(&artifacts.grammar_parse, grammar)
    );
    let _ = writeln!(out, "direct-rules:");
    for rule in &grammar.direct_rules {
        let label = artifacts
            .grammar_parse
            .rules
            .iter()
            .find(|direct_rule| direct_rule.id == *rule)
            .map_or_else(
                || rule.to_string(),
                grammar_frontend::grammar_rule_object_id,
            );
        let _ = writeln!(out, "  {label}");
    }
    let _ = writeln!(out, "effective-rules:");
    for rule in &grammar.effective_rules {
        let label = artifacts
            .grammar_parse
            .effective_rules
            .iter()
            .find(|effective_rule| effective_rule.id == *rule)
            .map_or_else(
                || rule.to_string(),
                grammar_frontend::effective_grammar_rule_object_id,
            );
        let _ = writeln!(out, "  {label}");
    }
    out.push_str(&render_grammar_activation_summary(
        &artifacts.grammar_parse,
        grammar,
    ));
    Some(out)
}

fn dump_grammar_package_object(
    artifacts: &SessionArtifacts,
    object: &EnvironmentObject,
) -> Option<String> {
    let grammar = artifacts
        .grammar_parse
        .grammars
        .iter()
        .find(|grammar| grammar_package_object_id(grammar) == object.id.as_str())?;
    let mut out = String::new();
    let _ = writeln!(out, "grammar package {}", object.id);
    let _ = writeln!(out, "grammar {}", grammar.id);
    let _ = writeln!(out, "resources:");
    for resource in grammar_resource_paths(artifacts, grammar) {
        let _ = writeln!(out, "  {}", grammar_resource_object_id(grammar, &resource));
    }
    let _ = writeln!(out, "actions:");
    for rule in artifacts
        .grammar_parse
        .rules
        .iter()
        .filter(|rule| rule.grammar == grammar.id && rule.action.is_some())
    {
        if let Some(action_id) = grammar_frontend::grammar_action_object_id(rule) {
            let _ = writeln!(out, "  {action_id}");
        }
    }
    Some(out)
}

fn dump_grammar_resource_object(
    artifacts: &SessionArtifacts,
    object: &EnvironmentObject,
) -> Option<String> {
    let (grammar_slug, resource) = object
        .id
        .as_str()
        .strip_prefix("grammar-resource:")?
        .split_once('/')?;
    let grammar = artifacts
        .grammar_parse
        .grammars
        .iter()
        .find(|grammar| grammar.slug == grammar_slug)?;
    let mut out = String::new();
    let _ = writeln!(out, "grammar resource {}", object.id);
    let _ = writeln!(out, "grammar {}", grammar.id);
    let _ = writeln!(out, "resource {resource}");
    let _ = writeln!(out, "rules:");
    for rule in artifacts.grammar_parse.rules.iter().filter(|rule| {
        rule.grammar == grammar.id && grammar_rule_resource_path(rule).as_deref() == Some(resource)
    }) {
        let _ = writeln!(out, "  {}", grammar_frontend::grammar_rule_object_id(rule));
    }
    Some(out)
}

fn dump_grammar_activation_object(artifacts: &SessionArtifacts, object_id: &str) -> Option<String> {
    let activation = artifacts
        .grammar_parse
        .activations
        .iter()
        .find(|activation| {
            grammar_frontend::grammar_activation_object_id(activation) == object_id
        })?;
    let mut out = String::new();
    let _ = writeln!(out, "grammar activation {object_id}");
    let _ = writeln!(out, "id {}", activation.id);
    let _ = writeln!(out, "environment {}", activation.environment);
    let _ = writeln!(
        out,
        "grammar {} {}",
        activation.grammar, activation.grammar_slug
    );
    if let Some(grammar) = artifacts
        .grammar_parse
        .grammars
        .iter()
        .find(|grammar| grammar.id == activation.grammar)
    {
        let _ = writeln!(
            out,
            "grammar-object {}",
            grammar_frontend::grammar_object_id(grammar)
        );
        let _ = writeln!(
            out,
            "extends {}",
            render_grammar_extends(&artifacts.grammar_parse, grammar)
        );
    }
    let _ = writeln!(
        out,
        "span {}..{}",
        activation.span.start, activation.span.end
    );
    let _ = writeln!(out, "root-rule {}", activation.root_rule);
    let _ = writeln!(out, "reason {}", activation.reason);
    Some(out)
}

fn dump_staged_region_object(artifacts: &SessionArtifacts, object_id: &str) -> Option<String> {
    let region = artifacts
        .grammar_parse
        .staged_regions
        .iter()
        .find(|region| grammar_frontend::staged_parse_region_object_id(region) == object_id)?;
    let mut out = String::new();
    let _ = writeln!(out, "staged region {object_id}");
    let _ = writeln!(out, "adapter {}", region.adapter);
    let _ = writeln!(out, "activation {}", region.activation);
    let _ = writeln!(out, "grammar {} {}", region.grammar, region.grammar_slug);
    let _ = writeln!(out, "driver {}", region.driver.as_str());
    let _ = writeln!(out, "oracle {}", region.oracle.as_str());
    let _ = writeln!(
        out,
        "activation-directive {}..{}",
        region.activation_directive.start, region.activation_directive.end
    );
    let _ = writeln!(
        out,
        "source-span {}..{}",
        region.source_span.start, region.source_span.end
    );
    let _ = writeln!(out, "forms {}", region.forms_data);
    let _ = writeln!(out, "materialized {}", region.materialized_source);
    let _ = writeln!(out, "data {}", region.data);
    Some(out)
}

fn dump_environment_object_record(object: &EnvironmentObject) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "object {}", object.id);
    let _ = writeln!(out, "kind: {}", object.kind.as_str());
    let _ = writeln!(out, "class: {}", object.meta_class);
    let _ = writeln!(out, "origin: {}", object.origin.as_str());
    let _ = writeln!(out, "storage: {}", object.storage.as_str());
    let _ = writeln!(out, "presentation:");
    let _ = writeln!(out, "  title: {}", object.presentation.title);
    let _ = writeln!(out, "source:");
    let _ = writeln!(out, "  {}", render_source_facet(&object.source));
    let _ = writeln!(out, "documentation:");
    let _ = writeln!(
        out,
        "  {}",
        render_documentation_facet(&object.documentation)
    );
    let _ = writeln!(out, "artifacts:");
    if object.artifacts.keys.is_empty() {
        let _ = writeln!(out, "  <none>");
    } else {
        for key in &object.artifacts.keys {
            let _ = writeln!(out, "  {key}");
        }
    }
    let _ = writeln!(out, "affordances:");
    for affordance in &object.affordances.names {
        let _ = writeln!(out, "  {}", affordance.as_str());
    }
    let _ = writeln!(out, "execution:");
    if let Some(execution) = &object.execution {
        let _ = writeln!(out, "  {}", execution.detail);
    } else {
        let _ = writeln!(out, "  <none>");
    }
    let _ = writeln!(out, "inspection:");
    let _ = writeln!(out, "  {}", object.inspection.detail);
    if let Some(facet) = &object.typing {
        let _ = writeln!(out, "typing:");
        match facet {
            TypingFacet::Config { config, status } => {
                let _ = writeln!(
                    out,
                    "  config={} status={} {}",
                    typing::config_label(*config),
                    status.as_str(),
                    typing_policy_label(*config)
                );
            }
            TypingFacet::Decision { decision, severity } => {
                let _ = writeln!(
                    out,
                    "  check={} verdict={} rule={} severity={}",
                    decision.check.as_str(),
                    decision.verdict.as_str(),
                    decision.rule.as_str(),
                    severity.as_str()
                );
            }
        }
    }
    out
}

fn dump_environment_object_summary(object: &EnvironmentObject, out: &mut String) {
    let _ = writeln!(out, "kind: {}", object.kind.as_str());
    let _ = writeln!(out, "title: {}", object.presentation.title);
    let _ = writeln!(out, "source {}", render_source_facet(&object.source));
    let _ = writeln!(
        out,
        "documentation {}",
        render_documentation_facet(&object.documentation)
    );
    let _ = writeln!(out, "inspection: {}", object.inspection.detail);
}

fn lowered_function_for_object<'a>(
    lowered: &'a lower::Module,
    object: &EnvironmentObject,
) -> Option<&'a lower::Function> {
    let kind = match object.kind {
        EnvironmentKind::Function => lower::FunctionKind::Function(sem::FunctionId(object_index(
            object.id.as_str(),
            "function:f",
        )?
            as u32)),
        EnvironmentKind::Method => lower::FunctionKind::Method(sem::MethodId(object_index(
            object.id.as_str(),
            "method:m",
        )? as u32)),
        EnvironmentKind::Generic => lower::FunctionKind::Dispatcher(sem::GenericFunctionId(
            object_index(object.id.as_str(), "generic:g")? as u32,
        )),
        _ => return None,
    };
    lowered
        .functions
        .iter()
        .find(|function| function.kind == kind)
}

fn lowered_function_kind_label(kind: lower::FunctionKind) -> String {
    match kind {
        lower::FunctionKind::Function(id) => format!("function:f{}", id.0),
        lower::FunctionKind::Method(id) => format!("method:m{}", id.0),
        lower::FunctionKind::Dispatcher(id) => format!("dispatcher generic:g{}", id.0),
        lower::FunctionKind::SlotAccessor { class, slot, .. } => {
            format!("accessor c{}.s{}", class.0, slot.0)
        }
        lower::FunctionKind::CallableThunk { callable } => {
            format!("callable-thunk {callable:?}")
        }
        lower::FunctionKind::Closure(id) => format!("closure:{}", id.0),
    }
}

fn dump_stage_name(stage: DumpStage) -> &'static str {
    match stage {
        DumpStage::Syntax => "syntax",
        DumpStage::Ast => "ast",
        DumpStage::Sem => "sem",
        DumpStage::Mop => "mop",
        DumpStage::Eop => "eop",
        DumpStage::Lowered => "lowered",
        DumpStage::Tape => "tape",
        DumpStage::Typing => "typing",
        DumpStage::All => "all",
    }
}

fn render_affordances(affordances: &AffordanceFacet) -> String {
    let mut out = String::new();
    for (index, affordance) in affordances.names.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(affordance.as_str());
    }
    out
}

fn render_diagnostic_labels(diagnostic: &Diagnostic) -> String {
    let mut out = String::new();
    out.push('[');
    for (index, label) in diagnostic.labels.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let style = match label.style {
            crate::diagnostic::DiagnosticLabelStyle::Primary => "primary",
            crate::diagnostic::DiagnosticLabelStyle::Secondary => "secondary",
        };
        let _ = write!(out, "{style}:{}..{}", label.span.start, label.span.end);
        if let Some(message) = &label.message {
            let _ = write!(out, ":\"{message}\"");
        }
    }
    out.push(']');
    out
}

fn render_diagnostic_args(diagnostic: &Diagnostic) -> String {
    let mut out = String::new();
    out.push('[');
    for (index, arg) in diagnostic.args.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let _ = write!(out, "{}={}", arg.name, arg.value.render());
    }
    out.push(']');
    out
}

struct EnvironmentObjectParts {
    id: String,
    kind: EnvironmentKind,
    meta_class: &'static str,
    origin: EnvironmentOrigin,
    storage: EnvironmentStorage,
    title: String,
    affordances: Vec<EnvironmentAffordance>,
    execution: Option<String>,
    inspection: String,
}

fn environment_object(parts: EnvironmentObjectParts) -> EnvironmentObject {
    EnvironmentObject {
        id: EnvironmentObjectId::new(parts.id),
        kind: parts.kind,
        meta_class: parts.meta_class.to_string(),
        origin: parts.origin,
        storage: parts.storage,
        presentation: PresentationFacet::new(parts.title),
        source: SourceFacet::none(),
        documentation: DocumentationFacet::new(Vec::new()),
        artifacts: ArtifactFacet::new(Vec::new()),
        affordances: AffordanceFacet::new(parts.affordances),
        execution: parts.execution.map(ExecutionFacet::new),
        inspection: InspectionFacet::new(parts.inspection),
        typing: None,
    }
}

fn project_session_environment(
    artifacts: &SessionArtifacts,
    output: &mut dyn EnvironmentObjectSink,
) {
    use EnvironmentAffordance as Aff;

    let module = &artifacts.sem;
    let runtime = runtime_bridge::runtime_layout_from_sem(module);
    let mut objects = Vec::new();
    let sink: &mut dyn EnvironmentObjectSink = &mut objects;
    sink.emit(environment_object(EnvironmentObjectParts {
        id: format!("source:{}", artifacts.source.id),
        kind: EnvironmentKind::SourceFile,
        meta_class: "<source-file>",
        origin: EnvironmentOrigin::SourceAuthored,
        storage: EnvironmentStorage::SourceDefined,
        title: artifacts.source.name.clone(),
        affordances: Vec::from([
            Aff::Inspect,
            Aff::DumpSyntax,
            Aff::DumpAst,
            Aff::DumpSem,
            Aff::DumpMop,
            Aff::DumpEop,
            Aff::DumpLowered,
            Aff::DumpTape,
            Aff::Artifacts,
            Aff::Diff,
            Aff::ShowGrammarStack,
            Aff::ShowParseTrace,
        ]),
        execution: None,
        inspection: format!(
            "source-file={} bytes={}",
            artifacts.source.id,
            artifacts.source.source.len()
        ),
    }));

    project_grammar_registry(artifacts, sink);
    project_environment_grammar(artifacts, sink);

    for class in &module.classes {
        project_environment_class(module, &runtime, sink, class);
    }
    for generic in &module.generics {
        project_environment_generic(module, sink, generic);
    }
    for method in &module.methods {
        project_environment_method(module, sink, method);
    }
    for function in &module.functions {
        project_environment_function(module, sink, function);
    }
    for function in &module.host_functions {
        project_environment_host_function(module, sink, function);
    }
    for constant in &module.constants {
        project_environment_constant(module, sink, constant);
    }
    let dispatch_sites = artifacts.dispatch_sites();
    for site in &dispatch_sites {
        project_environment_dispatch_site(module, sink, site);
    }
    project_typing(artifacts, &dispatch_sites, sink);
    for entry in artifacts.mutations() {
        project_environment_mutation(module, sink, entry);
    }
    for (index, diagnostic) in artifacts.diagnostics.iter().enumerate() {
        sink.emit(environment_object(EnvironmentObjectParts {
            id: format!("diagnostic:d{index}"),
            kind: EnvironmentKind::Diagnostic,
            meta_class: "<diagnostic>",
            origin: EnvironmentOrigin::CompilerGenerated,
            storage: EnvironmentStorage::CachedArtifact,
            title: diagnostic.code.to_string(),
            affordances: Vec::from([Aff::Inspect, Aff::OpenSource, Aff::DumpSem, Aff::DumpEop]),
            execution: None,
            inspection: format!(
                "code={} severity={:?} span={}..{} labels={} args={}",
                diagnostic.code,
                diagnostic.severity,
                diagnostic.span.start,
                diagnostic.span.end,
                render_diagnostic_labels(diagnostic),
                render_diagnostic_args(diagnostic)
            ),
        }));
    }
    for (index, class) in runtime.classes().iter().enumerate() {
        project_environment_runtime_class(module, sink, index, class);
    }
    for report in artifacts.artifact_reports() {
        let artifact = artifact_label(report.artifact);
        sink.emit(environment_object(EnvironmentObjectParts {
            id: format!("artifact:{}", report.key),
            kind: EnvironmentKind::Artifact,
            meta_class: "<query-artifact>",
            origin: EnvironmentOrigin::CompilerGenerated,
            storage: EnvironmentStorage::CachedArtifact,
            title: artifact.clone(),
            affordances: Vec::from([Aff::Inspect, Aff::ShowDependencies, Aff::DumpArtifacts]),
            execution: None,
            inspection: format!("key={} artifact={artifact}", report.key),
        }));
    }
    for mut object in objects {
        enrich_environment_object(artifacts, &mut object);
        output.emit(object);
    }
}

fn enrich_environment_object(artifacts: &SessionArtifacts, object: &mut EnvironmentObject) {
    let source_file = artifacts.source.id;
    match object.kind {
        EnvironmentKind::SourceFile => {
            object.source = SourceFacet::new(
                Some(source_file),
                None,
                Some(Span::new(0, artifacts.source.source.len())),
            );
            object.artifacts = artifact_facet(source_file, &[QueryArtifact::Source]);
        }
        EnvironmentKind::Class => {
            if let Some(class) = object_index(object.id.as_str(), "class:c")
                .and_then(|index| artifacts.sem.classes.get(index))
            {
                object.source =
                    SourceFacet::new(Some(source_file), Some(class.definition), Some(class.span));
                object.artifacts = artifact_facet(
                    source_file,
                    &[
                        QueryArtifact::Bind,
                        QueryArtifact::LoweredModule,
                        QueryArtifact::TapeProgram,
                    ],
                );
            }
        }
        EnvironmentKind::Slot | EnvironmentKind::Accessor => {
            if let Some((class_index, slot_index)) = slot_object_indices(object.id.as_str())
                && let Some(slot) = artifacts
                    .sem
                    .classes
                    .get(class_index)
                    // EOP slot/accessor ids address effective layout slots, so
                    // inherited slots still resolve through the selected class.
                    .and_then(|class| class.effective_slots.get(slot_index))
            {
                object.source =
                    SourceFacet::new(Some(source_file), Some(slot.definition), Some(slot.span));
                object.artifacts = artifact_facet(
                    source_file,
                    &[
                        QueryArtifact::Bind,
                        QueryArtifact::LoweredModule,
                        QueryArtifact::TapeProgram,
                    ],
                );
            }
        }
        EnvironmentKind::Generic => {
            if let Some(generic) = object_index(object.id.as_str(), "generic:g")
                .and_then(|index| artifacts.sem.generics.get(index))
            {
                object.source = SourceFacet::new(
                    Some(source_file),
                    Some(generic.definition),
                    Some(generic.span),
                );
                object.artifacts = artifact_facet(
                    source_file,
                    &[
                        QueryArtifact::Bind,
                        QueryArtifact::GenericDispatcher(generic.definition),
                        QueryArtifact::LoweredModule,
                        QueryArtifact::TapeProgram,
                    ],
                );
            }
        }
        EnvironmentKind::Method => {
            if let Some(method) = object_index(object.id.as_str(), "method:m")
                .and_then(|index| artifacts.sem.methods.get(index))
            {
                object.source = SourceFacet::new(
                    Some(source_file),
                    Some(method.definition),
                    Some(method.span),
                );
                object.artifacts = artifact_facet(
                    source_file,
                    &[
                        QueryArtifact::Bind,
                        QueryArtifact::MethodLookup(method.definition),
                        QueryArtifact::MethodLowering(method.definition),
                        QueryArtifact::MethodTape(method.definition),
                        QueryArtifact::LoweredModule,
                        QueryArtifact::TapeProgram,
                    ],
                );
            }
        }
        EnvironmentKind::Function => {
            if let Some(function) = object_index(object.id.as_str(), "function:f")
                .and_then(|index| artifacts.sem.functions.get(index))
            {
                object.source = SourceFacet::new(
                    Some(source_file),
                    Some(function.definition),
                    Some(function.span),
                );
                object.artifacts = artifact_facet(
                    source_file,
                    &[
                        QueryArtifact::Bind,
                        QueryArtifact::LoweredModule,
                        QueryArtifact::TapeProgram,
                        QueryArtifact::RuntimeResult,
                    ],
                );
            }
        }
        EnvironmentKind::Constant => {
            if let Some(constant) = object_index(object.id.as_str(), "constant:k")
                .and_then(|index| artifacts.sem.constants.get(index))
            {
                object.source = SourceFacet::new(
                    Some(source_file),
                    Some(constant.definition),
                    Some(constant.span),
                );
                object.artifacts = artifact_facet(
                    source_file,
                    &[
                        QueryArtifact::Bind,
                        QueryArtifact::LoweredModule,
                        QueryArtifact::TapeProgram,
                    ],
                );
            }
        }
        EnvironmentKind::DispatchSite => {
            if let Some(site) = artifacts
                .dispatch_sites()
                .into_iter()
                .find(|site| site.id == object.id.as_str())
            {
                object.source = SourceFacet::new(
                    Some(source_file),
                    Some(site.owner.definition),
                    Some(site.span),
                );
                object.artifacts = ArtifactFacet::new(
                    site.related_artifacts
                        .iter()
                        .map(|report| report.key)
                        .collect(),
                );
            }
        }
        EnvironmentKind::TypingConfig => {
            object.artifacts = artifact_facet(
                source_file,
                &[QueryArtifact::TypingConfig, QueryArtifact::Typing],
            );
        }
        EnvironmentKind::TypingDecision => {
            if let Some(TypingFacet::Decision { decision, .. }) = &object.typing {
                object.source =
                    SourceFacet::new(Some(source_file), Some(decision.owner), Some(decision.span));
                object.artifacts = artifact_facet(
                    source_file,
                    &[
                        QueryArtifact::DefinitionTyping(decision.owner),
                        QueryArtifact::Typing,
                    ],
                );
            }
        }
        EnvironmentKind::Diagnostic => {
            if let Some(diagnostic) = object_index(object.id.as_str(), "diagnostic:d")
                .and_then(|index| artifacts.diagnostics.get(index))
            {
                object.source = SourceFacet::new(Some(source_file), None, Some(diagnostic.span));
            }
            object.artifacts = artifact_facet(source_file, &[QueryArtifact::Diagnostics]);
        }
        EnvironmentKind::Artifact => {
            if let Some(report) = artifacts
                .artifact_reports()
                .into_iter()
                .find(|report| object.id.as_str() == format!("artifact:{}", report.key))
            {
                object.artifacts = ArtifactFacet::new(Vec::from([report.key]));
            }
        }
        EnvironmentKind::HostFunction => {
            object.artifacts = artifact_facet(
                source_file,
                &[
                    QueryArtifact::Bind,
                    QueryArtifact::LoweredModule,
                    QueryArtifact::TapeProgram,
                ],
            );
        }
        EnvironmentKind::GrammarRegistry
        | EnvironmentKind::RegisteredGrammarEnvironment
        | EnvironmentKind::StagedGrammarAdapter
        | EnvironmentKind::GrammarExtension => {
            object.artifacts = artifact_facet(
                source_file,
                &[
                    QueryArtifact::EnvironmentResolution,
                    QueryArtifact::GrammarStack,
                ],
            );
        }
        EnvironmentKind::LanguageEnvironment
        | EnvironmentKind::Grammar
        | EnvironmentKind::GrammarPackage
        | EnvironmentKind::GrammarResource => {
            object.artifacts = artifact_facet(
                source_file,
                &[QueryArtifact::GrammarStack, QueryArtifact::GrammarParse],
            );
        }
        EnvironmentKind::GrammarRule | EnvironmentKind::GrammarAction => {
            if let Some(rule) = artifacts.grammar_parse.rules.iter().find(|rule| {
                grammar_frontend::grammar_rule_object_id(rule) == object.id.as_str()
                    || grammar_frontend::grammar_action_object_id(rule).as_deref()
                        == Some(object.id.as_str())
            }) {
                object.source =
                    SourceFacet::new(Some(source_file), None, rule.projection.source_span);
                object.artifacts = artifact_facet(
                    source_file,
                    &[QueryArtifact::GrammarStack, QueryArtifact::GrammarParse],
                );
            }
        }
        EnvironmentKind::EffectiveGrammarRule => {
            object.artifacts = artifact_facet(
                source_file,
                &[QueryArtifact::GrammarStack, QueryArtifact::GrammarParse],
            );
        }
        EnvironmentKind::GrammarActivation => {
            if let Some(activation) =
                artifacts
                    .grammar_parse
                    .activations
                    .iter()
                    .find(|activation| {
                        grammar_frontend::grammar_activation_object_id(activation)
                            == object.id.as_str()
                    })
            {
                object.source = SourceFacet::new(Some(source_file), None, Some(activation.span));
            }
            object.artifacts = artifact_facet(
                source_file,
                &[QueryArtifact::GrammarStack, QueryArtifact::GrammarParse],
            )
        }
        EnvironmentKind::StagedRegion => {
            if let Some(region) = artifacts
                .grammar_parse
                .staged_regions
                .iter()
                .find(|region| {
                    grammar_frontend::staged_parse_region_object_id(region) == object.id.as_str()
                })
            {
                object.source = SourceFacet::new(Some(source_file), None, Some(region.source_span));
            }
            object.artifacts = artifact_facet(
                source_file,
                &[QueryArtifact::GrammarStack, QueryArtifact::GrammarParse],
            )
        }
        EnvironmentKind::GrammarExtensionActivation => {
            if let Some(activation) =
                artifacts
                    .grammar_parse
                    .extension_activations
                    .iter()
                    .find(|activation| {
                        grammar_frontend::grammar_extension_activation_object_id(activation)
                            == object.id.as_str()
                    })
            {
                object.source =
                    SourceFacet::new(Some(source_file), None, Some(activation.source_span));
            }
            object.artifacts = artifact_facet(source_file, &[QueryArtifact::GrammarParse]);
        }
        EnvironmentKind::GrammarExtensionEmission | EnvironmentKind::GrammarExtensionProvenance => {
            if let Some(span) = grammar_extension_object_source_span(artifacts, object) {
                object.source = SourceFacet::new(Some(source_file), None, Some(span));
            }
            object.artifacts = artifact_facet(source_file, &[QueryArtifact::GrammarParse]);
        }
        EnvironmentKind::RuntimeLayoutRow
        | EnvironmentKind::HostObject
        | EnvironmentKind::Mutation => {}
    }

    if object.origin == EnvironmentOrigin::SourceAuthored
        && object.source.source_file == Some(source_file)
        && let Some(span) = object.source.span
    {
        object.documentation =
            DocumentationFacet::new(leading_doc_comments_for_span(&artifacts.syntax, span));
    }
    if !object.documentation.lines.is_empty()
        && !object
            .affordances
            .contains(EnvironmentAffordance::ShowDocumentation)
    {
        object
            .affordances
            .names
            .push(EnvironmentAffordance::ShowDocumentation);
    }
}

fn grammar_extension_object_source_span(
    artifacts: &SessionArtifacts,
    object: &EnvironmentObject,
) -> Option<Span> {
    artifacts
        .grammar_parse
        .extension_emissions
        .iter()
        .find(|emission| {
            grammar_frontend::grammar_extension_emission_object_id(emission) == object.id.as_str()
        })
        .map(|emission| emission.source_span)
        .or_else(|| {
            artifacts
                .grammar_parse
                .extension_provenance
                .iter()
                .find(|provenance| {
                    grammar_frontend::grammar_extension_provenance_object_id(provenance)
                        == object.id.as_str()
                })
                .and_then(|provenance| {
                    artifacts
                        .grammar_parse
                        .extension_activations
                        .iter()
                        .find(|activation| {
                            activation.activation == provenance.activation
                                && activation.object_slug == provenance.object_slug
                        })
                        .map(|activation| activation.source_span)
                })
        })
}

fn artifact_facet(source_file: SourceFileId, artifacts: &[QueryArtifact]) -> ArtifactFacet {
    ArtifactFacet::new(
        artifacts
            .iter()
            .map(|artifact| artifact.key(source_file))
            .collect(),
    )
}

fn object_index(id: &str, prefix: &str) -> Option<usize> {
    id.strip_prefix(prefix)?.parse().ok()
}

fn slot_object_indices(id: &str) -> Option<(usize, usize)> {
    let suffix = id
        .strip_prefix("slot:c")
        .or_else(|| id.strip_prefix("accessor:c"))?;
    let (class, slot) = suffix.split_once(".s")?;
    Some((class.parse().ok()?, slot.parse().ok()?))
}

fn project_grammar_registry(artifacts: &SessionArtifacts, sink: &mut dyn EnvironmentObjectSink) {
    use EnvironmentAffordance as Aff;

    let environments = artifacts.grammar_registry.environment_slugs();
    let adapters = artifacts.grammar_registry.staged_adapter_names();
    sink.emit(environment_object(EnvironmentObjectParts {
        id: "grammar-registry:session".into(),
        kind: EnvironmentKind::GrammarRegistry,
        meta_class: "<grammar-registry>",
        origin: EnvironmentOrigin::CompilerGenerated,
        storage: EnvironmentStorage::FrontendRegistry,
        title: "session grammar registry".into(),
        affordances: Vec::from([Aff::Inspect, Aff::DumpEop, Aff::ShowGrammarRegistry]),
        execution: None,
        inspection: format!(
            "role=host-catalog-plumbing environments=[{}] staged-adapters=[{}]",
            environments.join(","),
            adapters.join(",")
        ),
    }));

    for provider in artifacts.grammar_registry.environment_providers() {
        project_registered_grammar_environment(sink, *provider);
    }
    for adapter in artifacts.grammar_registry.staged_adapters() {
        project_staged_grammar_adapter(sink, *adapter);
    }
    for descriptor in artifacts.grammar_registry.extension_descriptors() {
        project_grammar_extension_descriptor(sink, &descriptor);
    }
}

fn project_registered_grammar_environment(
    sink: &mut dyn EnvironmentObjectSink,
    provider: &'static dyn grammar_frontend::GrammarEnvironmentProvider,
) {
    use EnvironmentAffordance as Aff;

    let records = provider.records();
    let grammar_names = records
        .grammars
        .iter()
        .map(|grammar| grammar.slug.as_str())
        .collect::<Vec<_>>()
        .join(",");
    sink.emit(environment_object(EnvironmentObjectParts {
        id: format!(
            "registered-grammar-environment:{}",
            provider.environment_slug()
        ),
        kind: EnvironmentKind::RegisteredGrammarEnvironment,
        meta_class: "<registered-grammar-environment>",
        origin: registered_environment_origin(provider.environment_slug()),
        storage: EnvironmentStorage::FrontendRegistry,
        title: provider.environment_name().into(),
        affordances: Vec::from([
            Aff::Inspect,
            Aff::DumpMop,
            Aff::DumpEop,
            Aff::ShowGrammarRegistry,
            Aff::ShowGrammars,
        ]),
        execution: None,
        inspection: format!(
            "role=registered-environment-template slug={} name={} discovers-staged-regions={} grammars=[{}]",
            provider.environment_slug(),
            provider.environment_name(),
            provider.discovers_staged_regions(),
            grammar_names
        ),
    }));
}

fn registered_environment_origin(slug: &str) -> EnvironmentOrigin {
    if slug == grammar_frontend::BASE_LOKO_ENVIRONMENT_SLUG {
        EnvironmentOrigin::CompilerGenerated
    } else {
        EnvironmentOrigin::HostProvided
    }
}

fn project_staged_grammar_adapter(
    sink: &mut dyn EnvironmentObjectSink,
    adapter: &'static dyn grammar_frontend::StagedGrammarAdapter,
) {
    use EnvironmentAffordance as Aff;

    sink.emit(environment_object(EnvironmentObjectParts {
        id: format!("staged-grammar-adapter:{}", adapter.adapter_name()),
        kind: EnvironmentKind::StagedGrammarAdapter,
        meta_class: "<staged-grammar-adapter>",
        origin: EnvironmentOrigin::HostProvided,
        storage: EnvironmentStorage::FrontendRegistry,
        title: adapter.grammar_name().into(),
        affordances: Vec::from([
            Aff::Inspect,
            Aff::DumpEop,
            Aff::ShowGrammarRegistry,
            Aff::ShowGrammarExtension,
        ]),
        execution: None,
        inspection: format!(
            "role=host-adapter-implementation name={} grammar={} root-rule={} capabilities=[{}] emits=[{}] provenance-rules=[{}]",
            adapter.adapter_name(),
            adapter.grammar_slug(),
            adapter.root_rule(),
            render_extension_capabilities(adapter.required_capabilities()),
            adapter.emitted_form_kinds().join(","),
            adapter.provenance_rule_names().join(",")
        ),
    }));
}

fn project_grammar_extension_descriptor(
    sink: &mut dyn EnvironmentObjectSink,
    descriptor: &grammar_frontend::GrammarExtensionDescriptor,
) {
    use EnvironmentAffordance as Aff;

    sink.emit(environment_object(EnvironmentObjectParts {
        id: grammar_frontend::grammar_extension_object_id(descriptor),
        kind: EnvironmentKind::GrammarExtension,
        meta_class: "<grammar-extension>",
        origin: EnvironmentOrigin::HostProvided,
        storage: EnvironmentStorage::FrontendRegistry,
        title: descriptor.grammar_name.clone(),
        affordances: Vec::from([
            Aff::Inspect,
            Aff::DumpEop,
            Aff::ShowGrammarRegistry,
            Aff::ShowGrammarExtension,
        ]),
        execution: None,
        inspection: format!(
            "role=grammar-extension-descriptor adapter={} grammar={} activation={:?} region={:?} end={:?} accepts={:?} capabilities=[{}] emits=[{}] provenance-rules=[{}]",
            descriptor.adapter,
            descriptor.grammar_slug,
            descriptor.activation_directive,
            descriptor.region_start_directive,
            descriptor.region_end_directive,
            descriptor.accepted_region,
            render_extension_capabilities(&descriptor.capabilities),
            descriptor.emitted_form_kinds.join(","),
            descriptor.provenance_rule_names.join(",")
        ),
    }));
}

fn project_environment_grammar(artifacts: &SessionArtifacts, sink: &mut dyn EnvironmentObjectSink) {
    use EnvironmentAffordance as Aff;

    let report = &artifacts.grammar_parse;
    project_language_environment(artifacts, sink, &report.environment);
    for grammar in &report.grammars {
        project_grammar_package(artifacts, sink, grammar);
        let id = grammar_frontend::grammar_object_id(grammar);
        sink.emit(environment_object(EnvironmentObjectParts {
            id,
            kind: EnvironmentKind::Grammar,
            meta_class: "<grammar>",
            origin: grammar_origin(grammar),
            storage: EnvironmentStorage::CachedArtifact,
            title: grammar.name.clone(),
            affordances: Vec::from([
                Aff::Inspect,
                Aff::DumpMop,
                Aff::DumpEop,
                Aff::ShowGrammars,
                Aff::ShowDirectRules,
                Aff::ShowEffectiveRules,
                Aff::ShowActivations,
                Aff::ShowGrammarStack,
                Aff::ShowParseTrace,
            ]),
            execution: None,
            inspection: format!(
                "id={} slug={} name={} extends=[{}] direct-rules={} effective-rules={} activations={}",
                grammar.id,
                grammar.slug,
                grammar.name,
                render_grammar_extends(&artifacts.grammar_parse, grammar),
                grammar.direct_rules.len(),
                grammar.effective_rules.len(),
                report
                    .activations
                    .iter()
                    .filter(|activation| activation.grammar == grammar.id)
                    .count()
            ),
        }));
    }
    for rule in &report.rules {
        project_grammar_rule(sink, rule);
        if rule.action.is_some() {
            project_grammar_action(sink, rule);
        }
    }
    for rule in &report.effective_rules {
        project_effective_grammar_rule(sink, rule);
    }
    for activation in &report.activations {
        project_grammar_activation(sink, activation);
    }
    for region in &report.staged_regions {
        project_staged_parse_region(sink, region);
    }
    for activation in &report.extension_activations {
        project_grammar_extension_activation(sink, activation);
    }
    for emission in &report.extension_emissions {
        project_grammar_extension_emission(artifacts, sink, emission);
    }
    for provenance in &report.extension_provenance {
        project_grammar_extension_provenance(sink, provenance);
    }
}

fn project_grammar_package(
    artifacts: &SessionArtifacts,
    sink: &mut dyn EnvironmentObjectSink,
    grammar: &grammar_frontend::Grammar,
) {
    use EnvironmentAffordance as Aff;

    let resources = grammar_resource_paths(artifacts, grammar);
    if resources.is_empty() {
        return;
    }
    sink.emit(environment_object(EnvironmentObjectParts {
        id: grammar_package_object_id(grammar),
        kind: EnvironmentKind::GrammarPackage,
        meta_class: "<grammar-package>",
        origin: EnvironmentOrigin::SourceAuthored,
        storage: EnvironmentStorage::FrontendRegistry,
        title: format!("{} grammar package", grammar.slug),
        affordances: Vec::from([
            Aff::Inspect,
            Aff::DumpMop,
            Aff::DumpEop,
            Aff::ShowGrammars,
            Aff::ShowDirectRules,
            Aff::ShowGrammarStack,
        ]),
        execution: None,
        inspection: format!(
            "grammar={} resources=[{}] direct-rules={} actions={}",
            grammar.id,
            resources.join(","),
            grammar.direct_rules.len(),
            artifacts
                .grammar_parse
                .rules
                .iter()
                .filter(|rule| rule.grammar == grammar.id && rule.action.is_some())
                .count()
        ),
    }));
    for resource in resources {
        sink.emit(environment_object(EnvironmentObjectParts {
            id: grammar_resource_object_id(grammar, &resource),
            kind: EnvironmentKind::GrammarResource,
            meta_class: "<grammar-resource>",
            origin: EnvironmentOrigin::SourceAuthored,
            storage: EnvironmentStorage::FrontendRegistry,
            title: resource.clone(),
            affordances: Vec::from([
                Aff::Inspect,
                Aff::DumpMop,
                Aff::DumpEop,
                Aff::ShowDirectRules,
                Aff::ShowExpression,
            ]),
            execution: None,
            inspection: format!(
                "grammar={} package={} resource={} rules={}",
                grammar.id,
                grammar_package_object_id(grammar),
                resource,
                artifacts
                    .grammar_parse
                    .rules
                    .iter()
                    .filter(|rule| rule.grammar == grammar.id
                        && grammar_rule_resource_path(rule).as_deref() == Some(resource.as_str()))
                    .count()
            ),
        }));
    }
}

fn project_language_environment(
    artifacts: &SessionArtifacts,
    sink: &mut dyn EnvironmentObjectSink,
    environment: &LanguageEnvironment,
) {
    use EnvironmentAffordance as Aff;

    sink.emit(environment_object(EnvironmentObjectParts {
        id: grammar_frontend::language_environment_object_id(environment),
        kind: EnvironmentKind::LanguageEnvironment,
        meta_class: "<language-environment>",
        origin: EnvironmentOrigin::CompilerGenerated,
        storage: EnvironmentStorage::CachedArtifact,
        title: environment.name.clone(),
        affordances: Vec::from([
            Aff::Inspect,
            Aff::DumpMop,
            Aff::DumpEop,
            Aff::ShowGrammars,
            Aff::ShowActivations,
            Aff::ShowGrammarStack,
            Aff::ShowParseTrace,
            Aff::ShowParserShadow,
        ]),
        execution: None,
        inspection: format!(
            "id={} slug={} name={} grammar-stack={} grammars=[{}] activations={}",
            environment.id,
            environment.slug,
            environment.name,
            environment.grammar_stack.len(),
            environment
                .grammar_stack
                .iter()
                .filter_map(|grammar_id| artifacts
                    .grammar_parse
                    .grammars
                    .iter()
                    .find(|grammar| grammar.id == *grammar_id))
                .map(|grammar| grammar.slug.as_str())
                .collect::<Vec<_>>()
                .join(","),
            artifacts.grammar_parse.activations.len()
        ),
    }));
}

fn grammar_origin(grammar: &grammar_frontend::Grammar) -> EnvironmentOrigin {
    if grammar.slug == grammar_frontend::BASE_LOKO_ENVIRONMENT_SLUG {
        EnvironmentOrigin::CompilerGenerated
    } else {
        EnvironmentOrigin::HostProvided
    }
}

fn project_grammar_rule(sink: &mut dyn EnvironmentObjectSink, rule: &GrammarRule) {
    use EnvironmentAffordance as Aff;

    sink.emit(environment_object(EnvironmentObjectParts {
        id: grammar_frontend::grammar_rule_object_id(rule),
        kind: EnvironmentKind::GrammarRule,
        meta_class: "<grammar-rule>",
        origin: grammar_rule_origin(rule),
        storage: EnvironmentStorage::CachedArtifact,
        title: rule.name.clone(),
        affordances: Vec::from([
            Aff::Inspect,
            Aff::DumpMop,
            Aff::DumpEop,
            Aff::ShowGrammarRule,
            Aff::ShowExpression,
            Aff::ShowAction,
            Aff::ShowParseTrace,
        ]),
        execution: None,
        inspection: format!(
            "id={} grammar={} origin={} location={} expression=\"{}\" data={}",
            rule.id,
            rule.grammar,
            rule.projection.origin,
            rule.projection.location,
            rule.expression,
            rule.expression_data
        ),
    }));
}

fn project_grammar_action(sink: &mut dyn EnvironmentObjectSink, rule: &GrammarRule) {
    use EnvironmentAffordance as Aff;

    let Some(action) = &rule.action else {
        return;
    };
    let Some(id) = grammar_frontend::grammar_action_object_id(rule) else {
        return;
    };
    sink.emit(environment_object(EnvironmentObjectParts {
        id,
        kind: EnvironmentKind::GrammarAction,
        meta_class: "<grammar-action>",
        origin: grammar_rule_origin(rule),
        storage: EnvironmentStorage::CachedArtifact,
        title: format!("{} for {}", action.name, rule.name),
        affordances: Vec::from([
            Aff::Inspect,
            Aff::DumpMop,
            Aff::DumpEop,
            Aff::ShowAction,
            Aff::ShowCaptures,
            Aff::ShowActionOutput,
            Aff::ShowExpression,
        ]),
        execution: None,
        inspection: format!(
            "rule={} action={} args={} expression={}",
            grammar_frontend::grammar_rule_object_id(rule),
            action.name,
            action.args.len(),
            grammar_frontend::grammar_action_source_data(action)
        ),
    }));
}

fn project_effective_grammar_rule(
    sink: &mut dyn EnvironmentObjectSink,
    rule: &grammar_frontend::EffectiveGrammarRule,
) {
    use EnvironmentAffordance as Aff;

    let overridden = rule
        .overridden_rule
        .map_or_else(|| "none".to_string(), |rule| rule.to_string());
    sink.emit(environment_object(EnvironmentObjectParts {
        id: grammar_frontend::effective_grammar_rule_object_id(rule),
        kind: EnvironmentKind::EffectiveGrammarRule,
        meta_class: "<effective-grammar-rule>",
        origin: EnvironmentOrigin::CompilerGenerated,
        storage: EnvironmentStorage::CachedArtifact,
        title: rule.name.clone(),
        affordances: Vec::from([
            Aff::Inspect,
            Aff::DumpMop,
            Aff::DumpEop,
            Aff::ShowGrammarRule,
            Aff::ShowParseTrace,
        ]),
        execution: None,
        inspection: format!(
            "id={} grammar={} selected-rule={} selected-rule-grammar={} resolution={} overridden-rule={}",
            rule.id,
            rule.grammar,
            rule.selected_rule,
            rule.selected_rule_grammar,
            rule.resolution.as_str(),
            overridden
        ),
    }));
}

fn grammar_rule_origin(rule: &GrammarRule) -> EnvironmentOrigin {
    match rule.projection.origin {
        grammar_frontend::ProjectionOrigin::TrustedHostFixture => EnvironmentOrigin::HostProvided,
        grammar_frontend::ProjectionOrigin::SourceAuthored => EnvironmentOrigin::SourceAuthored,
        grammar_frontend::ProjectionOrigin::CompilerProjected
        | grammar_frontend::ProjectionOrigin::CompilerProjectedPrototype => {
            EnvironmentOrigin::CompilerGenerated
        }
    }
}

fn grammar_package_object_id(grammar: &grammar_frontend::Grammar) -> String {
    format!("grammar-package:{}", grammar.slug)
}

fn grammar_resource_object_id(grammar: &grammar_frontend::Grammar, resource: &str) -> String {
    format!("grammar-resource:{}/{resource}", grammar.slug)
}

fn grammar_resource_paths(
    artifacts: &SessionArtifacts,
    grammar: &grammar_frontend::Grammar,
) -> Vec<String> {
    let mut resources = Vec::new();
    for rule in artifacts
        .grammar_parse
        .rules
        .iter()
        .filter(|rule| rule.grammar == grammar.id)
    {
        if let Some(resource) = grammar_rule_resource_path(rule)
            && !resources.contains(&resource)
        {
            resources.push(resource);
        }
    }
    resources
}

fn grammar_rule_resource_path(rule: &GrammarRule) -> Option<String> {
    let suffix = rule.projection.location.strip_prefix("resource:")?;
    let (resource, _) = suffix.rsplit_once('/')?;
    Some(resource.to_string())
}

fn project_grammar_activation(
    sink: &mut dyn EnvironmentObjectSink,
    activation: &GrammarActivation,
) {
    use EnvironmentAffordance as Aff;

    sink.emit(environment_object(EnvironmentObjectParts {
        id: grammar_frontend::grammar_activation_object_id(activation),
        kind: EnvironmentKind::GrammarActivation,
        meta_class: "<grammar-activation>",
        origin: EnvironmentOrigin::CompilerGenerated,
        storage: EnvironmentStorage::CachedArtifact,
        title: format!("{} parse activation", activation.grammar_slug),
        affordances: Vec::from([
            Aff::Inspect,
            Aff::DumpMop,
            Aff::DumpEop,
            Aff::ShowGrammars,
            Aff::ShowActivations,
            Aff::ShowGrammarStack,
            Aff::ShowParseTrace,
            Aff::ShowParserShadow,
        ]),
        execution: None,
        inspection: format!(
            "id={} environment={} grammar={} span={}..{} root-rule={} reason=\"{}\"",
            activation.id,
            activation.environment,
            activation.grammar,
            activation.span.start,
            activation.span.end,
            activation.root_rule,
            activation.reason
        ),
    }));
}

fn project_staged_parse_region(
    sink: &mut dyn EnvironmentObjectSink,
    region: &grammar_frontend::StagedParseRegion,
) {
    use EnvironmentAffordance as Aff;

    sink.emit(environment_object(EnvironmentObjectParts {
        id: grammar_frontend::staged_parse_region_object_id(region),
        kind: EnvironmentKind::StagedRegion,
        meta_class: "<staged-region>",
        origin: EnvironmentOrigin::CompilerGenerated,
        storage: EnvironmentStorage::CachedArtifact,
        title: format!("{} staged region {}", region.adapter, region.object_slug),
        affordances: Vec::from([
            Aff::Inspect,
            Aff::DumpMop,
            Aff::DumpEop,
            Aff::ShowGrammarStack,
            Aff::ShowParseTrace,
        ]),
        execution: None,
        inspection: format!(
            "adapter={} activation={} grammar={} driver={} oracle={} directive={}..{} span={}..{} forms={} materialized={:?} data={}",
            region.adapter,
            region.activation,
            region.grammar,
            region.driver.as_str(),
            region.oracle.as_str(),
            region.activation_directive.start,
            region.activation_directive.end,
            region.source_span.start,
            region.source_span.end,
            region.forms_data,
            region.materialized_source,
            region.data
        ),
    }));
}

fn project_grammar_extension_activation(
    sink: &mut dyn EnvironmentObjectSink,
    activation: &grammar_frontend::GrammarExtensionActivation,
) {
    use EnvironmentAffordance as Aff;

    sink.emit(environment_object(EnvironmentObjectParts {
        id: grammar_frontend::grammar_extension_activation_object_id(activation),
        kind: EnvironmentKind::GrammarExtensionActivation,
        meta_class: "<grammar-extension-activation>",
        origin: EnvironmentOrigin::CompilerGenerated,
        storage: EnvironmentStorage::CachedArtifact,
        title: format!("{} extension activation {}", activation.adapter, activation.object_slug),
        affordances: Vec::from([
            Aff::Inspect,
            Aff::DumpMop,
            Aff::DumpEop,
            Aff::ShowGrammarExtension,
            Aff::ShowParseTrace,
        ]),
        execution: None,
        inspection: format!(
            "adapter={} activation={} grammar={} directive={}..{} marker={}..{} end={}..{} span={}..{} capabilities=[{}] emitted-forms={}",
            activation.adapter,
            activation.activation,
            activation.grammar,
            activation.activation_directive.start,
            activation.activation_directive.end,
            activation.start_marker.start,
            activation.start_marker.end,
            activation.end_marker.start,
            activation.end_marker.end,
            activation.source_span.start,
            activation.source_span.end,
            render_extension_capabilities(&activation.capabilities),
            activation.emitted_form_count
        ),
    }));
}

fn project_grammar_extension_emission(
    artifacts: &SessionArtifacts,
    sink: &mut dyn EnvironmentObjectSink,
    emission: &grammar_frontend::GrammarExtensionEmission,
) {
    use EnvironmentAffordance as Aff;

    let generated = frontend_emission_generated_label(artifacts, emission)
        .unwrap_or_else(|| "none".to_string());
    sink.emit(environment_object(EnvironmentObjectParts {
        id: grammar_frontend::grammar_extension_emission_object_id(emission),
        kind: EnvironmentKind::GrammarExtensionEmission,
        meta_class: "<grammar-extension-emission>",
        origin: EnvironmentOrigin::CompilerGenerated,
        storage: EnvironmentStorage::CachedArtifact,
        title: format!(
            "{} {} f{}",
            emission.adapter, emission.form_kind, emission.form_index
        ),
        affordances: Vec::from([
            Aff::Inspect,
            Aff::DumpMop,
            Aff::DumpEop,
            Aff::ShowGrammarExtension,
            Aff::ShowParseTrace,
        ]),
        execution: None,
        inspection: format!(
            "adapter={} activation={} form-index={} kind={} span={}..{} generated={} data={}",
            emission.adapter,
            emission.activation,
            emission.form_index,
            emission.form_kind,
            emission.source_span.start,
            emission.source_span.end,
            generated,
            emission.data
        ),
    }));
}

fn project_grammar_extension_provenance(
    sink: &mut dyn EnvironmentObjectSink,
    provenance: &grammar_frontend::GrammarExtensionProvenance,
) {
    use EnvironmentAffordance as Aff;

    sink.emit(environment_object(EnvironmentObjectParts {
        id: grammar_frontend::grammar_extension_provenance_object_id(provenance),
        kind: EnvironmentKind::GrammarExtensionProvenance,
        meta_class: "<grammar-extension-provenance>",
        origin: EnvironmentOrigin::CompilerGenerated,
        storage: EnvironmentStorage::CachedArtifact,
        title: format!("{} provenance {}", provenance.adapter, provenance.rule_name),
        affordances: Vec::from([
            Aff::Inspect,
            Aff::DumpMop,
            Aff::DumpEop,
            Aff::ShowGrammarExtension,
            Aff::ShowParseTrace,
        ]),
        execution: None,
        inspection: format!(
            "adapter={} activation={} grammar={} rule={} rule-name={} nodes={}",
            provenance.adapter,
            provenance.activation,
            provenance.grammar,
            provenance.rule,
            provenance.rule_name,
            provenance.node_count
        ),
    }));
}

fn project_environment_class(
    module: &sem::Module,
    runtime: &RuntimeLayout,
    sink: &mut dyn EnvironmentObjectSink,
    class: &sem::Class,
) {
    use EnvironmentAffordance as Aff;

    let runtime_class = runtime.class(class.id);
    let runtime_kind =
        runtime_class.map_or("missing", |class| runtime_object_kind_label(class.kind));
    let superclass = sem_superclass_label(module, class.superclass);
    let superclass_chain = sem_superclass_chain_label(module, class.id);
    sink.emit(environment_object(EnvironmentObjectParts {
        id: format!("class:c{}", class.id.0),
        kind: EnvironmentKind::Class,
        meta_class: "<class>",
        origin: eop_class_origin(class.origin),
        storage: eop_class_storage(class.origin),
        title: class.name.clone(),
        affordances: Vec::from([
            Aff::Inspect,
            Aff::DumpMop,
            Aff::DumpEop,
            Aff::ShowRuntimeLayout,
            Aff::ShowSlots,
        ]),
        execution: None,
        inspection: format!(
            "definition={} class-id=c{} superclass={superclass} chain={superclass_chain} finalized={} direct-slots={} effective-slots={} init-plan={} accepted-keywords={} initialize-methods=deferred override=explicit-init-args-over-defaults runtime={runtime_kind}",
            class.definition,
            class.id.0,
            class.finalized,
            class.direct_slots.len(),
            class.effective_slots.len(),
            sem_initialization_plan_summary(module, class),
            sem_initialization_keywords(class)
        ),
    }));

    for slot in &class.effective_slots {
        project_environment_slot(module, sink, class, slot);
        project_environment_accessor(module, sink, class, slot);
    }
}

fn project_environment_slot(
    module: &sem::Module,
    sink: &mut dyn EnvironmentObjectSink,
    class: &sem::Class,
    slot: &sem::EffectiveSlot,
) {
    use EnvironmentAffordance as Aff;

    let source = if slot.source_class == class.id {
        "direct"
    } else {
        "inherited"
    };
    sink.emit(environment_object(EnvironmentObjectParts {
        id: format!("slot:c{}.s{}", class.id.0, slot.id.0),
        kind: EnvironmentKind::Slot,
        meta_class: "<slot>",
        origin: eop_slot_origin(slot.origin),
        storage: eop_slot_storage(slot.origin),
        title: format!("{}.{}", class.name, slot.name),
        affordances: Vec::from([Aff::Inspect, Aff::DumpMop, Aff::DumpEop, Aff::ShowAccessor]),
        execution: None,
        inspection: format!(
            "definition={} owner=class:c{} slot-id=s{} source={} source-slot=class:c{}.s{} kind={} type={} init-keyword={}:{} init-value={} allocation={} mutability={} runtime={}",
            slot.definition,
            class.id.0,
            slot.id.0,
            source,
            slot.source_class.0,
            slot.source_slot.0,
            sem_kind_label(module, slot.kind),
            slot.type_name,
            slot.init_keyword,
            if slot.required_init_keyword {
                " required"
            } else {
                ""
            },
            sem_effective_slot_init_value_label(slot),
            sem_slot_allocation_label(slot.allocation),
            sem_slot_mutability_label(slot.mutability),
            sem_runtime_repr(slot.kind)
        ),
    }));
}

fn project_environment_accessor(
    module: &sem::Module,
    sink: &mut dyn EnvironmentObjectSink,
    class: &sem::Class,
    slot: &sem::EffectiveSlot,
) {
    use EnvironmentAffordance as Aff;

    sink.emit(environment_object(EnvironmentObjectParts {
        id: format!("accessor:c{}.s{}", class.id.0, slot.id.0),
        kind: EnvironmentKind::Accessor,
        meta_class: "<accessor>",
        origin: eop_slot_origin(slot.origin),
        storage: eop_slot_storage(slot.origin),
        title: slot.accessor.name.clone(),
        affordances: Vec::from([Aff::Inspect, Aff::DumpMop, Aff::DumpEop, Aff::ShowSlot]),
        execution: Some(format!(
            "callable=known result={} reads=slot:c{}.s{}",
            sem_kind_label(module, slot.accessor.result_kind),
            class.id.0,
            slot.id.0
        )),
        inspection: format!(
            "definition={} owner=class:c{} slot=s{} source-slot=class:c{}.s{}",
            slot.accessor.definition,
            class.id.0,
            slot.id.0,
            slot.source_class.0,
            slot.source_slot.0
        ),
    }));
}

fn project_environment_generic(
    module: &sem::Module,
    sink: &mut dyn EnvironmentObjectSink,
    generic: &sem::GenericFunction,
) {
    use EnvironmentAffordance as Aff;

    let params = generic
        .params
        .iter()
        .zip(&generic.param_kinds)
        .map(|(name, kind)| match kind {
            Some(kind) => format!("{name}:{}", sem_kind_label(module, *kind)),
            None => format!("{name}:untyped"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let method_count = module
        .methods
        .iter()
        .filter(|method| method.generic == generic.id)
        .count();
    let dispatch = dispatch_methods(module, generic.id)
        .iter()
        .map(|method| {
            format!(
                "m{}=>{}",
                method.id.0,
                sem_dispatch_applicability_vector_label(module, &method.specializers)
            )
        })
        .collect::<Vec<_>>()
        .join(";");
    let generated = !matches!(generic.origin, sem::GenericFunctionOrigin::Source);
    let mut affordances = Vec::from([Aff::Inspect, Aff::DumpMop, Aff::DumpEop, Aff::ShowDispatch]);
    if !generated {
        affordances.push(Aff::AddMethod);
    }
    sink.emit(environment_object(EnvironmentObjectParts {
        id: format!("generic:g{}", generic.id.0),
        kind: EnvironmentKind::Generic,
        meta_class: "<generic-function>",
        origin: if generated {
            EnvironmentOrigin::CompilerGenerated
        } else {
            EnvironmentOrigin::SourceAuthored
        },
        storage: if generated {
            EnvironmentStorage::CachedArtifact
        } else {
            EnvironmentStorage::SourceDefined
        },
        title: format!(
            "{}({params}) -> {}",
            generic.name,
            sem_kind_list_label(module, &generic.result_kinds)
        ),
        affordances,
        execution: Some(format!(
            "callable=generic arity={} dispatch-params={} methods={method_count}",
            generic.arity,
            dispatch_params_label(&generic.dispatch_params)
        )),
        inspection: format!(
            "definition={} generic-id=g{} origin={} dispatch={dispatch}",
            generic.definition,
            generic.id.0,
            sem_generic_origin_label(generic.origin)
        ),
    }));
}

fn project_environment_method(
    module: &sem::Module,
    sink: &mut dyn EnvironmentObjectSink,
    method: &sem::Method,
) {
    use EnvironmentAffordance as Aff;

    let generic = module.generics.get(method.generic.0 as usize);
    let generic_name = generic.map_or("<?>", |generic| generic.name.as_str());
    let specializers = sem_specializer_vector_label(module, &method.specializers);
    let specializer_origins = sem_specializer_origin_vector_label(module, &method.specializers);
    let applicable = sem_dispatch_applicability_vector_label(module, &method.specializers);
    let generated = matches!(method.origin, sem::MethodOrigin::SlotSetter { .. });
    let mut affordances = Vec::from([
        Aff::Inspect,
        Aff::DumpSem,
        Aff::DumpLowered,
        Aff::DumpTape,
        Aff::DumpEop,
        Aff::ShowDependencies,
    ]);
    if !generated {
        affordances.push(Aff::EditBody);
    }
    sink.emit(environment_object(EnvironmentObjectParts {
        id: format!("method:m{}", method.id.0),
        kind: EnvironmentKind::Method,
        meta_class: "<method>",
        origin: if generated {
            EnvironmentOrigin::CompilerGenerated
        } else {
            EnvironmentOrigin::SourceAuthored
        },
        storage: if generated {
            EnvironmentStorage::CachedArtifact
        } else {
            EnvironmentStorage::SourceDefined
        },
        title: format!(
            "{generic_name}{specializers} -> {}",
            sem_kind_list_label(module, &method.result_kinds)
        ),
        affordances,
        execution: Some(format!(
            "callable=method participates-in=generic:g{} specializers={specializers}",
            method.generic.0
        )),
        inspection: format!(
            "definition={} method-id=m{} method-origin={} specializer-origins={specializer_origins} applies-to={applicable}",
            method.definition,
            method.id.0,
            sem_method_origin_label(method.origin)
        ),
    }));
}

fn project_environment_function(
    module: &sem::Module,
    sink: &mut dyn EnvironmentObjectSink,
    function: &sem::Function,
) {
    use EnvironmentAffordance as Aff;

    let params = function
        .params
        .iter()
        .zip(&function.param_kinds)
        .map(|(local, kind)| {
            let name = function
                .locals
                .get(local.0 as usize)
                .map_or("?", |local| local.name.as_str());
            format!("{name}:{}", sem_kind_label(module, *kind))
        })
        .collect::<Vec<_>>()
        .join(", ");
    sink.emit(environment_object(EnvironmentObjectParts {
        id: format!("function:f{}", function.id.0),
        kind: EnvironmentKind::Function,
        meta_class: "<function>",
        origin: EnvironmentOrigin::SourceAuthored,
        storage: EnvironmentStorage::SourceDefined,
        title: format!(
            "{}({params}) -> {}",
            function.name,
            sem_kind_list_label(module, &function.result_kinds)
        ),
        affordances: Vec::from([
            Aff::Inspect,
            Aff::DumpSem,
            Aff::DumpLowered,
            Aff::DumpTape,
            Aff::DumpEop,
            Aff::Run,
            Aff::ShowDependencies,
            Aff::EditBody,
        ]),
        execution: Some(format!("callable=direct arity={}", function.params.len())),
        inspection: format!(
            "definition={} function-id=f{}",
            function.definition, function.id.0
        ),
    }));
}

fn project_environment_host_function(
    module: &sem::Module,
    sink: &mut dyn EnvironmentObjectSink,
    function: &sem::ResolvedHostFunction,
) {
    use EnvironmentAffordance as Aff;

    let params = sem_kind_list_label(module, &function.param_kinds);
    sink.emit(environment_object(EnvironmentObjectParts {
        id: format!("host-function:{}", function.name),
        kind: EnvironmentKind::HostFunction,
        meta_class: "<host-function>",
        origin: EnvironmentOrigin::HostBacked,
        storage: EnvironmentStorage::HostRegistry,
        title: format!(
            "{}({params}) -> {}",
            function.name,
            sem_kind_label(module, function.result_kind)
        ),
        affordances: Vec::from([
            Aff::Inspect,
            Aff::DumpSem,
            Aff::DumpLowered,
            Aff::DumpTape,
            Aff::DumpEop,
        ]),
        execution: Some(format!(
            "callable=host scalar=true arity={}",
            function.param_kinds.len()
        )),
        inspection: format!("host-function-id=h{} handler=embedding", function.id.0),
    }));
}

fn project_environment_constant(
    module: &sem::Module,
    sink: &mut dyn EnvironmentObjectSink,
    constant: &sem::Constant,
) {
    use EnvironmentAffordance as Aff;

    sink.emit(environment_object(EnvironmentObjectParts {
        id: format!("constant:k{}", constant.id.0),
        kind: EnvironmentKind::Constant,
        meta_class: "<constant>",
        origin: EnvironmentOrigin::SourceAuthored,
        storage: EnvironmentStorage::SourceDefined,
        title: format!(
            "{} = {}",
            constant.name,
            sem_kind_label(module, constant.result_kind)
        ),
        affordances: Vec::from([
            Aff::Inspect,
            Aff::DumpSem,
            Aff::DumpLowered,
            Aff::DumpTape,
            Aff::DumpEop,
            Aff::ShowDependencies,
        ]),
        execution: Some("binding=immutable top-level".to_string()),
        inspection: format!(
            "definition={} constant-id=k{}",
            constant.definition, constant.id.0
        ),
    }));
}

fn project_environment_dispatch_site(
    module: &sem::Module,
    sink: &mut dyn EnvironmentObjectSink,
    site: &DispatchSiteRecord,
) {
    use EnvironmentAffordance as Aff;

    let generic = generic_label(module, site.generic);
    let static_kinds = sem_kind_vector_label(module, &site.static_argument_kinds);
    let exact_classes = exact_class_vector_label(module, &site.exact_static_classes);
    let selected = site.selected_method.map_or_else(
        || "runtime-needed".into(),
        |method| method_label(module, method),
    );
    let artifact_keys = site
        .related_artifacts
        .iter()
        .map(|report| report.key.to_string())
        .collect::<Vec<_>>()
        .join(",");
    sink.emit(environment_object(EnvironmentObjectParts {
        id: site.id.clone(),
        kind: EnvironmentKind::DispatchSite,
        meta_class: "<dispatch-site>",
        origin: EnvironmentOrigin::CompilerGenerated,
        storage: EnvironmentStorage::CachedArtifact,
        title: format!("{generic} in {}", site.owner.name),
        affordances: Vec::from([
            Aff::Inspect,
            Aff::ShowDispatch,
            Aff::ShowDependencies,
            Aff::DumpLowered,
            Aff::DumpTape,
            Aff::DumpEop,
            Aff::OpenSource,
        ]),
        execution: Some(format!(
            "dispatcher={} runtime-needed={} selected={selected}",
            site.lowered_dispatcher, site.runtime_needed
        )),
        inspection: format!(
            "key={} owner={} {} span={}..{} generic={} contract={} static-args={} exact-classes={} applicable={} rejected={} artifacts={artifact_keys}",
            site.key,
            site.owner.kind.as_str(),
            site.owner.name,
            site.span.start,
            site.span.end,
            generic,
            generic_contract_label(module, site.generic),
            static_kinds,
            exact_classes,
            site.applicable.len(),
            site.rejected.len()
        ),
    }));
}

/// Stable EOP id of the session typing configuration object.
const TYPING_CONFIG_OBJECT_ID: &str = "typing-config:session";

/// Stable EOP id of one typing decision: owner definition plus ordinal.
fn typing_decision_object_id(decision: &typing::TypingDecision) -> String {
    format!("typing:{}:t{}", decision.owner, decision.index)
}

fn typing_policy_label(config: typing::TypingConfig) -> String {
    typing::TypingCheck::ALL
        .iter()
        .map(|check| {
            let policy = config.policy(*check);
            format!(
                "{}=checked:{},refuted:{}",
                check.as_str(),
                policy.checked.as_str(),
                policy.refuted.as_str()
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn typing_dispatch_site<'a>(
    decision: &typing::TypingDecision,
    sites: &'a [DispatchSiteRecord],
) -> Option<&'a DispatchSiteRecord> {
    let typing::TypingExpectation::Dispatch { generic, .. } = &decision.expected else {
        return None;
    };
    sites
        .iter()
        .find(|site| site.span == decision.span && site.generic == *generic)
}

fn project_typing(
    artifacts: &SessionArtifacts,
    dispatch_sites: &[DispatchSiteRecord],
    sink: &mut dyn EnvironmentObjectSink,
) {
    use EnvironmentAffordance as Aff;

    let module = &artifacts.sem;
    let typed = &artifacts.typing;
    let mut config = environment_object(EnvironmentObjectParts {
        id: TYPING_CONFIG_OBJECT_ID.into(),
        kind: EnvironmentKind::TypingConfig,
        meta_class: "<typing-config>",
        origin: EnvironmentOrigin::CompilerGenerated,
        storage: EnvironmentStorage::CachedArtifact,
        title: format!("typing {}", typing::config_label(typed.config)),
        affordances: Vec::from([
            Aff::Inspect,
            Aff::ShowTyping,
            Aff::ShowDependencies,
            Aff::DumpEop,
        ]),
        execution: None,
        inspection: format!(
            "config={} status={} decisions={} proved={} checked={} refuted={} {}",
            typing::config_label(typed.config),
            typed.status.as_str(),
            typed.decisions().count(),
            typed.count(typing::TypingVerdict::Proved),
            typed.count(typing::TypingVerdict::Checked),
            typed.count(typing::TypingVerdict::Refuted),
            typing_policy_label(typed.config)
        ),
    });
    config.typing = Some(TypingFacet::Config {
        config: typed.config,
        status: typed.status,
    });
    sink.emit(config);

    for decision in typed.decisions() {
        let owner = typing::definition_label(module, decision.owner);
        let expected = typing::expectation_label(module, &decision.expected);
        let title = match &decision.expected {
            typing::TypingExpectation::Dispatch { generic, .. } => {
                format!("dispatch {} in {owner}", generic_label(module, *generic))
            }
            typing::TypingExpectation::Class(_) => format!("cast to {expected} in {owner}"),
        };
        let severity = typed.severity(decision);
        let dispatch_site = typing_dispatch_site(decision, dispatch_sites)
            .map_or_else(|| "none".into(), |site| site.id.clone());
        let mut object = environment_object(EnvironmentObjectParts {
            id: typing_decision_object_id(decision),
            kind: EnvironmentKind::TypingDecision,
            meta_class: "<typing-decision>",
            origin: EnvironmentOrigin::CompilerGenerated,
            storage: EnvironmentStorage::CachedArtifact,
            title,
            affordances: Vec::from([Aff::Inspect, Aff::ShowTyping, Aff::OpenSource, Aff::DumpEop]),
            execution: None,
            inspection: format!(
                "check={} verdict={} rule={} owner={} span={}..{} expected={expected} actual={} severity={} dispatch-site={dispatch_site}",
                decision.check.as_str(),
                decision.verdict.as_str(),
                decision.rule.as_str(),
                decision.owner,
                decision.span.start,
                decision.span.end,
                typing::static_type_name_list(module, &decision.actual),
                severity.as_str(),
            ),
        });
        object.typing = Some(TypingFacet::Decision {
            decision: decision.clone(),
            severity,
        });
        sink.emit(object);
    }
}

fn explain_typing_decision(
    artifacts: &SessionArtifacts,
    decision: &typing::TypingDecision,
) -> String {
    let module = &artifacts.sem;
    let typed = &artifacts.typing;
    let policy = typed.config.policy(decision.check);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "typing decision {}",
        typing_decision_object_id(decision)
    );
    let _ = writeln!(
        out,
        "owner: {} {}",
        typing::definition_label(module, decision.owner),
        decision.owner
    );
    let _ = writeln!(out, "check: {}", decision.check.as_str());
    let _ = writeln!(out, "verdict: {}", decision.verdict.as_str());
    let _ = writeln!(out, "rule: {}", decision.rule.as_str());
    let _ = writeln!(out, "span: {}..{}", decision.span.start, decision.span.end);
    let _ = writeln!(
        out,
        "expected: {}",
        typing::expectation_label(module, &decision.expected)
    );
    let _ = writeln!(
        out,
        "actual: {}",
        typing::static_type_name_list(module, &decision.actual)
    );
    if let typing::TypingExpectation::Dispatch { methods, .. } = &decision.expected {
        for method in methods {
            let _ = writeln!(out, "  method {}", method_label(module, *method));
        }
        let _ = writeln!(
            out,
            "selected-method: {}",
            decision
                .selected_method
                .map_or_else(|| "runtime".into(), |method| method_label(module, method))
        );
        if let Some(site) = typing_dispatch_site(decision, &artifacts.dispatch_sites()) {
            let _ = writeln!(out, "dispatch-site: {}", site.id);
        }
    }
    let _ = writeln!(
        out,
        "severity: {} (config {} {} checked={} refuted={})",
        typed.severity(decision).as_str(),
        typing::config_label(typed.config),
        decision.check.as_str(),
        policy.checked.as_str(),
        policy.refuted.as_str()
    );
    let _ = writeln!(
        out,
        "contract: advisory; the runtime still performs this check"
    );
    out
}

fn project_environment_mutation(
    module: &sem::Module,
    sink: &mut dyn EnvironmentObjectSink,
    entry: &AppliedMutation,
) {
    use EnvironmentAffordance as Aff;

    let (request_label, target_label) = match &entry.request {
        SessionMutation::AddMethod { generic, .. } => {
            ("add-method", generic_label(module, *generic))
        }
    };
    let inserted_label = method_label(module, entry.outcome.inserted_method);
    let strategy_label = match entry.outcome.recompute_strategy {
        RecomputeStrategy::FullCandidateRebuild => "full-candidate-rebuild",
    };
    sink.emit(environment_object(EnvironmentObjectParts {
        id: format!("mutation:m{}", entry.mutation_id),
        kind: EnvironmentKind::Mutation,
        meta_class: "<mutation>",
        origin: EnvironmentOrigin::Session,
        storage: EnvironmentStorage::TransientInMemory,
        title: format!("{request_label} on {target_label} (#{})", entry.mutation_id),
        affordances: Vec::from([Aff::Inspect, Aff::DumpEop]),
        execution: None,
        inspection: format!(
            "mutation-id={} request={request_label} target={target_label} inserted={inserted_label} affected-artifacts={} strategy={strategy_label}",
            entry.mutation_id,
            entry.outcome.affected_artifacts.len(),
        ),
    }));
}

fn project_environment_runtime_class(
    module: &sem::Module,
    sink: &mut dyn EnvironmentObjectSink,
    index: usize,
    class: &runtime::RuntimeClass,
) {
    use EnvironmentAffordance as Aff;

    let superclass = sem_superclass_label(module, class.superclass);
    sink.emit(environment_object(EnvironmentObjectParts {
        id: format!("runtime-class:c{index}"),
        kind: EnvironmentKind::RuntimeLayoutRow,
        meta_class: "<runtime-class>",
        origin: EnvironmentOrigin::CompilerGenerated,
        storage: EnvironmentStorage::CachedArtifact,
        title: format!("runtime {}", class.name),
        affordances: Vec::from([Aff::Inspect, Aff::DumpMop, Aff::DumpEop, Aff::DumpTape]),
        execution: None,
        inspection: format!(
            "class-id=c{index} storage-kind={} superclass={superclass} slots={}",
            runtime_object_kind_label(class.kind),
            class.slots.len()
        ),
    }));
    for (slot_index, slot) in class.slots.iter().enumerate() {
        let capability = slot.capability();
        let source = module
            .classes
            .get(index)
            .and_then(|sem_class| {
                sem_class
                    .effective_slots
                    .get(slot_index)
                    .map(|effective| sem_effective_slot_source_label(sem_class, effective))
            })
            .unwrap_or_else(|| "source=missing".into());
        sink.emit(environment_object(EnvironmentObjectParts {
            id: format!("runtime-slot:c{index}.s{slot_index}"),
            kind: EnvironmentKind::RuntimeLayoutRow,
            meta_class: "<runtime-slot>",
            origin: EnvironmentOrigin::CompilerGenerated,
            storage: EnvironmentStorage::CachedArtifact,
            title: format!("runtime {}.{}", class.name, slot.name),
            affordances: Vec::from([Aff::Inspect, Aff::DumpMop, Aff::DumpEop, Aff::DumpTape]),
            execution: None,
            inspection: format!(
                "class-id=c{index} slot-id=s{slot_index} storage={} readable={} writable={} write-rejection={} kind={} {source} runtime={}",
                runtime_slot_storage_label(capability.storage),
                capability.readable,
                capability.writable,
                runtime_slot_write_rejection_label(capability.write_rejection),
                runtime_kind_label(module, slot.kind),
                runtime_repr(slot.kind)
            ),
        }));
    }
}

fn eop_class_origin(origin: sem::ClassOrigin) -> EnvironmentOrigin {
    match origin {
        sem::ClassOrigin::ObjectRoot => EnvironmentOrigin::CompilerGenerated,
        sem::ClassOrigin::Native => EnvironmentOrigin::SourceAuthored,
        sem::ClassOrigin::BuiltinValue { .. } => EnvironmentOrigin::CompilerGenerated,
        sem::ClassOrigin::HostBacked { .. } => EnvironmentOrigin::HostBacked,
    }
}

fn eop_class_storage(origin: sem::ClassOrigin) -> EnvironmentStorage {
    match origin {
        sem::ClassOrigin::ObjectRoot => EnvironmentStorage::CachedArtifact,
        sem::ClassOrigin::Native => EnvironmentStorage::SourceDefined,
        sem::ClassOrigin::BuiltinValue { .. } => EnvironmentStorage::CachedArtifact,
        sem::ClassOrigin::HostBacked { .. } => EnvironmentStorage::HostRegistry,
    }
}

fn eop_slot_origin(origin: sem::SlotOrigin) -> EnvironmentOrigin {
    match origin {
        sem::SlotOrigin::Native => EnvironmentOrigin::SourceAuthored,
        sem::SlotOrigin::HostBacked { .. } => EnvironmentOrigin::HostBacked,
    }
}

fn eop_slot_storage(origin: sem::SlotOrigin) -> EnvironmentStorage {
    match origin {
        sem::SlotOrigin::Native => EnvironmentStorage::SourceDefined,
        sem::SlotOrigin::HostBacked { .. } => EnvironmentStorage::HostAdapter,
    }
}

fn value_class_label(module: &sem::Module, label: &str, class: sem::ClassId) -> String {
    module.classes.get(class.0 as usize).map_or_else(
        || format!("{label}(class#{})", class.0),
        |class| format!("{label}({})", class.name),
    )
}

fn value_class_fingerprint(module: &sem::Module, label: &str, class: sem::ClassId) -> String {
    module.classes.get(class.0 as usize).map_or_else(
        || format!("{label}#{}", class.0),
        |class| format!("{label}#{}", class.definition.raw()),
    )
}

fn sem_kind_label(module: &sem::Module, kind: sem::ValueKind) -> String {
    match kind {
        sem::ValueKind::Any => "object-top".into(),
        sem::ValueKind::I64 => "integer".into(),
        sem::ValueKind::F64 => "float".into(),
        sem::ValueKind::Bool => "boolean".into(),
        sem::ValueKind::Callable => "callable".into(),
        sem::ValueKind::String(class) => value_class_label(module, "string", class),
        sem::ValueKind::Symbol(class) => value_class_label(module, "symbol", class),
        sem::ValueKind::Vector(class) => value_class_label(module, "vector", class),
        sem::ValueKind::Object(class) => module.classes.get(class.0 as usize).map_or_else(
            || format!("class#{}", class.0),
            |class| format!("object({})", class.name),
        ),
        sem::ValueKind::ClassObject(class) => module.classes.get(class.0 as usize).map_or_else(
            || format!("class-object#{}", class.0),
            |class| format!("class-object({})", class.name),
        ),
        sem::ValueKind::GenericFunctionObject(generic) => {
            module.generics.get(generic.0 as usize).map_or_else(
                || format!("generic-function-object#{}", generic.0),
                |generic| format!("generic-function-object({})", generic.name),
            )
        }
        sem::ValueKind::MethodObject(method) => format!("method-object(m{})", method.0),
        sem::ValueKind::SlotDescriptorObject { class, slot } => {
            format!("slot-descriptor(c{}.s{})", class.0, slot.0)
        }
        sem::ValueKind::KnownCallable(callable) => {
            format!("known-callable({})", sem_callable_label(module, callable))
        }
        sem::ValueKind::Error => "error".into(),
    }
}

fn sem_kind_list_label(module: &sem::Module, kinds: &[sem::ValueKind]) -> String {
    match kinds {
        [] => "()".into(),
        [kind] => sem_kind_label(module, *kind),
        _ => {
            let labels = kinds
                .iter()
                .map(|kind| sem_kind_label(module, *kind))
                .collect::<Vec<_>>()
                .join(", ");
            format!("({labels})")
        }
    }
}

fn runtime_kind_label(module: &sem::Module, kind: runtime::ValueKind) -> String {
    match kind {
        runtime::ValueKind::Any => "object-top".into(),
        runtime::ValueKind::I64 => "integer".into(),
        runtime::ValueKind::F64 => "float".into(),
        runtime::ValueKind::Bool => "boolean".into(),
        runtime::ValueKind::String(class) => value_class_label(module, "string", class),
        runtime::ValueKind::Symbol(class) => value_class_label(module, "symbol", class),
        runtime::ValueKind::Vector(class) => value_class_label(module, "vector", class),
        runtime::ValueKind::Object(class) => module.classes.get(class.0 as usize).map_or_else(
            || format!("class#{}", class.0),
            |class| format!("object({})", class.name),
        ),
        runtime::ValueKind::ClassObject(class) => module.classes.get(class.0 as usize).map_or_else(
            || format!("class-object#{}", class.0),
            |class| format!("class-object({})", class.name),
        ),
        runtime::ValueKind::GenericFunctionObject(generic) => {
            module.generics.get(generic.0 as usize).map_or_else(
                || format!("generic-function-object#{}", generic.0),
                |generic| format!("generic-function-object({})", generic.name),
            )
        }
        runtime::ValueKind::MethodObject(method) => format!("method-object(m{})", method.0),
        runtime::ValueKind::SlotDescriptorObject { class, slot } => {
            format!("slot-descriptor(c{}.s{})", class.0, slot.0)
        }
    }
}

const fn numeric_kind_label(kind: sem::NumericKind) -> &'static str {
    match kind {
        sem::NumericKind::Integer => "integer",
        sem::NumericKind::Float => "float",
    }
}

fn sem_kind_label_for_expr(kind: sem::ValueKind) -> String {
    match kind {
        sem::ValueKind::Any => "object-top".into(),
        sem::ValueKind::I64 => "integer".into(),
        sem::ValueKind::F64 => "float".into(),
        sem::ValueKind::Bool => "boolean".into(),
        sem::ValueKind::Callable => "callable".into(),
        sem::ValueKind::String(class) => format!("string(c{})", class.0),
        sem::ValueKind::Symbol(class) => format!("symbol(c{})", class.0),
        sem::ValueKind::Vector(class) => format!("vector(c{})", class.0),
        sem::ValueKind::Object(class) => format!("object(c{})", class.0),
        sem::ValueKind::ClassObject(class) => format!("class-object(c{})", class.0),
        sem::ValueKind::GenericFunctionObject(generic) => {
            format!("generic-function-object(g{})", generic.0)
        }
        sem::ValueKind::MethodObject(method) => format!("method-object(m{})", method.0),
        sem::ValueKind::SlotDescriptorObject { class, slot } => {
            format!("slot-descriptor(c{}.s{})", class.0, slot.0)
        }
        sem::ValueKind::KnownCallable(callable) => {
            format!("known-callable({})", sem_callable_label_for_expr(callable))
        }
        sem::ValueKind::Error => "error".into(),
    }
}

fn sem_kind_list_label_for_expr(kinds: &[sem::ValueKind]) -> String {
    match kinds {
        [] => "()".into(),
        [kind] => sem_kind_label_for_expr(*kind),
        values => {
            let values = values
                .iter()
                .map(|kind| sem_kind_label_for_expr(*kind))
                .collect::<Vec<_>>()
                .join(", ");
            format!("({values})")
        }
    }
}

fn sem_kind_fingerprint(module: &sem::Module, kind: sem::ValueKind) -> String {
    match kind {
        sem::ValueKind::Any => "object-top".into(),
        sem::ValueKind::I64 => "integer".into(),
        sem::ValueKind::F64 => "float".into(),
        sem::ValueKind::Bool => "boolean".into(),
        sem::ValueKind::Callable => "callable".into(),
        sem::ValueKind::String(class) => value_class_fingerprint(module, "string", class),
        sem::ValueKind::Symbol(class) => value_class_fingerprint(module, "symbol", class),
        sem::ValueKind::Vector(class) => value_class_fingerprint(module, "vector", class),
        sem::ValueKind::Object(class) => module.classes.get(class.0 as usize).map_or_else(
            || format!("class#{}", class.0),
            |class| format!("object#{}", class.definition.raw()),
        ),
        sem::ValueKind::ClassObject(class) => module.classes.get(class.0 as usize).map_or_else(
            || format!("class-object#{}", class.0),
            |class| format!("class-object#{}", class.definition.raw()),
        ),
        sem::ValueKind::GenericFunctionObject(generic) => {
            module.generics.get(generic.0 as usize).map_or_else(
                || format!("generic-function-object#{}", generic.0),
                |generic| format!("generic-function-object#{}", generic.definition.raw()),
            )
        }
        sem::ValueKind::MethodObject(method) => module.methods.get(method.0 as usize).map_or_else(
            || format!("method-object#{}", method.0),
            |method| format!("method-object#{}", method.definition.raw()),
        ),
        sem::ValueKind::SlotDescriptorObject { class, slot } => module
            .classes
            .get(class.0 as usize)
            .and_then(|class| class.effective_slots.get(slot.0 as usize))
            .map_or_else(
                || format!("slot-descriptor#{}.{}", class.0, slot.0),
                |slot| format!("slot-descriptor#{}", slot.definition.raw()),
            ),
        sem::ValueKind::KnownCallable(callable) => sem_callable_fingerprint(module, callable),
        sem::ValueKind::Error => "error".into(),
    }
}

fn sem_kind_list_fingerprint(module: &sem::Module, kinds: &[sem::ValueKind]) -> String {
    match kinds {
        [] => "()".into(),
        [kind] => sem_kind_fingerprint(module, *kind),
        _ => {
            let kinds = kinds
                .iter()
                .map(|kind| sem_kind_fingerprint(module, *kind))
                .collect::<Vec<_>>()
                .join(",");
            format!("({kinds})")
        }
    }
}

fn sem_runtime_repr(kind: sem::ValueKind) -> &'static str {
    match kind {
        sem::ValueKind::Any => "boxed-any",
        sem::ValueKind::I64 => "i64",
        sem::ValueKind::F64 => "f64",
        sem::ValueKind::Bool => "bool",
        sem::ValueKind::String(_)
        | sem::ValueKind::Symbol(_)
        | sem::ValueKind::Vector(_)
        | sem::ValueKind::Object(_)
        | sem::ValueKind::ClassObject(_)
        | sem::ValueKind::GenericFunctionObject(_)
        | sem::ValueKind::MethodObject(_)
        | sem::ValueKind::SlotDescriptorObject { .. } => "object-handle",
        sem::ValueKind::Callable => "closure",
        sem::ValueKind::KnownCallable(_) => "compile-time-callable",
        sem::ValueKind::Error => "error",
    }
}

fn runtime_repr(kind: runtime::ValueKind) -> &'static str {
    match kind {
        runtime::ValueKind::Any => "boxed-any",
        runtime::ValueKind::I64 => "i64",
        runtime::ValueKind::F64 => "f64",
        runtime::ValueKind::Bool => "bool",
        runtime::ValueKind::String(_)
        | runtime::ValueKind::Symbol(_)
        | runtime::ValueKind::Vector(_)
        | runtime::ValueKind::Object(_)
        | runtime::ValueKind::ClassObject(_)
        | runtime::ValueKind::GenericFunctionObject(_)
        | runtime::ValueKind::MethodObject(_)
        | runtime::ValueKind::SlotDescriptorObject { .. } => "object-handle",
    }
}

fn sem_slot_init_value_label(slot: &sem::Slot) -> &'static str {
    if slot.init_value.is_some() {
        "yes"
    } else {
        "none"
    }
}

fn sem_effective_slot_init_value_label(slot: &sem::EffectiveSlot) -> &'static str {
    if slot.init_value.is_some() {
        "yes"
    } else {
        "none"
    }
}

fn sem_slot_allocation_label(allocation: sem::SlotAllocation) -> &'static str {
    match allocation {
        sem::SlotAllocation::Instance => "instance",
    }
}

fn sem_slot_mutability_label(mutability: sem::SlotMutability) -> &'static str {
    match mutability {
        sem::SlotMutability::Mutable => "mutable",
        sem::SlotMutability::Constant => "constant",
    }
}

fn sem_intrinsic_label(intrinsic: sem::Intrinsic) -> &'static str {
    match intrinsic {
        sem::Intrinsic::ObjectClass => "object-class",
        sem::Intrinsic::ClassName => "class-name",
        sem::Intrinsic::ClassDirectSuperclass => "class-direct-superclass",
        sem::Intrinsic::ClassDirectSlots => "class-direct-slots",
        sem::Intrinsic::ClassEffectiveSlots => "class-effective-slots",
        sem::Intrinsic::SlotName => "slot-name",
        sem::Intrinsic::SlotType => "slot-type",
        sem::Intrinsic::SlotInitKeyword => "slot-init-keyword",
        sem::Intrinsic::SlotMutable => "slot-mutable?",
        sem::Intrinsic::SlotGetter => "slot-getter",
        sem::Intrinsic::SlotSetter => "slot-setter",
        sem::Intrinsic::GenericFunctionName => "generic-function-name",
        sem::Intrinsic::GenericFunctionMethods => "generic-function-methods",
        sem::Intrinsic::MethodGenericFunction => "method-generic-function",
        sem::Intrinsic::MethodSpecializers => "method-specializers",
        sem::Intrinsic::VectorSize => "vector-size",
        sem::Intrinsic::VectorRef => "vector-ref",
        sem::Intrinsic::FloatToString => "float-to-string",
        sem::Intrinsic::IntegerToString => "integer-to-string",
        sem::Intrinsic::Concatenate => "concatenate",
    }
}

fn sem_callable_label(module: &sem::Module, callable: sem::KnownCallable) -> String {
    match callable {
        sem::KnownCallable::DirectFunction(function) => {
            module.functions.get(function.0 as usize).map_or_else(
                || format!("function#{}", function.0),
                |function| format!("function {}", function.name),
            )
        }
        sem::KnownCallable::GenericFunction(generic) => {
            module.generics.get(generic.0 as usize).map_or_else(
                || format!("generic#{}", generic.0),
                |generic| format!("generic {}", generic.name),
            )
        }
        sem::KnownCallable::SlotAccessor { class, slot, .. } => module
            .classes
            .get(class.0 as usize)
            .and_then(|class| {
                class
                    .effective_slots
                    .get(slot.0 as usize)
                    .map(|slot| format!("accessor {}.{}", class.name, slot.name))
            })
            .unwrap_or_else(|| format!("accessor c{}.s{}", class.0, slot.0)),
    }
}

fn sem_callable_label_for_expr(callable: sem::KnownCallable) -> String {
    match callable {
        sem::KnownCallable::DirectFunction(function) => format!("function f{}", function.0),
        sem::KnownCallable::GenericFunction(generic) => format!("generic g{}", generic.0),
        sem::KnownCallable::SlotAccessor { class, slot, .. } => {
            format!("accessor c{}.s{}", class.0, slot.0)
        }
    }
}

fn sem_callable_fingerprint(module: &sem::Module, callable: sem::KnownCallable) -> String {
    match callable {
        sem::KnownCallable::DirectFunction(function) => {
            module.functions.get(function.0 as usize).map_or_else(
                || format!("function#{}", function.0),
                |function| format!("function#{}", function.definition.raw()),
            )
        }
        sem::KnownCallable::GenericFunction(generic) => {
            module.generics.get(generic.0 as usize).map_or_else(
                || format!("generic#{}", generic.0),
                |generic| format!("generic#{}", generic.definition.raw()),
            )
        }
        sem::KnownCallable::SlotAccessor {
            accessor,
            class,
            slot,
        } => {
            let class_definition = module
                .classes
                .get(class.0 as usize)
                .map_or(0, |class| class.definition.raw());
            let slot_definition = module
                .classes
                .get(class.0 as usize)
                .and_then(|class| class.effective_slots.get(slot.0 as usize))
                .map_or(0, |slot| slot.definition.raw());
            format!(
                "accessor#{}:{class_definition}.{slot_definition}",
                accessor.raw()
            )
        }
    }
}

fn dump_sem_locals(module: &sem::Module, out: &mut String, locals: &[sem::Local], indent: usize) {
    for local in locals {
        indent_line(out, indent);
        let _ = writeln!(
            out,
            "local l{} {} {} {}",
            local.id.0,
            local.definition,
            local.name,
            sem_kind_label(module, local.kind)
        );
    }
}

fn dump_sem_block(out: &mut String, block: &sem::Block, indent: usize) {
    for binding in &block.lets {
        indent_line(out, indent);
        let _ = write!(out, "let ");
        dump_sem_local_list(out, &binding.locals);
        out.push('\n');
        dump_sem_expr(out, &binding.value, indent + 2);
    }
    indent_line(out, indent);
    let _ = writeln!(out, "result");
    dump_sem_expr(out, &block.result, indent + 2);
}

fn dump_sem_local_list(out: &mut String, locals: &[sem::LocalId]) {
    match locals {
        [] => {
            let _ = write!(out, "()");
        }
        [local] => {
            let _ = write!(out, "l{}", local.0);
        }
        _ => {
            out.push('(');
            for (index, local) in locals.iter().enumerate() {
                if index != 0 {
                    out.push_str(", ");
                }
                let _ = write!(out, "l{}", local.0);
            }
            out.push(')');
        }
    }
}

fn dump_sem_expr(out: &mut String, expr: &sem::Expr, indent: usize) {
    indent_line(out, indent);
    match expr {
        sem::Expr::ConstI64 { value, .. } => {
            let _ = writeln!(out, "const.i64 {value}");
        }
        sem::Expr::ConstF64 { value, .. } => {
            let _ = writeln!(out, "const.f64 {value}");
        }
        sem::Expr::ConstBool { value, .. } => {
            let _ = writeln!(out, "const.bool {value}");
        }
        sem::Expr::StringLiteral { value, class, .. } => {
            let _ = writeln!(out, "literal.string {:?} class=c{}", value, class.0);
        }
        sem::Expr::SymbolLiteral { name, class, .. } => {
            let _ = writeln!(out, "literal.symbol #{name} class=c{}", class.0);
        }
        sem::Expr::VectorLiteral {
            elements, class, ..
        } => {
            let _ = writeln!(
                out,
                "literal.vector class=c{} elements={}",
                class.0,
                elements.len()
            );
            for element in elements {
                dump_sem_expr(out, element, indent + 2);
            }
        }
        sem::Expr::Values {
            values,
            result_kinds,
            ..
        } => {
            let _ = writeln!(out, "values results={result_kinds:?}");
            for value in values {
                dump_sem_expr(out, value, indent + 2);
            }
        }
        sem::Expr::Local {
            local, result_kind, ..
        } => {
            let _ = writeln!(
                out,
                "local l{} as {}",
                local.0,
                sem_kind_label_for_expr(*result_kind)
            );
        }
        sem::Expr::Capture {
            capture,
            result_kind,
            ..
        } => {
            let _ = writeln!(
                out,
                "capture c{} as {}",
                capture.0,
                sem_kind_label_for_expr(*result_kind)
            );
        }
        sem::Expr::ConstantRef {
            constant,
            result_kind,
            ..
        } => {
            let _ = writeln!(
                out,
                "constant-ref k{} as {}",
                constant.0,
                sem_kind_label_for_expr(*result_kind)
            );
        }
        sem::Expr::ClassObject { class, .. } => {
            let _ = writeln!(out, "class-object c{}", class.0);
        }
        sem::Expr::KnownCallable { callable, .. } => {
            let _ = writeln!(
                out,
                "known-callable {}",
                sem_callable_label_for_expr(*callable)
            );
        }
        sem::Expr::Closure {
            closure,
            captures,
            result_kind,
            ..
        } => {
            let _ = writeln!(
                out,
                "closure c{} captures={} result={}",
                closure.0,
                captures.len(),
                sem_kind_label_for_expr(*result_kind)
            );
        }
        sem::Expr::GenericFunctionObject { generic, .. } => {
            let _ = writeln!(out, "generic-function-object g{}", generic.0);
        }
        sem::Expr::IntrinsicCall {
            intrinsic,
            args,
            result_kind,
            ..
        } => {
            let _ = writeln!(
                out,
                "intrinsic {} result={}",
                sem_intrinsic_label(*intrinsic),
                sem_kind_label_for_expr(*result_kind)
            );
            for arg in args {
                dump_sem_expr(out, arg, indent + 2);
            }
        }
        sem::Expr::HostCall {
            function,
            args,
            result_kind,
            ..
        } => {
            let _ = writeln!(
                out,
                "host-call h{} result={}",
                function.0,
                sem_kind_label_for_expr(*result_kind)
            );
            for arg in args {
                dump_sem_expr(out, arg, indent + 2);
            }
        }
        sem::Expr::DirectCall { function, args, .. } => {
            let _ = writeln!(out, "direct-call f{}", function.0);
            for arg in args {
                dump_sem_expr(out, arg, indent + 2);
            }
        }
        sem::Expr::CallableCall {
            callable,
            args,
            result_kinds,
            ..
        } => {
            let _ = writeln!(
                out,
                "callable-call result={}",
                sem_kind_list_label_for_expr(result_kinds)
            );
            dump_sem_expr(out, callable, indent + 2);
            for arg in args {
                dump_sem_expr(out, arg, indent + 2);
            }
        }
        sem::Expr::GenericCall { generic, args, .. } => {
            let _ = writeln!(out, "generic-call g{}", generic.0);
            for arg in args {
                dump_sem_expr(out, arg, indent + 2);
            }
        }
        sem::Expr::Signal {
            condition,
            condition_class,
            result_kinds,
            ..
        } => {
            let _ = writeln!(
                out,
                "signal c{} result={}",
                condition_class.0,
                sem_kind_list_label_for_expr(result_kinds)
            );
            dump_sem_expr(out, condition, indent + 2);
        }
        sem::Expr::ClassOf { object, class, .. } => {
            let _ = writeln!(out, "class-of c{}", class.0);
            dump_sem_expr(out, object, indent + 2);
        }
        sem::Expr::InstanceOf { value, class, .. } => {
            let _ = writeln!(out, "instance? c{}", class.0);
            dump_sem_expr(out, value, indent + 2);
        }
        sem::Expr::Cast { value, class, .. } => {
            let _ = writeln!(out, "as c{}", class.0);
            dump_sem_expr(out, value, indent + 2);
        }
        sem::Expr::MakeObject {
            class,
            class_id,
            init_args,
            fields,
            ..
        } => {
            let _ = writeln!(out, "make c{}", class_id.0);
            indent_line(out, indent + 2);
            let _ = writeln!(out, "class");
            dump_sem_expr(out, class, indent + 4);
            if !init_args.is_empty() {
                indent_line(out, indent + 2);
                let _ = writeln!(out, "init-args");
                for arg in init_args {
                    indent_line(out, indent + 4);
                    let _ = writeln!(out, "{}: -> s{}", arg.keyword, arg.slot.0);
                    dump_sem_expr(out, &arg.value, indent + 6);
                }
            }
            for field in fields {
                indent_line(out, indent + 2);
                let _ = writeln!(
                    out,
                    "field s{} source={}",
                    field.slot.0,
                    sem_field_init_source_label(field.source)
                );
                dump_sem_expr(out, &field.value, indent + 4);
            }
        }
        sem::Expr::AccessorCall {
            accessor,
            receiver,
            class,
            slot,
            result_kind,
            ..
        } => {
            let _ = writeln!(
                out,
                "accessor-call {accessor} c{}.s{} result={result_kind:?}",
                class.0, slot.0
            );
            dump_sem_expr(out, receiver, indent + 2);
        }
        sem::Expr::SlotWrite {
            setter,
            receiver,
            value,
            class,
            slot,
            result_kind,
            ..
        } => {
            let _ = writeln!(
                out,
                "slot-write {setter} c{}.s{} result={result_kind:?}",
                class.0, slot.0
            );
            dump_sem_expr(out, receiver, indent + 2);
            dump_sem_expr(out, value, indent + 2);
        }
        sem::Expr::Binary {
            op, kind, lhs, rhs, ..
        } => {
            let _ = writeln!(out, "binary {op:?} {}", numeric_kind_label(*kind));
            dump_sem_expr(out, lhs, indent + 2);
            dump_sem_expr(out, rhs, indent + 2);
        }
        sem::Expr::Negate { kind, value, .. } => {
            let _ = writeln!(out, "negate {}", numeric_kind_label(*kind));
            dump_sem_expr(out, value, indent + 2);
        }
        sem::Expr::Convert { to, value, .. } => {
            let _ = writeln!(out, "convert to {}", numeric_kind_label(*to));
            dump_sem_expr(out, value, indent + 2);
        }
        sem::Expr::FloatCall { builtin, args, .. } => {
            let _ = writeln!(out, "float-call {}", builtin.name());
            for arg in args {
                dump_sem_expr(out, arg, indent + 2);
            }
        }
        sem::Expr::Compare {
            op, kind, lhs, rhs, ..
        } => {
            let _ = writeln!(out, "compare {op:?} {}", numeric_kind_label(*kind));
            dump_sem_expr(out, lhs, indent + 2);
            dump_sem_expr(out, rhs, indent + 2);
        }
        sem::Expr::MixedCompare {
            op,
            lhs_kind,
            lhs,
            rhs,
            ..
        } => {
            let _ = writeln!(
                out,
                "exact-compare {op:?} {}",
                numeric_kind_label(*lhs_kind)
            );
            dump_sem_expr(out, lhs, indent + 2);
            dump_sem_expr(out, rhs, indent + 2);
        }
        sem::Expr::ValueEqual { lhs, rhs, kind, .. } => {
            let _ = writeln!(out, "value-eq {}", sem_kind_label_for_expr(*kind));
            dump_sem_expr(out, lhs, indent + 2);
            dump_sem_expr(out, rhs, indent + 2);
        }
        sem::Expr::If {
            condition,
            refinement,
            then_branch,
            else_branch,
            ..
        } => {
            let _ = writeln!(out, "if");
            indent_line(out, indent + 2);
            let _ = writeln!(out, "condition");
            dump_sem_expr(out, condition, indent + 4);
            if let Some(refinement) = refinement {
                indent_line(out, indent + 2);
                let _ = writeln!(
                    out,
                    "refinement l{} => {}",
                    refinement.local.0,
                    sem_kind_label_for_expr(refinement.kind)
                );
            }
            indent_line(out, indent + 2);
            let _ = writeln!(out, "then");
            dump_sem_expr(out, then_branch, indent + 4);
            indent_line(out, indent + 2);
            let _ = writeln!(out, "else");
            dump_sem_expr(out, else_branch, indent + 4);
        }
        sem::Expr::Handle { body, restart, .. } => {
            let condition = restart
                .condition_local
                .map_or(String::from("none"), |local| format!("l{}", local.0));
            let _ = writeln!(
                out,
                "handle restart c{} condition={condition} result={}",
                restart.specializer.0,
                sem_kind_list_label_for_expr(&restart.result_kinds)
            );
            indent_line(out, indent + 2);
            let _ = writeln!(out, "body");
            dump_sem_expr(out, body, indent + 4);
            indent_line(out, indent + 2);
            let _ = writeln!(out, "restart");
            dump_sem_expr(out, &restart.body, indent + 4);
        }
        sem::Expr::Block { body, cleanup, .. } => {
            let _ = writeln!(out, "block-expr cleanup={}", cleanup.is_some());
            indent_line(out, indent + 2);
            let _ = writeln!(out, "body");
            dump_sem_block(out, body, indent + 4);
            if let Some(cleanup) = cleanup {
                indent_line(out, indent + 2);
                let _ = writeln!(out, "cleanup");
                dump_sem_block(out, cleanup, indent + 4);
            }
        }
        sem::Expr::Error { .. } => {
            let _ = writeln!(out, "error");
        }
    }
}

fn sem_class_origin_label(origin: sem::ClassOrigin) -> String {
    match origin {
        sem::ClassOrigin::ObjectRoot => "object-root".into(),
        sem::ClassOrigin::Native => "native".into(),
        sem::ClassOrigin::BuiltinValue { kind } => format!("builtin-value({})", kind.name()),
        sem::ClassOrigin::HostBacked { host_class } => {
            format!("host-backed(hc{})", host_class.0)
        }
    }
}

fn sem_generic_origin_label(origin: sem::GenericFunctionOrigin) -> String {
    match origin {
        sem::GenericFunctionOrigin::Source => "source".into(),
        sem::GenericFunctionOrigin::SlotSetter { class, slot } => {
            format!("slot-setter(c{}.s{})", class.0, slot.0)
        }
        sem::GenericFunctionOrigin::Numeric { operator } => {
            format!("numeric({}) sealed=<real>", operator.name())
        }
    }
}

fn sem_method_origin_label(origin: sem::MethodOrigin) -> String {
    match origin {
        sem::MethodOrigin::Source => "source".into(),
        sem::MethodOrigin::SlotSetter { class, slot } => {
            format!("slot-setter(c{}.s{})", class.0, slot.0)
        }
        sem::MethodOrigin::Session { sequence } => format!("session(#{sequence})"),
        sem::MethodOrigin::Numeric { operator } => format!("numeric({})", operator.name()),
    }
}

fn sem_superclass_label(module: &sem::Module, superclass: Option<sem::ClassId>) -> String {
    superclass.map_or_else(
        || "<none>".into(),
        |class| {
            module.classes.get(class.0 as usize).map_or_else(
                || format!("c{}:<missing>", class.0),
                |class| format!("c{}:{}", class.id.0, class.name),
            )
        },
    )
}

fn sem_superclass_chain_label(module: &sem::Module, class: sem::ClassId) -> String {
    let mut parts = Vec::new();
    for class_id in module.superclass_chain(class) {
        parts.push(module.classes.get(class_id.0 as usize).map_or_else(
            || format!("c{}:<missing>", class_id.0),
            |class| format!("c{}:{}", class.id.0, class.name),
        ));
    }
    parts.join(" -> ")
}

fn sem_effective_slot_source_label(class: &sem::Class, slot: &sem::EffectiveSlot) -> String {
    let source = if slot.source_class == class.id {
        "direct"
    } else {
        "inherited"
    };
    format!(
        "source={source} source-slot=c{}.s{}",
        slot.source_class.0, slot.source_slot.0
    )
}

fn sem_initialization_plan_summary(module: &sem::Module, class: &sem::Class) -> String {
    let steps = class
        .initialization_plan
        .steps
        .iter()
        .map(|step| sem_initialization_step_summary(module, step))
        .collect::<Vec<_>>();
    format!("[{}]", steps.join(","))
}

fn sem_initialization_keywords(class: &sem::Class) -> String {
    let keywords = class
        .initialization_plan
        .steps
        .iter()
        .map(|step| format!("{}:", step.init_keyword))
        .collect::<Vec<_>>();
    format!("[{}]", keywords.join(","))
}

fn sem_initialization_step_summary(module: &sem::Module, step: &sem::InitializationStep) -> String {
    format!(
        "s{}:{}:{}:{}:default={}:{}",
        step.slot.0,
        step.init_keyword,
        sem_initialization_source_label(step),
        sem_initialization_required_label(step),
        yes_no(step.has_default),
        sem_kind_label(module, step.kind)
    )
}

fn sem_initialization_step_detail(
    module: &sem::Module,
    class: &sem::Class,
    step: &sem::InitializationStep,
) -> String {
    let slot_name = class
        .effective_slots
        .get(step.slot.0 as usize)
        .map_or("<missing>", |slot| slot.name.as_str());
    format!(
        "slot={slot_name} keyword={}: source={} c{}.s{} kind={} type={} required={} default={} override=explicit-init-arg",
        step.init_keyword,
        sem_initialization_source_label(step),
        step.source_class.0,
        step.source_slot.0,
        sem_kind_label(module, step.kind),
        step.type_name,
        yes_no(step.required_init_keyword),
        yes_no(step.has_default)
    )
}

fn sem_initialization_source_label(step: &sem::InitializationStep) -> &'static str {
    if step.inherited {
        "inherited"
    } else {
        "direct"
    }
}

fn sem_initialization_required_label(step: &sem::InitializationStep) -> &'static str {
    if step.required_init_keyword {
        "required"
    } else {
        "optional"
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn dispatch_params_label(params: &[usize]) -> String {
    format!(
        "({})",
        params
            .iter()
            .map(|index| (index + 1).to_string())
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn sem_kind_vector_label(module: &sem::Module, kinds: &[sem::ValueKind]) -> String {
    format!(
        "({})",
        kinds
            .iter()
            .copied()
            .map(|kind| sem_kind_label(module, kind))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn exact_class_vector_label(module: &sem::Module, classes: &[Option<sem::ClassId>]) -> String {
    format!(
        "({})",
        classes
            .iter()
            .copied()
            .map(|class| class.map_or_else(|| "unknown".into(), |class| class_label(module, class)))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn distance_vector_label(distances: &[Option<usize>]) -> String {
    if distances.is_empty() {
        return "unknown".into();
    }
    format!(
        "({})",
        distances
            .iter()
            .map(|distance| distance
                .map_or_else(|| "unknown".into(), |distance| distance.to_string()))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn sem_dispatch_applicability_label(module: &sem::Module, specializer: sem::ClassId) -> String {
    let applicable = module
        .classes
        .iter()
        .filter_map(|class| {
            module
                .inheritance_distance(class.id, specializer)
                .map(|distance| format!("c{}:{}@{distance}", class.id.0, class.name))
        })
        .collect::<Vec<_>>();
    format!("[{}]", applicable.join(","))
}

fn sem_specializer_label(module: &sem::Module, specializer: sem::MethodSpecializer) -> String {
    specializer.map_or_else(|| "<object>".into(), |class| class_label(module, class))
}

fn sem_specializer_vector_label(
    module: &sem::Module,
    specializers: &[sem::MethodSpecializer],
) -> String {
    format!(
        "({})",
        specializers
            .iter()
            .copied()
            .map(|specializer| sem_specializer_label(module, specializer))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn sem_specializer_origin_vector_label(
    module: &sem::Module,
    specializers: &[sem::MethodSpecializer],
) -> String {
    format!(
        "({})",
        specializers
            .iter()
            .copied()
            .map(|specializer| {
                specializer.map_or_else(
                    || "top".into(),
                    |class| {
                        module.classes.get(class.0 as usize).map_or_else(
                            || "missing".into(),
                            |class| sem_class_origin_label(class.origin),
                        )
                    },
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn sem_dispatch_applicability_vector_label(
    module: &sem::Module,
    specializers: &[sem::MethodSpecializer],
) -> String {
    format!(
        "({})",
        specializers
            .iter()
            .copied()
            .map(|specializer| {
                specializer.map_or_else(
                    || "[<object>]".into(),
                    |class| sem_dispatch_applicability_label(module, class),
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn dispatch_methods(module: &sem::Module, generic: sem::GenericFunctionId) -> Vec<&sem::Method> {
    let mut methods = module
        .methods
        .iter()
        .filter(|method| method.generic == generic)
        .collect::<Vec<_>>();
    methods.sort_by(|left, right| {
        if module.method_more_specific(left, right) {
            core::cmp::Ordering::Less
        } else if module.method_more_specific(right, left) {
            core::cmp::Ordering::Greater
        } else {
            left.id.0.cmp(&right.id.0)
        }
    });
    methods
}

fn sem_slot_origin_label(origin: sem::SlotOrigin) -> String {
    match origin {
        sem::SlotOrigin::Native => "native".into(),
        sem::SlotOrigin::HostBacked {
            host_class,
            host_slot,
        } => format!("host-backed(hc{}.hs{})", host_class.0, host_slot.0),
    }
}

fn diff_artifacts(before: &SessionArtifacts, after: &SessionArtifacts) -> SessionDiff {
    let before_defs = definitions(&before.sem);
    let after_defs = definitions(&after.sem);
    let mut preserved = Vec::new();
    let mut changed = Vec::new();

    for before in &before_defs {
        if let Some(after) = after_defs
            .iter()
            .find(|candidate| candidate.report.id == before.report.id)
        {
            if before.fingerprint == after.fingerprint {
                preserved.push(after.report.clone());
            } else {
                changed.push(DefinitionChange {
                    definition: after.report.clone(),
                    change: DefinitionChangeKind::Modified,
                });
            }
        } else {
            changed.push(DefinitionChange {
                definition: before.report.clone(),
                change: DefinitionChangeKind::Removed,
            });
        }
    }
    for after in &after_defs {
        if !before_defs
            .iter()
            .any(|candidate| candidate.report.id == after.report.id)
        {
            changed.push(DefinitionChange {
                definition: after.report.clone(),
                change: DefinitionChangeKind::Added,
            });
        }
    }

    let affected = affected_artifacts(after, &changed);

    SessionDiff {
        preserved,
        changed,
        affected,
    }
}

fn affected_artifacts(
    after: &SessionArtifacts,
    changed: &[DefinitionChange],
) -> Vec<ArtifactReport> {
    if changed.is_empty() {
        return Vec::new();
    }
    let only_modified_methods = changed.iter().all(|change| {
        change.change == DefinitionChangeKind::Modified
            && change.definition.kind == DefinitionKind::Method
    });
    let mut graph = QueryInvalidation::for_module(&after.sem);
    let artifacts = if only_modified_methods {
        let mut artifacts = Vec::new();
        for change in changed {
            artifacts.extend(graph.method_definition_changed(change.definition.id));
        }
        artifacts
    } else {
        graph.source_changed()
    };
    let mut reports = Vec::new();
    for artifact in artifacts {
        let key = graph.key_for(artifact);
        if reports
            .iter()
            .all(|report: &ArtifactReport| report.key != key)
        {
            reports.push(ArtifactReport { artifact, key });
        }
    }
    reports
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DefinitionFingerprint {
    report: DefinitionReport,
    fingerprint: String,
}

fn definitions(module: &sem::Module) -> Vec<DefinitionFingerprint> {
    let mut definitions = Vec::new();
    for class in &module.classes {
        definitions.push(DefinitionFingerprint {
            report: DefinitionReport {
                id: class.definition,
                kind: DefinitionKind::Class,
                name: class.name.clone(),
            },
            fingerprint: format!(
                "class:{}:{}:super={}:direct={}:effective={}",
                class.name,
                sem_class_origin_label(class.origin),
                sem_superclass_label(module, class.superclass),
                class
                    .direct_slots
                    .iter()
                    .map(|slot| format!(
                        "{}={}:{}:accessor={}:{}:{}:{}:{}:{}",
                        slot.name,
                        sem_kind_fingerprint(module, slot.kind),
                        sem_slot_origin_label(slot.origin),
                        slot.accessor.definition.raw(),
                        slot.init_keyword,
                        slot.required_init_keyword,
                        slot_init_fingerprint(module, slot),
                        sem_slot_allocation_label(slot.allocation),
                        sem_slot_mutability_label(slot.mutability)
                    ))
                    .collect::<Vec<_>>()
                    .join(","),
                class
                    .effective_slots
                    .iter()
                    .map(|slot| format!(
                        "{}={}:{}:source=c{}.s{}:accessor={}:{}:{}:{}:{}:{}",
                        slot.name,
                        sem_kind_fingerprint(module, slot.kind),
                        sem_slot_origin_label(slot.origin),
                        slot.source_class.0,
                        slot.source_slot.0,
                        slot.accessor.definition.raw(),
                        slot.init_keyword,
                        slot.required_init_keyword,
                        effective_slot_init_fingerprint(module, slot),
                        sem_slot_allocation_label(slot.allocation),
                        sem_slot_mutability_label(slot.mutability)
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        });
        for slot in &class.direct_slots {
            definitions.push(DefinitionFingerprint {
                report: DefinitionReport {
                    id: slot.definition,
                    kind: DefinitionKind::Slot,
                    name: format!("{}.{}", class.name, slot.name),
                },
                fingerprint: format!(
                    "slot:{}:{}:{}:{}:accessor={}:{}:{}:{}:{}:{}",
                    class.name,
                    slot.name,
                    sem_kind_fingerprint(module, slot.kind),
                    sem_slot_origin_label(slot.origin),
                    slot.accessor.definition.raw(),
                    slot.init_keyword,
                    slot.required_init_keyword,
                    slot_init_fingerprint(module, slot),
                    sem_slot_allocation_label(slot.allocation),
                    sem_slot_mutability_label(slot.mutability)
                ),
            });
        }
    }
    for generic in &module.generics {
        definitions.push(DefinitionFingerprint {
            report: DefinitionReport {
                id: generic.definition,
                kind: DefinitionKind::Generic,
                name: generic.name.clone(),
            },
            fingerprint: format!(
                "generic:{}:{}:{}:{}",
                generic.name,
                generic
                    .param_kinds
                    .iter()
                    .map(|kind| kind.map_or_else(
                        || "untyped".into(),
                        |kind| sem_kind_fingerprint(module, kind)
                    ))
                    .collect::<Vec<_>>()
                    .join(","),
                generic.return_annotation.as_ref().map_or_else(
                    || "inferred".into(),
                    |kinds| sem_kind_list_fingerprint(module, kinds)
                ),
                generic.arity
            ),
        });
    }
    for method in &module.methods {
        let generic = module
            .generics
            .get(method.generic.0 as usize)
            .map_or("?", |generic| generic.name.as_str());
        let specializers = sem_specializer_vector_label(module, &method.specializers);
        definitions.push(DefinitionFingerprint {
            report: DefinitionReport {
                id: method.definition,
                kind: DefinitionKind::Method,
                name: format!("{generic}{specializers}"),
            },
            fingerprint: format!(
                "method:{generic}:{specializers}:{}:{}",
                method.return_annotation.as_ref().map_or_else(
                    || "inferred".into(),
                    |kinds| sem_kind_list_fingerprint(module, kinds)
                ),
                fingerprint_sem_block(module, &method.body)
            ),
        });
    }
    for function in &module.functions {
        definitions.push(DefinitionFingerprint {
            report: DefinitionReport {
                id: function.definition,
                kind: DefinitionKind::Function,
                name: function.name.clone(),
            },
            fingerprint: format!(
                "function:{}:{}:{}:{}:{}",
                function.name,
                function.params.len(),
                function
                    .param_kinds
                    .iter()
                    .map(|kind| sem_kind_fingerprint(module, *kind))
                    .collect::<Vec<_>>()
                    .join(","),
                function.return_annotation.as_ref().map_or_else(
                    || "inferred".into(),
                    |kinds| sem_kind_list_fingerprint(module, kinds)
                ),
                fingerprint_sem_block(module, &function.body)
            ),
        });
    }
    definitions
}

fn slot_init_fingerprint(module: &sem::Module, slot: &sem::Slot) -> String {
    let Some(init_value) = &slot.init_value else {
        return "none".into();
    };
    let mut out = String::new();
    fingerprint_sem_expr(module, init_value, &mut out);
    out
}

fn effective_slot_init_fingerprint(module: &sem::Module, slot: &sem::EffectiveSlot) -> String {
    let Some(init_value) = &slot.init_value else {
        return "none".into();
    };
    let mut out = String::new();
    fingerprint_sem_expr(module, init_value, &mut out);
    out
}

fn fingerprint_sem_block(module: &sem::Module, block: &sem::Block) -> String {
    let mut out = String::new();
    for binding in &block.lets {
        out.push_str("let:");
        for local in &binding.locals {
            let _ = write!(out, "{};", local.0);
        }
        out.push('=');
        fingerprint_sem_expr(module, &binding.value, &mut out);
        out.push(';');
    }
    out.push_str("result=");
    fingerprint_sem_expr(module, &block.result, &mut out);
    out
}

fn fingerprint_sem_expr(module: &sem::Module, expr: &sem::Expr, out: &mut String) {
    match expr {
        sem::Expr::ConstI64 { value, .. } => {
            let _ = write!(out, "i64({value})");
        }
        sem::Expr::ConstF64 { value, .. } => {
            let _ = write!(out, "f64({:#x})", value.to_bits());
        }
        sem::Expr::ConstBool { value, .. } => {
            let _ = write!(out, "bool({value})");
        }
        sem::Expr::StringLiteral { value, class, .. } => {
            let definition = module
                .classes
                .get(class.0 as usize)
                .map_or(0, |class| class.definition.raw());
            let _ = write!(out, "string({definition}:{value:?})");
        }
        sem::Expr::SymbolLiteral { name, class, .. } => {
            let definition = module
                .classes
                .get(class.0 as usize)
                .map_or(0, |class| class.definition.raw());
            let _ = write!(out, "symbol({definition}:#{name})");
        }
        sem::Expr::VectorLiteral {
            elements, class, ..
        } => {
            let definition = module
                .classes
                .get(class.0 as usize)
                .map_or(0, |class| class.definition.raw());
            let _ = write!(out, "vector({definition}:");
            fingerprint_args(module, elements, out);
            out.push(')');
        }
        sem::Expr::Values {
            values,
            result_kinds,
            ..
        } => {
            out.push_str("values(");
            for kind in result_kinds {
                let _ = write!(out, "{};", sem_kind_fingerprint(module, *kind));
            }
            out.push(':');
            fingerprint_args(module, values, out);
            out.push(')');
        }
        sem::Expr::Local {
            local, result_kind, ..
        } => {
            let result = sem_kind_fingerprint(module, *result_kind);
            let _ = write!(out, "local({}:{result})", local.0);
        }
        sem::Expr::Capture {
            capture,
            result_kind,
            ..
        } => {
            let result = sem_kind_fingerprint(module, *result_kind);
            let _ = write!(out, "capture({}:{result})", capture.0);
        }
        sem::Expr::ConstantRef {
            constant,
            result_kind,
            ..
        } => {
            let result = sem_kind_fingerprint(module, *result_kind);
            let definition = module
                .constants
                .get(constant.0 as usize)
                .map_or(0, |constant| constant.definition.raw());
            let _ = write!(out, "constant({definition}:{result})");
        }
        sem::Expr::ClassObject { class, .. } => {
            let definition = module
                .classes
                .get(class.0 as usize)
                .map_or(0, |class| class.definition.raw());
            let _ = write!(out, "class-object({definition})");
        }
        sem::Expr::KnownCallable { callable, .. } => {
            let fingerprint = sem_callable_fingerprint(module, *callable);
            let _ = write!(out, "known-callable({fingerprint})");
        }
        sem::Expr::Closure {
            closure,
            captures,
            result_kind,
            ..
        } => {
            let result = sem_kind_fingerprint(module, *result_kind);
            let _ = write!(out, "closure({}:{result}:", closure.0);
            for capture in captures {
                let kind = sem_kind_fingerprint(module, capture.kind);
                let _ = write!(out, "{}:{}:{kind},", capture.id.0, capture.source.0);
            }
            out.push(')');
        }
        sem::Expr::GenericFunctionObject { generic, .. } => {
            let definition = module
                .generics
                .get(generic.0 as usize)
                .map_or(0, |generic| generic.definition.raw());
            let _ = write!(out, "generic-function-object({definition})");
        }
        sem::Expr::IntrinsicCall {
            intrinsic,
            args,
            result_kind,
            ..
        } => {
            let result = sem_kind_fingerprint(module, *result_kind);
            let _ = write!(
                out,
                "intrinsic({}:{result}:",
                sem_intrinsic_label(*intrinsic)
            );
            fingerprint_args(module, args, out);
            out.push(')');
        }
        sem::Expr::HostCall {
            function,
            args,
            result_kind,
            ..
        } => {
            let result = sem_kind_fingerprint(module, *result_kind);
            let name = module
                .host_functions
                .get(function.0 as usize)
                .map_or("?", |function| function.name.as_str());
            let _ = write!(out, "host-call({name}:{result}:");
            fingerprint_args(module, args, out);
            out.push(')');
        }
        sem::Expr::DirectCall { function, args, .. } => {
            let definition = module
                .functions
                .get(function.0 as usize)
                .map_or(0, |function| function.definition.raw());
            let _ = write!(out, "call({definition}:");
            fingerprint_args(module, args, out);
            out.push(')');
        }
        sem::Expr::CallableCall {
            callable,
            args,
            result_kinds,
            ..
        } => {
            let result = sem_kind_list_fingerprint(module, result_kinds);
            let _ = write!(out, "callable({result}:");
            fingerprint_sem_expr(module, callable, out);
            out.push(':');
            fingerprint_args(module, args, out);
            out.push(')');
        }
        sem::Expr::GenericCall { generic, args, .. } => {
            let definition = module
                .generics
                .get(generic.0 as usize)
                .map_or(0, |generic| generic.definition.raw());
            let _ = write!(out, "generic({definition}:");
            fingerprint_args(module, args, out);
            out.push(')');
        }
        sem::Expr::Signal {
            condition,
            condition_class,
            result_kinds,
            ..
        } => {
            let definition = module
                .classes
                .get(condition_class.0 as usize)
                .map_or(0, |class| class.definition.raw());
            let _ = write!(out, "signal({definition}:{result_kinds:?}:");
            fingerprint_sem_expr(module, condition, out);
            out.push(')');
        }
        sem::Expr::ClassOf { object, class, .. } => {
            let definition = module
                .classes
                .get(class.0 as usize)
                .map_or(0, |class| class.definition.raw());
            let _ = write!(out, "class-of({definition}:");
            fingerprint_sem_expr(module, object, out);
            out.push(')');
        }
        sem::Expr::InstanceOf { value, class, .. } => {
            let definition = module
                .classes
                .get(class.0 as usize)
                .map_or(0, |class| class.definition.raw());
            let _ = write!(out, "instance?({definition}:");
            fingerprint_sem_expr(module, value, out);
            out.push(')');
        }
        sem::Expr::Cast { value, class, .. } => {
            let definition = module
                .classes
                .get(class.0 as usize)
                .map_or(0, |class| class.definition.raw());
            let _ = write!(out, "as({definition}:");
            fingerprint_sem_expr(module, value, out);
            out.push(')');
        }
        sem::Expr::MakeObject {
            class,
            class_id,
            init_args,
            fields,
            ..
        } => {
            let definition = module
                .classes
                .get(class_id.0 as usize)
                .map_or(0, |class| class.definition.raw());
            let _ = write!(out, "make({definition}:");
            fingerprint_sem_expr(module, class, out);
            out.push(':');
            for arg in init_args {
                let _ = write!(out, "arg{}:slot{}=", arg.keyword, arg.slot.0);
                fingerprint_sem_expr(module, &arg.value, out);
                out.push(',');
            }
            out.push(':');
            for field in fields {
                let kind = module
                    .classes
                    .get(class_id.0 as usize)
                    .and_then(|class| class.effective_slots.get(field.slot.0 as usize))
                    .map_or(String::from("error"), |slot| {
                        sem_kind_fingerprint(module, slot.kind)
                    });
                let _ = write!(
                    out,
                    "slot{}:{}:{kind}=",
                    field.slot.0,
                    sem_field_init_source_label(field.source)
                );
                fingerprint_sem_expr(module, &field.value, out);
                out.push(',');
            }
            out.push(')');
        }
        sem::Expr::AccessorCall {
            accessor,
            receiver,
            class,
            slot,
            result_kind,
            ..
        } => {
            let class_definition = module
                .classes
                .get(class.0 as usize)
                .map_or(0, |class| class.definition.raw());
            let slot_definition = module
                .classes
                .get(class.0 as usize)
                .and_then(|class| class.effective_slots.get(slot.0 as usize))
                .map_or(0, |slot| slot.definition.raw());
            let result = sem_kind_fingerprint(module, *result_kind);
            let _ = write!(
                out,
                "accessor({}:{}.{}:{result}:",
                accessor.raw(),
                class_definition,
                slot_definition
            );
            fingerprint_sem_expr(module, receiver, out);
            out.push(')');
        }
        sem::Expr::SlotWrite {
            setter,
            receiver,
            value,
            class,
            slot,
            result_kind,
            ..
        } => {
            let class_definition = module
                .classes
                .get(class.0 as usize)
                .map_or(0, |class| class.definition.raw());
            let slot_definition = module
                .classes
                .get(class.0 as usize)
                .and_then(|class| class.effective_slots.get(slot.0 as usize))
                .map_or(0, |slot| slot.definition.raw());
            let result = sem_kind_fingerprint(module, *result_kind);
            let _ = write!(
                out,
                "slot-write({}:{}.{}:{result}:",
                setter.raw(),
                class_definition,
                slot_definition
            );
            fingerprint_sem_expr(module, receiver, out);
            out.push(',');
            fingerprint_sem_expr(module, value, out);
            out.push(')');
        }
        sem::Expr::Negate { kind, value, .. } => {
            let _ = write!(out, "negate({}:", numeric_kind_label(*kind));
            fingerprint_sem_expr(module, value, out);
            out.push(')');
        }
        sem::Expr::Convert { to, value, .. } => {
            let _ = write!(out, "convert({}:", numeric_kind_label(*to));
            fingerprint_sem_expr(module, value, out);
            out.push(')');
        }
        sem::Expr::FloatCall { builtin, args, .. } => {
            let _ = write!(out, "float-call({}:", builtin.name());
            for arg in args {
                fingerprint_sem_expr(module, arg, out);
                out.push(',');
            }
            out.push(')');
        }
        sem::Expr::Binary {
            op, kind, lhs, rhs, ..
        } => {
            let _ = write!(out, "binary({op:?}:{}:", numeric_kind_label(*kind));
            fingerprint_sem_expr(module, lhs, out);
            out.push(',');
            fingerprint_sem_expr(module, rhs, out);
            out.push(')');
        }
        sem::Expr::Compare {
            op, kind, lhs, rhs, ..
        } => {
            let _ = write!(out, "compare({op:?}:{}:", numeric_kind_label(*kind));
            fingerprint_sem_expr(module, lhs, out);
            out.push(',');
            fingerprint_sem_expr(module, rhs, out);
            out.push(')');
        }
        sem::Expr::MixedCompare {
            op,
            lhs_kind,
            lhs,
            rhs,
            ..
        } => {
            let _ = write!(
                out,
                "exact-compare({op:?}:{}:",
                numeric_kind_label(*lhs_kind)
            );
            fingerprint_sem_expr(module, lhs, out);
            out.push(',');
            fingerprint_sem_expr(module, rhs, out);
            out.push(')');
        }
        sem::Expr::ValueEqual { lhs, rhs, kind, .. } => {
            let kind = sem_kind_fingerprint(module, *kind);
            let _ = write!(out, "value-eq({kind}:");
            fingerprint_sem_expr(module, lhs, out);
            out.push(',');
            fingerprint_sem_expr(module, rhs, out);
            out.push(')');
        }
        sem::Expr::If {
            condition,
            refinement,
            then_branch,
            else_branch,
            ..
        } => {
            out.push_str("if(");
            fingerprint_sem_expr(module, condition, out);
            if let Some(refinement) = refinement {
                let kind = sem_kind_fingerprint(module, refinement.kind);
                let _ = write!(out, "|refine{}={kind}", refinement.local.0);
            }
            out.push('?');
            fingerprint_sem_expr(module, then_branch, out);
            out.push(':');
            fingerprint_sem_expr(module, else_branch, out);
            out.push(')');
        }
        sem::Expr::Handle { body, restart, .. } => {
            let definition = module
                .classes
                .get(restart.specializer.0 as usize)
                .map_or(0, |class| class.definition.raw());
            let condition = restart.condition_local.map_or(u32::MAX, |local| local.0);
            let _ = write!(
                out,
                "handle({definition}:{:?}:condition={condition}:body=",
                restart.result_kinds
            );
            fingerprint_sem_expr(module, body, out);
            out.push_str(":restart=");
            fingerprint_sem_expr(module, &restart.body, out);
            out.push(')');
        }
        sem::Expr::Block { body, cleanup, .. } => {
            out.push_str("block(body=");
            out.push_str(&fingerprint_sem_block(module, body));
            if let Some(cleanup) = cleanup {
                out.push_str(":cleanup=");
                out.push_str(&fingerprint_sem_block(module, cleanup));
            }
            out.push(')');
        }
        sem::Expr::Error { .. } => {
            out.push_str("error");
        }
    }
}

fn fingerprint_args(module: &sem::Module, args: &[sem::Expr], out: &mut String) {
    for arg in args {
        fingerprint_sem_expr(module, arg, out);
        out.push(',');
    }
}

fn artifact_label(artifact: QueryArtifact) -> String {
    artifact.label()
}

fn sem_field_init_source_label(source: sem::FieldInitSource) -> &'static str {
    match source {
        sem::FieldInitSource::Explicit => "explicit",
        sem::FieldInitSource::Default => "default",
    }
}

fn indent_line(out: &mut String, indent: usize) {
    for _ in 0..indent {
        out.push(' ');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_scalar(
        artifacts: &SessionArtifacts,
        name: &str,
    ) -> Result<FunctionValue, FunctionCallError> {
        artifacts
            .call_function(name, &[], None)
            .map(|outcome| outcome.value)
    }

    const AREA: &str = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                        define generic area(x);\n\
                        define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
                        define function main()\n  area(make(<circle>, radius: 10))\nend;";
    const SIMPLE_MAIN: &str = "define function main()\n  42\nend";

    fn eop_object<'a>(objects: &'a [EnvironmentObject], id: &str) -> &'a EnvironmentObject {
        objects
            .iter()
            .find(|object| object.id.as_str() == id)
            .expect("environment object should exist")
    }

    fn doc_lines(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| (*line).to_string()).collect()
    }

    fn class_id(artifacts: &SessionArtifacts, name: &str) -> sem::ClassId {
        artifacts
            .sem
            .classes
            .iter()
            .find(|class| class.name == name)
            .map(|class| class.id)
            .expect("class should exist")
    }

    fn method_for_class(artifacts: &SessionArtifacts, name: &str) -> sem::MethodId {
        let class = class_id(artifacts, name);
        artifacts
            .sem
            .methods
            .iter()
            .find(|method| method.specializer == class)
            .map(|method| method.id)
            .expect("method should exist")
    }

    #[test]
    fn runtime_result_display_preserves_trap_details() {
        let result = RuntimeResult::Trap {
            trap: TrapInfo {
                unit: execution_tape::value::UnitId(0),
                func: execution_tape::value::FuncId(3),
                pc: 11,
                span_id: Some(17),
                trap: execution_tape::vm::Trap::HostCallFailed {
                    symbol: "interloko.slot_get".into(),
                    sig_hash: execution_tape::host::SigHash(456),
                    error: execution_tape::host::HostError::Failed,
                },
            },
        };
        let rendered = format!("{result}");

        assert!(rendered.contains("trap at u0:f3 pc=11 span=17"));
        assert!(rendered.contains("host_call=interloko.slot_get"));
        assert!(rendered.contains("sig_hash=456"));
        assert!(rendered.contains("host call failed"));
    }

    #[test]
    fn execution_trace_view_only_resolves_the_recorded_program() {
        let traced = CompilerSession::compile_source(
            "area.loko",
            AREA,
            RuntimeMode::RunMainTraced {
                detail: ExecutionTraceDetail::Boundaries,
            },
        );
        let trace = traced.runtime_trace.as_ref().expect("trace was requested");
        let shared = artifacts_for_mode(
            &traced,
            RuntimeMode::RunMainTraced {
                detail: ExecutionTraceDetail::Boundaries,
            },
        );
        assert!(shared.execution_trace_view(trace).is_resolved());

        let recompiled = CompilerSession::compile_source("area.loko", AREA, RuntimeMode::RunMain);
        let foreign = recompiled.execution_trace_view(trace);
        assert!(!foreign.is_resolved());
        let entry = foreign.entry().expect("entry was captured");
        assert!(entry.name().is_none());
        assert!(foreign.render().contains(&format!(
            "invocation: u{}:f{}",
            entry.id().unit.0,
            entry.id().func.0
        )));
    }

    #[test]
    fn environment_affordance_names_are_stable() {
        let names = [
            (EnvironmentAffordance::Inspect, "inspect"),
            (EnvironmentAffordance::DumpSyntax, "dump-syntax"),
            (EnvironmentAffordance::DumpAst, "dump-ast"),
            (EnvironmentAffordance::DumpSem, "dump-sem"),
            (EnvironmentAffordance::DumpMop, "dump-mop"),
            (EnvironmentAffordance::DumpEop, "dump-eop"),
            (EnvironmentAffordance::DumpLowered, "dump-lowered"),
            (EnvironmentAffordance::DumpTape, "dump-tape"),
            (EnvironmentAffordance::ShowDispatch, "show-dispatch"),
            (
                EnvironmentAffordance::ShowRuntimeLayout,
                "show-runtime-layout",
            ),
            (EnvironmentAffordance::ShowDependencies, "show-dependencies"),
            (
                EnvironmentAffordance::ShowDocumentation,
                "show-documentation",
            ),
            (EnvironmentAffordance::ShowAccessor, "show-accessor"),
            (EnvironmentAffordance::ShowSlot, "show-slot"),
            (EnvironmentAffordance::Run, "run"),
            (EnvironmentAffordance::EditBody, "edit-body"),
            (EnvironmentAffordance::AddMethod, "add-method"),
            (EnvironmentAffordance::Artifacts, "artifacts"),
            (EnvironmentAffordance::Diff, "diff"),
            (EnvironmentAffordance::OpenSource, "open-source"),
            (EnvironmentAffordance::DumpArtifacts, "dump-artifacts"),
            (EnvironmentAffordance::ShowSlots, "show-slots"),
            (
                EnvironmentAffordance::ShowGrammarStack,
                "show-grammar-stack",
            ),
            (EnvironmentAffordance::ShowGrammars, "show-grammars"),
            (EnvironmentAffordance::ShowDirectRules, "show-direct-rules"),
            (
                EnvironmentAffordance::ShowEffectiveRules,
                "show-effective-rules",
            ),
            (EnvironmentAffordance::ShowActivations, "show-activations"),
            (EnvironmentAffordance::ShowGrammarRule, "show-grammar-rule"),
            (EnvironmentAffordance::ShowExpression, "show-expression"),
            (EnvironmentAffordance::ShowAction, "show-action"),
            (EnvironmentAffordance::ShowCaptures, "show-captures"),
            (
                EnvironmentAffordance::ShowActionOutput,
                "show-action-output",
            ),
            (EnvironmentAffordance::ShowParseTrace, "show-parse-trace"),
            (
                EnvironmentAffordance::ShowParserShadow,
                "show-parser-shadow",
            ),
            (
                EnvironmentAffordance::ShowGrammarRegistry,
                "show-grammar-registry",
            ),
            (
                EnvironmentAffordance::ShowGrammarExtension,
                "show-grammar-extension",
            ),
            (EnvironmentAffordance::ShowTyping, "show-typing"),
        ];

        let all = EnvironmentAffordance::all();
        assert_eq!(all.len(), names.len());
        for (affordance, name) in names {
            assert_eq!(affordance.as_str(), name);
            assert_eq!(EnvironmentAffordance::from_name(name), Some(affordance));
            assert!(all.contains(&affordance));
        }
        assert_eq!(EnvironmentAffordance::from_name("bogus"), None);
    }

    #[test]
    fn session_reports_top_level_form_spans_from_syntax() {
        let source = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                      define generic area(x :: <object>) => <integer>;\n\
                      define method area(c :: <circle>) => <integer>\n  c.radius\nend\n\
                      define function main()\n  1\nend\n";
        let artifacts =
            CompilerSession::compile_source("forms.loko", source, RuntimeMode::CompileOnly);

        let forms = artifacts.top_level_forms();
        assert_eq!(
            forms.iter().map(|form| form.kind).collect::<Vec<_>>(),
            Vec::from([
                TopLevelFormKind::Class,
                TopLevelFormKind::Generic,
                TopLevelFormKind::Method,
                TopLevelFormKind::Function,
            ])
        );
        for form in &forms {
            assert!(
                artifacts
                    .grammar_parse
                    .provenance
                    .applications
                    .iter()
                    .any(|application| application.span == form.span),
                "missing rule application for {form:?}"
            );
        }
        assert_eq!(
            &artifacts.syntax.source[forms[0].span.start..forms[0].span.end],
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;"
        );
        assert_eq!(
            &artifacts.syntax.source[forms[1].span.start..forms[1].span.end],
            "define generic area(x :: <object>) => <integer>;"
        );
        assert_eq!(
            &artifacts.syntax.source[forms[2].span.start..forms[2].span.end],
            "define method area(c :: <circle>) => <integer>\n  c.radius\nend"
        );
        assert_eq!(
            &artifacts.syntax.source[forms[3].span.start..forms[3].span.end],
            "define function main()\n  1\nend"
        );
    }

    #[test]
    fn session_reports_unified_source_forms_for_base_and_staged_inputs() {
        let source = "// grammar-activate: records\n\
                      // staged-loko: records\n\
                      record circle\n\
                        radius: <integer>\n\
                      end\n\
                      // end-staged-loko\n\
                      define function main()\n\
                        1\n\
                      end\n";
        let artifacts =
            CompilerSession::compile_source("source-forms.loko", source, RuntimeMode::CompileOnly);
        let forms = artifacts.source_forms();

        assert!(forms.iter().any(|form| form.id == "form:f0"
            && form.kind == SourceFormKind::Function
            && form.origin == SourceFormOrigin::BaseCst));
        let staged = forms
            .iter()
            .find(|form| form.id == "staged-form:r0/f0")
            .expect("staged records form");
        assert_eq!(staged.kind, SourceFormKind::FrontendClass);
        assert_eq!(
            staged.origin,
            SourceFormOrigin::StagedGrammar {
                region: "r0".to_string(),
                form_index: 0,
            }
        );
        assert!(
            staged
                .rule_applications
                .iter()
                .any(|application| application.rule_name == "record-definition")
        );
        assert!(
            staged
                .produced_object_ids
                .iter()
                .any(|id| id.starts_with("class:c"))
        );
        assert!(staged.produced_object_ids.iter().all(|id| id != "class:c0"));
    }

    #[test]
    fn session_projects_constant_source_forms_and_environment_objects() {
        let source = "define constant $answer = 42;\n\
                      define function main()\n  $answer\nend\n";
        let artifacts =
            CompilerSession::compile_source("constant.loko", source, RuntimeMode::RunMain);

        assert!(matches!(
            artifacts.runtime_result,
            Some(RuntimeResult::Returned(FunctionValue::Integer(42)))
        ));
        let forms = artifacts.source_forms();
        let constant_form = forms
            .iter()
            .find(|form| form.kind == SourceFormKind::Constant)
            .expect("constant source form");
        assert!(
            constant_form
                .produced_object_ids
                .iter()
                .any(|id| id == "constant:k0")
        );

        let constant = artifacts
            .environment_objects()
            .into_iter()
            .find(|object| object.id.as_str() == "constant:k0")
            .expect("constant object");
        assert_eq!(constant.kind, EnvironmentKind::Constant);
        assert_eq!(
            constant.source.definition,
            Some(artifacts.sem.constants[0].definition)
        );
        assert!(
            constant
                .artifacts
                .keys
                .iter()
                .any(|key| key.kind == crate::identity::ArtifactKind::Bind)
        );
    }

    #[test]
    fn session_attaches_doc_comments_to_source_forms_and_eop_objects() {
        let source = "/// Circle docs\n\
                      /// Carries a radius.\n\
                      define class <circle> (<object>)\nend;\n\
                      /// Area dispatcher.\n\
                      define generic area(x :: <object>) => <integer>;\n\
                      // ordinary comments do not attach\n\
                      /// Area method.\n\
                      define method area(x :: <object>) => <integer>\n  1\nend\n\
                      /// Helper function.\n\
                      define function helper()\n  2\nend\n\
                      /// Answer constant.\n\
                      define constant $answer = 42;\n";
        let artifacts =
            CompilerSession::compile_source("docs.loko", source, RuntimeMode::CompileOnly);
        let forms = artifacts.source_forms();

        let assert_form_docs = |kind, lines: &[&str]| {
            let form = forms
                .iter()
                .find(|form| form.kind == kind)
                .expect("source form should exist");
            assert_eq!(form.doc_comments, doc_lines(lines));
        };
        assert_form_docs(SourceFormKind::Class, &["Circle docs", "Carries a radius."]);
        assert_form_docs(SourceFormKind::Generic, &["Area dispatcher."]);
        assert_form_docs(SourceFormKind::Method, &["Area method."]);
        assert_form_docs(SourceFormKind::Function, &["Helper function."]);
        assert_form_docs(SourceFormKind::Constant, &["Answer constant."]);

        let objects = artifacts.environment_objects();
        assert_eq!(
            eop_object(&objects, "class:c1").documentation.lines,
            doc_lines(&["Circle docs", "Carries a radius."])
        );
        assert_eq!(
            eop_object(&objects, "generic:g0").documentation.lines,
            doc_lines(&["Area dispatcher."])
        );
        assert_eq!(
            eop_object(&objects, "method:m0").documentation.lines,
            doc_lines(&["Area method."])
        );
        assert_eq!(
            eop_object(&objects, "function:f0").documentation.lines,
            doc_lines(&["Helper function."])
        );
        assert!(
            eop_object(&objects, "function:f0")
                .affordances
                .contains(EnvironmentAffordance::ShowDocumentation)
        );
        assert_eq!(
            eop_object(&objects, "constant:k0").documentation.lines,
            doc_lines(&["Answer constant."])
        );

        let eop = artifacts
            .dump_environment_object_stage("function:f0", DumpStage::Eop)
            .expect("function eop dump");
        assert!(eop.contains("documentation:"));
        assert!(eop.contains("[Helper function.]"));
    }

    #[test]
    fn documentation_comments_normalize_windows_line_endings() {
        let artifacts = CompilerSession::compile_source(
            "windows-docs.loko",
            "/// Helper docs\r\ndefine function helper() 1 end;\r\n",
            RuntimeMode::CompileOnly,
        );
        let object = artifacts
            .environment_objects()
            .into_iter()
            .find(|object| object.id.as_str() == "function:f0")
            .unwrap();
        assert_eq!(object.documentation.lines, doc_lines(&["Helper docs"]));
        assert!(
            artifacts
                .dump_stage(DumpStage::Eop)
                .contains("documentation [Helper docs]")
        );
    }

    #[test]
    fn session_answers_syntax_tooling_queries_by_span() {
        let source = "define function main()\n  40 + 2\nend\n";
        let artifacts =
            CompilerSession::compile_source("tooling.loko", source, RuntimeMode::CompileOnly);
        let expression_start = source.find("40 + 2").expect("expression text present");
        let expression_span = Span::new(expression_start, expression_start + "40 + 2".len());

        let parse_artifact = artifacts.frontend_parse_artifact();
        assert_eq!(parse_artifact.source_file, artifacts.source.id);
        assert!(parse_artifact.parse_tree.is_some());

        let nodes = artifacts.syntax_nodes_containing_span(expression_span);
        assert_eq!(nodes.first().map(|node| node.kind), Some(CstKind::Root));
        assert!(
            nodes
                .iter()
                .any(|node| node.kind == CstKind::Function && node.depth == 1)
        );
        assert!(nodes.iter().any(|node| node.kind == CstKind::BinaryExpr));

        let tokens = artifacts.syntax_tokens_intersecting_span(expression_span);
        assert_eq!(
            tokens
                .iter()
                .map(|token| (token.kind, token.text.as_str()))
                .collect::<Vec<_>>(),
            Vec::from([
                (TokenKind::Number, "40"),
                (TokenKind::Plus, "+"),
                (TokenKind::Number, "2"),
            ])
        );

        let cursor = Span::new(expression_start, expression_start);
        let cursor_tokens = artifacts.syntax_tokens_intersecting_span(cursor);
        assert_eq!(cursor_tokens.len(), 1);
        assert_eq!(cursor_tokens[0].text, "40");

        let applications = artifacts.grammar_rule_applications_containing_span(expression_span);
        assert_eq!(
            applications
                .first()
                .map(|application| application.rule_name.as_str()),
            Some("body")
        );
        assert!(
            applications
                .iter()
                .any(|application| application.rule_name == "function-definition")
        );

        let view = artifacts.parse_view_for_span(expression_span);
        assert_eq!(view.span, expression_span);
        assert_eq!(view.nodes, nodes);
        assert_eq!(view.tokens, tokens);
        assert!(
            view.rule_applications
                .iter()
                .any(|application| application.rule_name == "body")
        );
        assert!(
            view.rule_applications
                .iter()
                .any(|application| application.rule_name == "function-definition")
        );
        assert!(view.diagnostics.is_empty());
    }

    #[test]
    fn session_answers_trivia_tooling_queries_by_span() {
        let source =
            "define function main()\n  let x = 1; // trailing\n  /// body comment\n  x\nend\n";
        let artifacts =
            CompilerSession::compile_source("trivia.loko", source, RuntimeMode::CompileOnly);
        assert!(
            artifacts.diagnostics.is_empty(),
            "comments should parse as trivia: {:?}",
            artifacts.diagnostics
        );

        let doc_start = source
            .find("/// body comment")
            .expect("doc comment present");
        let doc_span = Span::new(doc_start, doc_start + "/// body comment".len());
        assert!(
            artifacts
                .syntax_tokens_intersecting_span(doc_span)
                .is_empty()
        );

        let trivia = artifacts.syntax_trivia_intersecting_span(doc_span);
        assert_eq!(
            trivia
                .iter()
                .map(|token| (token.kind, token.text.as_str()))
                .collect::<Vec<_>>(),
            Vec::from([(TokenKind::DocComment, "/// body comment")])
        );

        let view = artifacts.parse_view_for_span(doc_span);
        assert!(view.tokens.is_empty());
        assert_eq!(view.trivia, trivia);
        assert!(view.nodes.iter().any(|node| node.kind == CstKind::Function));
    }

    #[test]
    fn syntax_tooling_preserves_keyword_context_roles() {
        let source = "define class <circle> (<object>)\n  slot radius :: <integer>, required-init-keyword: r:;\nend;\n\
                      define function main()\n  let keyword = fallback:;\n  make(<circle>, r: 1)\nend";
        let artifacts =
            CompilerSession::compile_source("keyword-roles.loko", source, RuntimeMode::CompileOnly);

        let expression_keyword_start = source
            .find("fallback:")
            .expect("expression keyword symbol present");
        let expression_keyword_span = Span::new(
            expression_keyword_start,
            expression_keyword_start + "fallback:".len(),
        );
        let expression_keyword_nodes =
            artifacts.syntax_nodes_containing_span(expression_keyword_span);
        assert!(
            expression_keyword_nodes
                .iter()
                .any(|node| node.kind == CstKind::KeywordExpr)
        );
        assert!(
            artifacts
                .grammar_rule_applications_containing_span(expression_keyword_span)
                .iter()
                .any(|application| application.rule_name == "keyword-symbol")
        );

        let slot_keyword_context = source
            .find("required-init-keyword: r:")
            .expect("slot keyword option present");
        let slot_keyword_start = slot_keyword_context + "required-init-keyword: ".len();
        let slot_keyword_span = Span::new(slot_keyword_start, slot_keyword_start + "r:".len());
        let slot_keyword_nodes = artifacts.syntax_nodes_containing_span(slot_keyword_span);
        assert!(
            slot_keyword_nodes
                .iter()
                .any(|node| node.kind == CstKind::SlotKeyword)
        );
        assert!(
            artifacts
                .grammar_rule_applications_containing_span(slot_keyword_span)
                .iter()
                .any(|application| application.rule_name == "slot-keyword")
        );

        let argument_keyword_start = source.rfind("r: 1").expect("argument keyword present");
        let argument_keyword_span =
            Span::new(argument_keyword_start, argument_keyword_start + "r:".len());
        let argument_keyword_nodes = artifacts.syntax_nodes_containing_span(argument_keyword_span);
        assert!(
            argument_keyword_nodes
                .iter()
                .any(|node| node.kind == CstKind::ArgumentKeyword)
        );
        assert!(
            artifacts
                .grammar_rule_applications_containing_span(argument_keyword_span)
                .iter()
                .any(|application| application.rule_name == "argument-keyword")
        );
    }

    #[test]
    fn syntax_tooling_preserves_name_and_type_context_roles() {
        let source = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                      define function main(c :: class <circle>) => <integer>\n  let r :: <integer> = 1;\n  let t = <circle>;\n  make(<circle>, radius: r).radius\nend";
        let artifacts = CompilerSession::compile_source(
            "name-type-roles.loko",
            source,
            RuntimeMode::CompileOnly,
        );

        let assert_role = |start: usize, text: &str, kind: CstKind, rule_name: &str| {
            let span = Span::new(start, start + text.len());
            let nodes = artifacts.syntax_nodes_containing_span(span);
            assert!(
                nodes.iter().any(|node| node.kind == kind),
                "expected {kind:?} for `{text}`"
            );
            assert!(
                artifacts
                    .grammar_rule_applications_containing_span(span)
                    .iter()
                    .any(|application| application.rule_name == rule_name),
                "expected rule `{rule_name}` for `{text}`"
            );
        };

        assert_role(
            source.find("<circle>").expect("class name"),
            "<circle>",
            CstKind::TypeDefinitionName,
            "type-definition-name",
        );
        assert_role(
            source.find("<object>").expect("superclass"),
            "<object>",
            CstKind::SuperclassType,
            "superclass-type",
        );
        assert_role(
            source.find("slot radius").expect("slot") + "slot ".len(),
            "radius",
            CstKind::SlotName,
            "slot-name",
        );
        assert_role(
            source.find("function main").expect("function") + "function ".len(),
            "main",
            CstKind::ValueDefinitionName,
            "value-definition-name",
        );
        assert_role(
            source.find("(c ::").expect("parameter") + "(".len(),
            "c",
            CstKind::ParameterName,
            "parameter-name",
        );
        assert_role(
            source
                .find("(c :: class <circle>")
                .expect("class object annotation")
                + "(c :: class ".len(),
            "<circle>",
            CstKind::ClassObjectAnnotationType,
            "class-object-annotation-type",
        );
        assert_role(
            source.find("=> <integer>").expect("result annotation") + "=> ".len(),
            "<integer>",
            CstKind::AnnotationType,
            "annotation-type",
        );
        assert_role(
            source.find("let r").expect("let") + "let ".len(),
            "r",
            CstKind::LetName,
            "let-name",
        );
        assert_role(
            source.find("let t = <circle>").expect("type expression") + "let t = ".len(),
            "<circle>",
            CstKind::ExpressionTypeName,
            "expression-type-name",
        );
        assert_role(
            source.find("make(<circle>").expect("call name"),
            "make",
            CstKind::CallName,
            "call-name",
        );
        assert_role(
            source.rfind(".radius").expect("member") + ".".len(),
            "radius",
            CstKind::MemberName,
            "member-name",
        );
    }

    #[test]
    fn syntax_tooling_connects_spans_to_list_pattern_metadata() {
        let source = "define function main(x, y) => (<integer>, <integer>)\n  let values = #[1, 2];\n  add(x, y)\nend";
        let artifacts =
            CompilerSession::compile_source("list-patterns.loko", source, RuntimeMode::CompileOnly);

        let rule_data_for_span = |span: Span, rule_name: &str| {
            // This is the intended lookup path for tooling: source span ->
            // effective rule application -> selected direct rule -> rule
            // source data, where `list-patterns` lives.
            let application = artifacts
                .grammar_rule_applications_containing_span(span)
                .into_iter()
                .find(|application| application.rule_name == rule_name)
                .expect("span should be contained by requested rule");
            let effective = artifacts
                .grammar_parse
                .effective_rules
                .iter()
                .find(|rule| rule.id == application.rule)
                .expect("application should reference an effective grammar rule");
            let rule = artifacts
                .grammar_parse
                .rules
                .iter()
                .find(|rule| rule.id == effective.selected_rule)
                .expect("effective rule should reference a direct grammar rule");
            crate::source_data::dump(&rule.expression_data)
        };

        let parameter_start = source.find("x, y").expect("parameter list");
        let parameter_data = rule_data_for_span(
            Span::new(parameter_start, parameter_start + "x, y".len()),
            "parameter-list",
        );
        assert!(parameter_data.contains("element-text: \"parameter\""));
        assert!(parameter_data.contains("trailing-separator: #t"));

        let vector_start = source.find("#[1, 2]").expect("vector literal");
        let vector_data = rule_data_for_span(
            Span::new(vector_start, vector_start + "#[1, 2]".len()),
            "vector-expression",
        );
        assert!(vector_data.contains("element-text: \"expression\""));
        assert!(vector_data.contains("allows-empty: #t"));

        let argument_start = source.rfind("x, y").expect("argument list");
        let argument_data = rule_data_for_span(
            Span::new(argument_start, argument_start + "x, y".len()),
            "argument-list",
        );
        assert!(argument_data.contains("element-text: \"argument\""));
        assert!(argument_data.contains("minimum-count: 1"));
    }

    #[test]
    fn parse_view_reports_grammar_diagnostics_for_source_span() {
        let source = "define classs foo (<object>) end;";
        let artifacts =
            CompilerSession::compile_source("bad-keyword.loko", source, RuntimeMode::CompileOnly);
        let unexpected_span = Span::new(7, 13);

        let view = artifacts.parse_view_for_span(unexpected_span);
        assert_eq!(view.span, unexpected_span);
        assert_eq!(view.diagnostics.len(), 1);
        assert_eq!(
            view.diagnostics[0].code,
            DiagnosticCode::SyntaxExpectedToken
        );
        assert_eq!(view.diagnostics[0].span, unexpected_span);
    }

    #[test]
    fn parse_trace_renders_grammar_diagnostic_context() {
        let artifacts = CompilerSession::compile_source(
            "bad-syntax.loko",
            "define function main(",
            RuntimeMode::CompileOnly,
        );

        let trace = artifacts.dump_parse_trace();
        assert!(trace.contains("diagnostics:"));
        assert!(trace.contains("context=containing-rule-application"));
        assert!(trace.contains("application-span=0..21"));
        assert!(trace.contains("expected="));
    }

    #[test]
    fn session_collects_stages_artifacts_and_runtime() {
        let artifacts = CompilerSession::compile_source("area.loko", AREA, RuntimeMode::RunMain);

        assert_eq!(artifacts.diagnostics, Vec::new());
        assert!(artifacts.lowered.is_some());
        assert!(artifacts.tape.is_some());
        assert!(matches!(
            artifacts.runtime_result,
            Some(RuntimeResult::Returned(FunctionValue::Integer(100)))
        ));
        assert!(
            artifacts
                .artifact_reports()
                .iter()
                .any(|report| matches!(report.artifact, QueryArtifact::TapeProgram))
        );
        assert!(artifacts.dump_stage(DumpStage::Syntax).contains("Function"));
        assert!(artifacts.dump_stage(DumpStage::Ast).contains("function"));
        assert!(
            artifacts
                .dump_stage(DumpStage::Ast)
                .contains("radius :: <integer>")
        );
        assert!(artifacts.dump_stage(DumpStage::Sem).contains("slot s0"));
        assert!(
            artifacts
                .dump_stage(DumpStage::Sem)
                .contains(":: <integer>")
        );
        assert!(
            artifacts
                .dump_stage(DumpStage::Sem)
                .contains("generic-call")
        );
        assert!(
            artifacts
                .dump_stage(DumpStage::Sem)
                .contains("class-object")
        );
        assert!(
            artifacts
                .dump_stage(DumpStage::Sem)
                .contains("accessor-call")
        );
        let mop = artifacts.dump_stage(DumpStage::Mop);
        assert!(mop.contains("mop"));
        assert!(mop.contains("language environments:"));
        assert!(mop.contains("BaseLokoEnvironment"));
        assert!(mop.contains("grammars:"));
        assert!(mop.contains("BaseLokoGrammar"));
        assert!(mop.contains("grammar rules:"));
        assert!(mop.contains("function-definition"));
        assert!(mop.contains("grammar activations:"));
        assert!(mop.contains("classes:"));
        assert!(mop.contains("origin=native"));
        assert!(mop.contains("runtime=native"));
        assert!(mop.contains("initialization-plan:"));
        assert!(mop.contains("initialize-methods=deferred"));
        assert!(mop.contains("dispatch table:"));
        let eop = artifacts.dump_stage(DumpStage::Eop);
        assert!(eop.contains("eop"));
        assert!(eop.contains("object source:"));
        assert!(eop.contains("kind=source-file"));
        assert!(eop.contains("object language-environment:base-loko"));
        assert!(eop.contains("object grammar:base-loko"));
        assert!(eop.contains("object grammar-rule:base-loko/function-definition"));
        assert!(eop.contains("object effective-grammar-rule:base-loko/function-definition"));
        assert!(eop.contains("object grammar-activation:base-loko/source-file"));
        assert!(eop.contains("object class:c0"));
        assert!(eop.contains("object generic:g0"));
        assert!(eop.contains("object method:m0"));
        assert!(eop.contains("object function:f0"));
        assert!(eop.contains("object artifact:"));
        assert!(
            artifacts
                .dump_stage(DumpStage::Lowered)
                .contains("area#dispatch")
        );
        assert!(
            artifacts
                .dump_stage(DumpStage::Lowered)
                .contains("slot.read")
        );
        assert!(
            artifacts
                .dump_stage(DumpStage::Tape)
                .contains("program=\"interloko\"")
        );
        assert!(artifacts.dump_artifact_graph().contains("dependencies:"));
        assert!(
            artifacts
                .dump_artifact_graph()
                .contains("grammar-parse <- grammar-stack")
        );
        assert!(artifacts.dump_grammar_stack().contains("BaseLokoGrammar"));
        let parse_trace = artifacts.dump_parse_trace();
        assert!(parse_trace.contains("rule provenance:"));
        assert!(parse_trace.contains("rule applications:"));
        assert!(parse_trace.contains("rule-name=function-definition"));
        let shadow = artifacts.dump_parser_shadow_report();
        assert!(shadow.contains("parser shadow status=oracle-unavailable"));
        assert!(shadow.contains("implementation source-authored-base-loko-grammar"));
        assert!(shadow.contains("top-level-signature class-definition,generic-definition,method-definition,function-definition"));
        assert!(
            artifacts
                .explain_grammar_rule("grammar-rule:base-loko/function-definition")
                .is_some()
        );
        let expression = artifacts
            .explain_grammar_expression("grammar-rule:base-loko/function-definition")
            .expect("grammar rule expression");
        assert!(
            expression.contains("grammar expression grammar-rule:base-loko/function-definition")
        );
        assert!(expression.contains("rule function-definition"));
        assert!(expression.contains("data sequence("));
        assert!(
            artifacts
                .explain_grammar_environment("language-environment:base-loko")
                .expect("language environment grammar list")
                .contains("grammar:base-loko")
        );
        assert!(
            artifacts
                .explain_direct_grammar_rules("grammar:base-loko")
                .expect("direct rule list")
                .contains("grammar-rule:base-loko/function-definition")
        );
        assert!(
            artifacts
                .explain_effective_grammar_rules("grammar:base-loko")
                .expect("effective rule list")
                .contains("effective-grammar-rule:base-loko/function-definition")
        );
        assert!(
            artifacts
                .explain_grammar_activations("grammar:base-loko")
                .expect("activation list")
                .contains("grammar-activation:base-loko/source-file")
        );
    }

    #[test]
    fn session_reports_artifact_work_and_timing_fields() {
        let artifacts = CompilerSession::compile_source("area.loko", AREA, RuntimeMode::RunMain);
        let metrics = artifacts.artifact_metrics();

        let parse = metrics
            .iter()
            .find(|report| report.artifact == QueryArtifact::Parse)
            .expect("parse metric should be present");
        assert!(parse.metrics.work_units > 0);
        assert_eq!(parse.metrics.elapsed_ticks, 1);

        let runtime = metrics
            .iter()
            .find(|report| report.artifact == QueryArtifact::RuntimeResult)
            .expect("runtime metric should be present");
        assert_eq!(runtime.metrics.work_units, 1);

        let graph = artifacts.dump_artifact_graph();
        assert!(graph.contains("work="));
        assert!(graph.contains("ticks="));
        assert!(graph.contains("ns="));
    }

    #[test]
    fn session_captures_execution_trace_when_requested() {
        let artifacts = CompilerSession::compile_source(
            "area.loko",
            AREA,
            RuntimeMode::RunMainTraced {
                detail: ExecutionTraceDetail::Boundaries,
            },
        );
        assert!(matches!(
            artifacts.runtime_result,
            Some(RuntimeResult::Returned(FunctionValue::Integer(100)))
        ));
        let trace = artifacts
            .runtime_trace
            .as_ref()
            .expect("trace should be captured");
        assert_eq!(trace.detail(), ExecutionTraceDetail::Boundaries);
        assert_eq!(trace.recording().status(), Ok(()));
        assert!(trace.recording().instruction_status().is_none());
        trace
            .recording()
            .replay()
            .expect("a complete recording replays without the live host");

        let view = artifacts.execution_trace_view(trace);
        assert!(view.is_resolved());
        let entry = view.entry().expect("entry should be captured");
        assert_eq!(entry.name(), Some("main"));
        assert_eq!(
            entry.object_id(),
            Some(EnvironmentObjectId::new("function:f0"))
        );
        assert!(view.activation_count() >= 3);
        let slot_read = view
            .host_calls()
            .find(|call| call.symbol == Some("interloko.slot_get"))
            .expect("the method reads a slot through the host");
        assert!(matches!(
            slot_read.location.function.kind(),
            Some(lower::FunctionKind::Method(_))
        ));
        let source = slot_read.location.source.expect("slot read has a span");
        assert_eq!(&AREA[source.offset..source.offset + 1], "c");
        assert!(slot_read.stack.depth() >= Some(3));
        let outcome = view.outcome().expect("outcome should be captured");
        assert!(matches!(
            outcome.result,
            recording::Outcome::Returned([recording::RecordedValue::I64(100)])
        ));

        let dump = artifacts.dump_execution_trace();
        assert!(dump.starts_with("execution trace: boundaries\n  capture: complete\n"));
        assert!(dump.contains("invocation: main [function:f0] args=()"));
        assert!(dump.contains("host-call interloko.slot_get"));
        assert!(dump.contains(" from main@"));
        assert!(dump.contains("outcome: returned (100) at main@"));
        assert!(!dump.contains("instructions:"));

        let plain = CompilerSession::compile_source("area.loko", AREA, RuntimeMode::RunMain);
        assert!(plain.runtime_trace.is_none());
        assert!(
            plain
                .dump_execution_trace()
                .contains("traced execution was not requested")
        );
    }

    #[test]
    fn traced_nested_calls_keep_exact_callers_and_outcome_stacks() {
        for should_trap in [false, true] {
            let expression = if should_trap { "helper(x)" } else { "x + 1" };
            let source = format!(
                "define function helper(x :: <integer>) => <integer>\n  {expression}\nend;\ndefine function main()\n  helper(41)\nend;"
            );
            let artifacts = CompilerSession::compile_source(
                "nested.loko",
                &source,
                RuntimeMode::RunMainTraced {
                    detail: ExecutionTraceDetail::Instructions,
                },
            );
            assert!(
                artifacts.diagnostics.is_empty(),
                "{:?}",
                artifacts.diagnostics
            );
            if should_trap {
                assert!(matches!(
                    artifacts.runtime_result,
                    Some(RuntimeResult::Trap { .. })
                ));
            } else {
                assert!(matches!(
                    artifacts.runtime_result,
                    Some(RuntimeResult::Returned(FunctionValue::Integer(42)))
                ));
            }
            let trace = artifacts.runtime_trace.as_ref().unwrap();
            let view = artifacts.execution_trace_view(trace);
            let tape = &artifacts.tape.as_ref().unwrap().program;
            let is_call_site = |location: TracedLocation<'_>| {
                execution_tape::disasm::disassemble_function(
                    tape.program(),
                    location.function.id().func,
                )
                .unwrap()
                .instruction(location.pc)
                .unwrap()
                .opcode()
                .is_call_like()
            };
            assert_eq!(
                trace.recording().instruction_status(),
                Some(recording::InstructionStatus::Complete)
            );

            let mut nested = 0;
            for step in view.instructions() {
                let depth = step.stack.depth().expect("call frames are recorded");
                let callers: Vec<_> = step.stack.locations().skip(1).collect();
                assert_eq!(callers.len(), depth - 1);
                for caller in callers {
                    assert!(is_call_site(caller));
                    nested += 1;
                }
                if depth > 1 {
                    assert_eq!(step.location.function.name(), Some("helper"));
                }
            }
            assert!(nested > 0);

            let outcome = view.outcome().expect("outcome should be captured");
            let stack: Vec<_> = outcome.stack.locations().collect();
            if should_trap {
                let recording::Outcome::Trapped(trap) = outcome.result else {
                    panic!("expected a trap, found {:?}", outcome.result);
                };
                assert!(matches!(
                    trap.trap,
                    execution_tape::vm::Trap::CallDepthExceeded
                ));
                // The trap stack is pinned before unwinding: every activation
                // is still present, each suspended at its call instruction.
                assert_eq!(stack.len(), view.activation_count());
                assert!(stack.iter().skip(1).all(|caller| is_call_site(*caller)));
                assert_eq!(stack.last().unwrap().function.name(), Some("main"));
            } else {
                assert!(matches!(
                    outcome.result,
                    recording::Outcome::Returned([recording::RecordedValue::I64(42)])
                ));
                assert_eq!(view.activation_count(), 2);
                assert_eq!(stack.len(), 1);
            }
            let dump = artifacts.dump_execution_trace();
            assert!(dump.contains(" call -> helper@0 "));
            assert!(dump.contains(" instr helper@0 (2:") && dump.contains(") depth=2 "));
            assert!(dump.contains(if should_trap {
                "outcome: trapped: call depth limit exceeded at helper@"
            } else {
                "outcome: returned (42) at main@"
            }));
        }
    }

    #[test]
    fn session_can_capture_instruction_trace_for_named_runs() {
        let artifacts =
            CompilerSession::compile_source("area.loko", AREA, RuntimeMode::CompileOnly);
        let traced = artifacts
            .call_function("main", &[], Some(ExecutionTraceDetail::Instructions))
            .expect("main should run");
        assert_eq!(traced.value, FunctionValue::Integer(100));
        let trace = traced.trace.expect("trace should be captured");
        let view = artifacts.execution_trace_view(&trace);
        assert!(view.instructions().len() > 0);
        assert!(
            view.instructions()
                .any(|step| step.mnemonic() == "call" && step.next.is_some())
        );
        assert!(
            artifacts
                .execution_trace_view(&trace)
                .render()
                .contains(" instr main@0 ")
        );
    }

    #[test]
    fn grammar_object_ids_survive_harmless_source_edits() {
        let source_file = SourceFileId::from_path("examples/area.loko");
        let before = CompilerSession::compile_source_with_id(
            source_file,
            "examples/area.loko",
            AREA,
            RuntimeMode::CompileOnly,
        );
        let after = CompilerSession::compile_source_with_id(
            source_file,
            "examples/area.loko",
            &AREA.replace(
                "define generic area(x);",
                "define generic area(x); // harmless",
            ),
            RuntimeMode::CompileOnly,
        );

        let before_objects = before.environment_objects();
        let after_objects = after.environment_objects();
        for id in [
            "language-environment:base-loko",
            "grammar:base-loko",
            "grammar-rule:base-loko/function-definition",
            "grammar-activation:base-loko/source-file",
        ] {
            assert_eq!(
                eop_object(&before_objects, id).id,
                eop_object(&after_objects, id).id
            );
        }
    }

    const SHAPES_WITH_UNTAKEN_REFUTED_CALL: &str = "define class <shape> (<object>)\nend;\n\
        define class <circle> (<shape>)\nend;\n\
        define generic area(x);\n\
        define method area(c :: <circle>)\n  7\nend;\n\
        define function measure(s :: <shape>)\n  area(s)\nend;\n\
        define function main()\n  \
        if 1 < 2 then area(make(<circle>)) else area(make(<shape>)) end\nend;";

    #[test]
    fn soft_typing_warnings_ride_the_diagnostic_list_without_blocking() {
        let artifacts = CompilerSession::compile_source(
            "typing.loko",
            SHAPES_WITH_UNTAKEN_REFUTED_CALL,
            RuntimeMode::RunMain,
        );

        assert_eq!(artifacts.diagnostics.len(), 1);
        let warning = &artifacts.diagnostics[0];
        assert_eq!(
            warning.code,
            DiagnosticCode::TypingDispatchNoApplicableMethod
        );
        assert_eq!(warning.severity, crate::diagnostic::Severity::Warning);
        assert_eq!(artifacts.typing.diagnostics, artifacts.diagnostics);
        assert!(!artifacts.has_errors());
        assert!(artifacts.tape.is_some());
        assert!(matches!(
            artifacts.runtime_result,
            Some(RuntimeResult::Returned(FunctionValue::Integer(7)))
        ));
        assert!(
            artifacts
                .dump_stage(DumpStage::Typing)
                .contains("t1 dispatch refuted rule=dispatch-no-applicable-method")
        );
    }

    #[test]
    fn session_typing_config_sets_reporting_policy() {
        let mut session = CompilerSession::new();
        assert_eq!(session.typing_config(), typing::TypingConfig::soft());
        session.set_typing_config(typing::TypingConfig::strict());
        let id = session.add_source("typing.loko", SHAPES_WITH_UNTAKEN_REFUTED_CALL);
        let artifacts = session
            .compile(id, RuntimeMode::CompileOnly)
            .expect("source was just added");

        let codes = artifacts
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code, diagnostic.severity))
            .collect::<Vec<_>>();
        assert_eq!(
            codes,
            [
                (
                    DiagnosticCode::TypingDispatchRuntimeChecked,
                    crate::diagnostic::Severity::Warning
                ),
                (
                    DiagnosticCode::TypingDispatchNoApplicableMethod,
                    crate::diagnostic::Severity::Error
                ),
            ]
        );
        assert!(artifacts.has_errors());
        // Typing errors are advisory for the pipeline: lowering still ran.
        assert!(artifacts.tape.is_some());
    }

    #[test]
    fn typing_config_change_rederives_diagnostics_without_recompiling() {
        let mut session = IncrementalCompilerSession::new();
        let id = session.add_source("typing.loko", SHAPES_WITH_UNTAKEN_REFUTED_CALL);
        let soft = session
            .compile(id, RuntimeMode::CompileOnly)
            .expect("source was just added");
        session.set_typing_config(typing::TypingConfig::silent());
        let silent = session
            .compile(id, RuntimeMode::CompileOnly)
            .expect("source was just added");

        assert_eq!(soft.artifacts.diagnostics.len(), 1);
        assert!(silent.artifacts.diagnostics.is_empty());
        assert_eq!(
            silent.artifacts.typing.config,
            typing::TypingConfig::silent()
        );
        assert_eq!(
            silent.artifacts.typing.definitions,
            soft.artifacts.typing.definitions
        );

        let main = silent.artifacts.sem.functions[1].definition;
        for reused in [
            QueryArtifact::Bind,
            QueryArtifact::DefinitionTyping(main),
            QueryArtifact::LoweredModule,
            QueryArtifact::TapeProgram,
        ] {
            assert_eq!(
                cache_action(&silent, reused),
                Some(CacheAction::Reused),
                "{reused:?}"
            );
        }
        for recomputed in [
            QueryArtifact::TypingConfig,
            QueryArtifact::Typing,
            QueryArtifact::Diagnostics,
        ] {
            assert_eq!(
                cache_action(&silent, recomputed),
                Some(CacheAction::Recomputed),
                "{recomputed:?}"
            );
        }
        let config_report =
            cache_report(&silent, QueryArtifact::TypingConfig).expect("typing config report");
        assert!(config_report.invalidated);
        assert!(config_report.affected);
    }

    #[test]
    fn editing_one_definition_keeps_other_definition_typing_reused() {
        let mut session = IncrementalCompilerSession::new();
        let id = session.add_source("typing.loko", SHAPES_WITH_UNTAKEN_REFUTED_CALL);
        let _ = session
            .compile(id, RuntimeMode::CompileOnly)
            .expect("source was just added");
        let edited = SHAPES_WITH_UNTAKEN_REFUTED_CALL.replace(
            "else area(make(<shape>)) end",
            "else area(make(<circle>)) end",
        );
        let update = session
            .edit_and_compile(id, &edited, RuntimeMode::CompileOnly)
            .expect("source exists");

        let measure = update.artifacts.sem.functions[0].definition;
        let main = update.artifacts.sem.functions[1].definition;
        assert!(update.artifacts.diagnostics.is_empty());
        assert_eq!(
            cache_action(&update, QueryArtifact::DefinitionTyping(measure)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::DefinitionTyping(main)),
            Some(CacheAction::Recomputed)
        );

        // Shifting every span leaves per-definition typing reusable.
        let shifted = session
            .edit_and_compile(id, &format!("\n\n{edited}"), RuntimeMode::CompileOnly)
            .expect("source exists");
        assert_eq!(
            cache_action(&shifted, QueryArtifact::DefinitionTyping(main)),
            Some(CacheAction::Reused)
        );
    }

    #[test]
    fn typing_config_and_decisions_project_as_typed_eop_objects() {
        let artifacts = CompilerSession::compile_source(
            "typing.loko",
            SHAPES_WITH_UNTAKEN_REFUTED_CALL,
            RuntimeMode::CompileOnly,
        );
        let objects = artifacts.environment_objects();

        let config = eop_object(&objects, "typing-config:session");
        assert_eq!(config.kind, EnvironmentKind::TypingConfig);
        assert!(
            config
                .affordances
                .contains(EnvironmentAffordance::ShowTyping)
        );
        assert_eq!(
            config.typing,
            Some(TypingFacet::Config {
                config: typing::TypingConfig::soft(),
                status: typing::TypingStatus::Checked,
            })
        );

        let decisions = objects
            .iter()
            .filter(|object| object.kind == EnvironmentKind::TypingDecision)
            .collect::<Vec<_>>();
        assert_eq!(decisions.len(), artifacts.typing.decisions().count());
        let refuted = decisions
            .iter()
            .find_map(|object| match &object.typing {
                Some(TypingFacet::Decision { decision, severity })
                    if decision.verdict == typing::TypingVerdict::Refuted =>
                {
                    Some((*object, decision, *severity))
                }
                _ => None,
            })
            .expect("refuted decision object");
        let (object, decision, severity) = refuted;
        let main = artifacts.sem.functions[1].definition;
        assert_eq!(decision.owner, main);
        assert_eq!(severity, typing::TypingSeverity::Warning);
        assert_eq!(object.source.definition, Some(main));
        assert_eq!(object.source.span, Some(decision.span));
        assert!(
            object
                .artifacts
                .keys
                .contains(&QueryArtifact::DefinitionTyping(main).key(artifacts.source.id))
        );

        let explanation = artifacts
            .explain_typing(object.id.as_str())
            .expect("decision explanation");
        assert!(explanation.contains("rule: dispatch-no-applicable-method"));
        assert!(explanation.contains("actual: exactly <shape>"));
        assert_eq!(
            artifacts.explain_typing("typing-config:session"),
            Some(artifacts.dump_stage(DumpStage::Typing))
        );
        let main_object = objects
            .iter()
            .find(|object| {
                object.kind == EnvironmentKind::Function && object.source.definition == Some(main)
            })
            .expect("main function object");
        assert!(
            artifacts
                .explain_typing(main_object.id.as_str())
                .expect("definition typing")
                .contains("function main")
        );
        assert_eq!(artifacts.explain_typing("typing:missing"), None);
    }

    #[test]
    fn grammar_environment_selection_changes_stack_and_cache_reports() {
        let source_file = SourceFileId::from_path("examples/grammar.loko");
        let mut session = IncrementalCompilerSession::new();
        session.add_source_with_id(source_file, "examples/grammar.loko", SIMPLE_MAIN);
        let base = session
            .compile(source_file, RuntimeMode::CompileOnly)
            .expect("source was just added");
        let derived = session
            .set_grammar_environment_and_compile(
                source_file,
                GrammarEnvironmentSelection::derived_host(),
                RuntimeMode::CompileOnly,
            )
            .expect("source was just added");

        assert!(
            base.artifacts
                .dump_grammar_stack()
                .contains("BaseLokoGrammar")
        );
        assert!(
            derived
                .artifacts
                .dump_grammar_stack()
                .contains("DerivedHostGrammar")
        );
        assert!(
            derived
                .artifacts
                .dump_stage(DumpStage::Eop)
                .contains("effective-grammar-rule:derived-host/function-definition")
        );

        let grammar_stack = derived
            .reports
            .iter()
            .find(|report| report.artifact == QueryArtifact::GrammarStack)
            .expect("grammar stack report");
        let grammar_parse = derived
            .reports
            .iter()
            .find(|report| report.artifact == QueryArtifact::GrammarParse)
            .expect("grammar parse report");
        let source = derived
            .reports
            .iter()
            .find(|report| report.artifact == QueryArtifact::Source)
            .expect("source report");

        assert_eq!(source.action, CacheAction::Reused);
        assert_eq!(grammar_stack.action, CacheAction::Recomputed);
        assert!(grammar_stack.invalidated);
        assert!(grammar_stack.affected);
        assert_eq!(grammar_parse.action, CacheAction::Recomputed);
        assert!(grammar_parse.invalidated);
        assert!(grammar_parse.affected);
    }

    #[test]
    fn compiler_sessions_compile_through_their_owned_grammar_registry() {
        static EMPTY_ENVIRONMENTS: [&dyn grammar_frontend::GrammarEnvironmentProvider; 0] = [];
        static EMPTY_STAGED_ADAPTERS: [&dyn grammar_frontend::StagedGrammarAdapter; 0] = [];

        let registry = GrammarFrontendRegistry::new(&EMPTY_ENVIRONMENTS, &EMPTY_STAGED_ADAPTERS);
        let mut session = CompilerSession::with_grammar_registry(registry);
        let source = session.add_source("empty-registry.loko", SIMPLE_MAIN);
        let artifacts = session
            .compile(source, RuntimeMode::CompileOnly)
            .expect("source was just added");

        assert_eq!(session.grammar_registry(), registry);
        assert_eq!(artifacts.grammar_registry, registry);
        assert!(
            artifacts
                .grammar_parse
                .diagnostics
                .iter()
                .any(|diagnostic| {
                    diagnostic
                        .args
                        .iter()
                        .any(|arg| arg.name == "environment" && arg.value.render() == "base-loko")
                })
        );

        let mut incremental = IncrementalCompilerSession::with_grammar_registry(registry);
        let source = incremental.add_source("empty-registry.loko", SIMPLE_MAIN);
        let update = incremental
            .compile(source, RuntimeMode::CompileOnly)
            .expect("source was just added");

        assert_eq!(incremental.grammar_registry(), registry);
        assert_eq!(update.artifacts.grammar_registry, registry);
        assert!(
            update
                .artifacts
                .grammar_parse
                .diagnostics
                .iter()
                .any(|diagnostic| {
                    diagnostic
                        .args
                        .iter()
                        .any(|arg| arg.name == "environment" && arg.value.render() == "base-loko")
                })
        );
    }

    #[test]
    fn session_runs_main_through_typed_scalar_results() {
        let run = |source: &str| {
            CompilerSession::compile_source("main.loko", source, RuntimeMode::RunMain)
                .runtime_result
                .expect("main should run")
        };

        assert!(matches!(
            run("define function main() => <boolean>\n  1 < 2\nend;"),
            RuntimeResult::Returned(FunctionValue::Boolean(true))
        ));
        let string = run("define function main() => <string>\n  \"glass\\nhouse\"\nend;");
        assert!(
            matches!(&string, RuntimeResult::Returned(FunctionValue::String(text)) if text == "glass\nhouse")
        );
        assert_eq!(string.to_string(), "returned \"glass\\nhouse\"");
        let object = run(
            "define class <box> (<object>)\n  slot value :: <integer>;\nend;\n\
             define function main()\n  make(<box>, value: 1)\nend;",
        );
        assert!(matches!(object, RuntimeResult::UnsupportedResult { .. }));
        assert_eq!(
            object.code(),
            Some(DiagnosticCode::RuntimeUnsupportedResult)
        );

        let mut registry = sem::HostMetaRegistry::new();
        registry.add_function(sem::HostFunction::new(
            "host-answer",
            Vec::new(),
            sem::HostValueKind::Integer,
        ));
        let artifacts = CompilerSession::compile_source_with_host_registry(
            "host.loko",
            "define function main()\n  host-answer()\nend;",
            RuntimeMode::RunMain,
            &registry,
        );
        let result = artifacts.runtime_result.expect("main should run");
        assert!(
            matches!(&result, RuntimeResult::HostFunctionFailed { name, .. } if name == "host-answer")
        );
        assert_eq!(
            result.code(),
            Some(DiagnosticCode::RuntimeHostFunctionFailed)
        );
    }

    #[test]
    fn session_runs_named_zero_arg_functions() {
        let artifacts = CompilerSession::compile_source(
            "scratch.loko",
            "define function helper(x)\n  x + 1\nend;\n\
             define function __interloko_repl_expr_0()\n  helper(41)\nend;\n\
             define function object_result()\n  make(<box>, value: 1)\nend;\n\
             define class <box> (<object>)\n  slot value :: <integer>;\nend;",
            RuntimeMode::CompileOnly,
        );

        assert_eq!(artifacts.diagnostics, Vec::new());
        assert!(matches!(
            run_scalar(&artifacts, "__interloko_repl_expr_0"),
            Ok(FunctionValue::Integer(42))
        ));
        assert!(
            matches!(run_scalar(&artifacts, "missing"), Err(FunctionCallError::MissingFunction { name }) if name == "missing")
        );
        assert!(
            matches!(run_scalar(&artifacts, "helper"), Err(FunctionCallError::ArgumentCountMismatch { name, .. }) if name == "helper")
        );
        assert!(
            matches!(run_scalar(&artifacts, "object_result"), Err(FunctionCallError::UnsupportedResult { name, .. }) if name == "object_result")
        );
    }

    #[test]
    fn session_calls_string_dispatchers_without_exposing_runtime_handles() {
        let artifacts = CompilerSession::compile_source(
            "commands.loko",
            "define function dispatch(command :: <string>) => <string>\n  \
               if command = \"look\" then \"a glasshouse\" else \"unknown command\" end\n\
             end;",
            RuntimeMode::CompileOnly,
        );

        assert_eq!(artifacts.diagnostics, Vec::new());
        let look = artifacts
            .call_function(
                "dispatch",
                &[FunctionValue::String("look".into())],
                Some(ExecutionTraceDetail::Boundaries),
            )
            .expect("string command should execute");
        assert_eq!(look.value, FunctionValue::String("a glasshouse".into()));
        let trace = look.trace.expect("requested trace should be present");
        let view = artifacts.execution_trace_view(&trace);
        assert_eq!(
            view.entry().and_then(|entry| entry.name()),
            Some("dispatch")
        );
        assert_eq!(view.arguments().len(), 1);
        assert!(view.activation_count() >= 1);

        let unknown = artifacts
            .call_function("dispatch", &[FunctionValue::String("dance".into())], None)
            .expect("fallback should execute");
        assert_eq!(
            unknown.value,
            FunctionValue::String("unknown command".into())
        );
        assert!(unknown.trace.is_none());

        assert!(matches!(
            artifacts.call_function("dispatch", &[FunctionValue::Integer(1)], None),
            Err(FunctionCallError::ArgumentTypeMismatch {
                index: 0,
                actual: FunctionValueKind::Integer,
                ..
            })
        ));
    }

    #[test]
    fn float_to_integer_conversion_is_exact_or_traps() {
        let artifacts = CompilerSession::compile_source(
            "convert.loko",
            "define function to-integer(x :: <float>) => <integer>\n  as(<integer>, x)\nend;",
            RuntimeMode::CompileOnly,
        );
        assert_eq!(artifacts.diagnostics, Vec::new());
        let convert = |value: f64| {
            artifacts.call_function("to-integer", &[FunctionValue::Float(value)], None)
        };

        for (input, expected) in [(-0.0, 0), (3.0, 3), (-9.223372036854776e18, i64::MIN)] {
            assert!(
                matches!(convert(input), Ok(outcome) if outcome.value == FunctionValue::Integer(expected)),
                "{input} should convert to {expected}"
            );
        }
        for input in [2.5, -0.5, f64::NAN, f64::INFINITY, 9.223372036854776e18] {
            assert!(
                matches!(convert(input), Err(FunctionCallError::Trap { .. })),
                "{input} should trap"
            );
        }
    }

    #[test]
    fn integer_arithmetic_traps_on_overflow_at_the_fixnum_edges() {
        let artifacts = CompilerSession::compile_source(
            "overflow.loko",
            "define function add(a :: <integer>, b :: <integer>) => <integer>\n  a + b\nend;\n\
             define function sub(a :: <integer>, b :: <integer>) => <integer>\n  a - b\nend;\n\
             define function mul(a :: <integer>, b :: <integer>) => <integer>\n  a * b\nend;\n\
             define function neg(a :: <integer>) => <integer>\n  -a\nend;",
            RuntimeMode::CompileOnly,
        );
        assert_eq!(artifacts.diagnostics, Vec::new());
        let call = |name: &str, args: &[i64]| {
            let args: Vec<_> = args.iter().copied().map(FunctionValue::Integer).collect();
            artifacts.call_function(name, &args, None)
        };

        for (name, args, expected) in [
            ("add", &[i64::MAX - 1, 1][..], i64::MAX),
            ("add", &[i64::MIN, 0][..], i64::MIN),
            ("sub", &[i64::MIN + 1, 1][..], i64::MIN),
            ("sub", &[-1, i64::MAX][..], i64::MIN),
            ("mul", &[i64::MIN, 1][..], i64::MIN),
            ("mul", &[i64::MAX, -1][..], i64::MIN + 1),
            ("neg", &[i64::MAX][..], i64::MIN + 1),
            ("neg", &[0][..], 0),
        ] {
            assert!(
                matches!(call(name, args), Ok(outcome) if outcome.value == FunctionValue::Integer(expected)),
                "{name}{args:?} should return {expected}"
            );
        }
        for (name, args) in [
            ("add", &[i64::MAX, 1][..]),
            ("add", &[i64::MIN, -1][..]),
            ("sub", &[i64::MIN, 1][..]),
            ("sub", &[0, i64::MIN][..]),
            ("mul", &[i64::MIN, -1][..]),
            ("mul", &[1 << 32, 1 << 31][..]),
            ("neg", &[i64::MIN][..]),
        ] {
            let Err(FunctionCallError::Trap { trap, .. }) = call(name, args) else {
                panic!("{name}{args:?} should trap");
            };
            assert_eq!(loko_trap(&trap), Some(lower::LokoTrap::IntegerOverflow));
        }
    }

    #[test]
    fn integer_float_comparisons_are_exact() {
        extern crate std;
        use core::cmp::Ordering;

        let artifacts = CompilerSession::compile_source(
            "compare.loko",
            "define function lt(n :: <integer>, x :: <float>) => <boolean>\n  n < x\nend;\n\
             define function eq(n :: <integer>, x :: <float>) => <boolean>\n  n = x\nend;\n\
             define function ge(x :: <float>, n :: <integer>) => <boolean>\n  x >= n\nend;",
            RuntimeMode::CompileOnly,
        );
        assert_eq!(artifacts.diagnostics, Vec::new());
        // Independent reference: integral floats in range compare as i128;
        // fractional ones fall strictly between their floor and floor + 1.
        let reference = |n: i64, x: f64| -> Option<Ordering> {
            if x.is_nan() {
                return None;
            }
            if x >= 9_223_372_036_854_775_808.0 {
                return Some(Ordering::Less);
            }
            if x < -9_223_372_036_854_775_808.0 {
                return Some(Ordering::Greater);
            }
            let n = i128::from(n);
            let floor = std::primitive::f64::floor(x) as i128;
            if std::primitive::f64::fract(x) == 0.0 {
                Some(n.cmp(&floor))
            } else if n <= floor {
                Some(Ordering::Less)
            } else {
                Some(Ordering::Greater)
            }
        };
        let call = |name: &str, args: [FunctionValue; 2]| match artifacts
            .call_function(name, &args, None)
        {
            Ok(outcome) => outcome.value,
            Err(error) => panic!("{name}: {error}"),
        };
        let integers = [
            0,
            1,
            -1,
            3,
            (1 << 53) + 1,
            1 << 53,
            i64::MAX,
            i64::MAX - 1,
            i64::MIN,
            i64::MIN + 1,
        ];
        let floats = [
            0.0,
            -0.0,
            0.5,
            -0.5,
            2.5,
            3.0,
            9_007_199_254_740_992.0,
            9_007_199_254_740_994.0,
            9_223_372_036_854_775_808.0,
            -9_223_372_036_854_775_808.0,
            9_223_372_036_854_774_784.0,
            -9_223_372_036_854_777_856.0,
            1e300,
            -1e-300,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
        ];
        for n in integers {
            for x in floats {
                let expected = reference(n, x);
                let (int, float) = (FunctionValue::Integer(n), FunctionValue::Float(x));
                assert_eq!(
                    call("lt", [int.clone(), float.clone()]),
                    FunctionValue::Boolean(expected == Some(Ordering::Less)),
                    "{n} < {x}"
                );
                assert_eq!(
                    call("eq", [int.clone(), float.clone()]),
                    FunctionValue::Boolean(expected == Some(Ordering::Equal)),
                    "{n} = {x}"
                );
                assert_eq!(
                    call("ge", [float, int]),
                    FunctionValue::Boolean(expected.is_some_and(Ordering::is_le)),
                    "{x} >= {n}"
                );
            }
        }
    }

    #[test]
    fn turn_stats_separate_allocation_from_survival() {
        let artifacts = CompilerSession::compile_source(
            "survival.loko",
            "define function main() => <string>\n  \
             concatenate(concatenate(\"a\", \"b\"), integer-to-string(42))\nend;",
            RuntimeMode::CompileOnly,
        );
        assert_eq!(artifacts.diagnostics, Vec::new());
        let outcome = artifacts
            .call_function("main", &[], None)
            .expect("main should run");
        assert_eq!(outcome.value, FunctionValue::String("ab42".into()));
        let turn = outcome.turn;
        // Two literals, one intermediate, the integer text and the result.
        assert_eq!(turn.allocated.objects, 5);
        // Only the result string is reachable when the turn ends.
        assert_eq!(turn.reachable.objects, 1);
        assert!(turn.reachable.bytes < turn.allocated.bytes);
        assert!(turn.survival_ratio().is_some_and(|ratio| ratio < 0.5));
    }

    const TURN_PROGRAM: &str = "define class <cell> (<object>)\n  \
        slot value :: <integer>, init-value: 0;\nend;\n\
        define function churn(n :: <integer>, acc :: <integer>) => <integer>\n  \
        if n = 0 then acc else \
        churn(n - 1, acc + make(<cell>).value + score(concatenate(\"ab\", \"cd\"))) end\nend;\n\
        define function score(text :: <string>) => <integer>\n  \
        if text = \"abcd\" then 4 else 0 end\nend;\n\
        define function tag(n :: <integer>) => <boolean>\n  \
        if n = 0 then #ready = #ready else #other = #other end\nend;\n\
        define function tag-then-trap(n :: <integer>) => <integer>\n  \
        if #fresh = #fresh then n + 9223372036854775807 else 0 end\nend;";

    #[test]
    fn runtime_turns_keep_the_heap_flat() {
        let artifacts =
            CompilerSession::compile_source("turns.loko", TURN_PROGRAM, RuntimeMode::CompileOnly);
        assert_eq!(artifacts.diagnostics, Vec::new());
        let mut runtime = artifacts.runtime().expect("tape is available");
        let empty = runtime.heap_stats();
        for _ in 0..20 {
            let outcome = runtime
                .call_function(
                    &artifacts,
                    "churn",
                    &[FunctionValue::Integer(50), FunctionValue::Integer(0)],
                    None,
                )
                .expect("churn should run");
            assert_eq!(outcome.value, FunctionValue::Integer(200));
            // Each turn allocates cells and strings, and none survive.
            assert!(outcome.turn.allocated.objects >= 150);
            assert_eq!(outcome.turn.reachable, HeapStats::default());
            assert_eq!(runtime.heap_stats(), empty);
        }

        // Interned symbols are image state and persist across turns.
        runtime
            .call_function(&artifacts, "tag", &[FunctionValue::Integer(0)], None)
            .expect("tag should run");
        let tagged = runtime.heap_stats();
        assert_eq!(tagged.objects, empty.objects + 1);
        runtime
            .call_function(&artifacts, "tag", &[FunctionValue::Integer(0)], None)
            .expect("tag should run");
        assert_eq!(runtime.heap_stats(), tagged);
    }

    #[test]
    fn runtime_turns_replay_deterministically() {
        let artifacts =
            CompilerSession::compile_source("turns.loko", TURN_PROGRAM, RuntimeMode::CompileOnly);
        assert_eq!(artifacts.diagnostics, Vec::new());
        let replay = || {
            let mut runtime = artifacts.runtime().expect("tape is available");
            let mut log = Vec::new();
            for (name, args) in [
                (
                    "churn",
                    &[FunctionValue::Integer(30), FunctionValue::Integer(0)][..],
                ),
                ("tag", &[FunctionValue::Integer(0)][..]),
                ("tag-then-trap", &[FunctionValue::Integer(1)][..]),
                ("tag", &[FunctionValue::Integer(1)][..]),
            ] {
                let outcome = runtime.call_function(&artifacts, name, args, None);
                log.push((
                    outcome.as_ref().ok().map(|outcome| outcome.turn),
                    runtime.heap_stats(),
                ));
            }
            log
        };
        assert_eq!(replay(), replay());
    }

    #[test]
    fn trapped_turns_leave_the_heap_unchanged() {
        let artifacts =
            CompilerSession::compile_source("turns.loko", TURN_PROGRAM, RuntimeMode::CompileOnly);
        assert_eq!(artifacts.diagnostics, Vec::new());
        let mut runtime = artifacts.runtime().expect("tape is available");
        runtime
            .call_function(&artifacts, "tag", &[FunctionValue::Integer(1)], None)
            .expect("tag should run");
        let before = runtime.heap_stats();
        let trapped = runtime.call_function(
            &artifacts,
            "tag-then-trap",
            &[FunctionValue::Integer(1)],
            None,
        );
        assert!(matches!(trapped, Err(FunctionCallError::Trap { .. })));
        // The trapped turn interned `#fresh`; the abort drops it again.
        assert_eq!(runtime.heap_stats(), before);

        // A runtime refuses artifacts with a different layout.
        let other = CompilerSession::compile_source(
            "other.loko",
            "define class <other> (<object>)\nend;\ndefine function main() => <integer>\n  1\nend;",
            RuntimeMode::CompileOnly,
        );
        assert!(matches!(
            runtime.call_function(&other, "main", &[], None),
            Err(FunctionCallError::RuntimeLayoutChanged)
        ));
    }

    #[test]
    fn boxed_scalar_dispatch_keeps_the_heap_flat() {
        let artifacts = CompilerSession::compile_source(
            "boxing.loko",
            "define generic weigh(x);\n\
             define method weigh(x :: <integer>) => <integer>\n  x\nend;\n\
             define method weigh(x :: <double-float>) => <integer>\n  1\nend;\n\
             define function spin(n :: <integer>, acc :: <integer>) => <integer>\n  \
             if n = 0 then acc else spin(n - 1, acc + weigh(n) + weigh(0.5) + weigh(n * 2) + weigh(-1.5)) end\nend;\n\
             define function wide(n :: <integer>) => <integer>\n  weigh(n)\nend;",
            RuntimeMode::CompileOnly,
        );
        assert_eq!(artifacts.diagnostics, Vec::new());
        let spin = |n: i64| {
            artifacts
                .call_function(
                    "spin",
                    &[FunctionValue::Integer(n), FunctionValue::Integer(0)],
                    None,
                )
                .expect("spin should run")
        };

        let few = spin(10);
        let many = spin(200);
        assert_eq!(few.value, FunctionValue::Integer(3 * 55 + 2 * 10));
        assert_eq!(many.value, FunctionValue::Integer(3 * 20_100 + 2 * 200));
        // 800 boxed arguments, and not one heap box.
        assert_eq!(many.turn.allocated, few.turn.allocated);
        assert_eq!(many.turn.allocated.boxed_values, 0);

        // Integers outside the immediate range still box exactly, on the heap.
        let wide = artifacts
            .call_function("wide", &[FunctionValue::Integer(i64::MAX)], None)
            .expect("wide should run");
        assert_eq!(wide.value, FunctionValue::Integer(i64::MAX));
        assert_eq!(wide.turn.allocated.boxed_values, 1);
    }

    #[test]
    fn boxed_objects_and_booleans_keep_the_heap_flat() {
        let artifacts = CompilerSession::compile_source(
            "boxing.loko",
            "define class <point> (<object>)\n  slot x :: <integer>, init-value: 1;\nend;\n\
             define generic keep(x);\n\
             define method keep(x :: <object>) => <object>\n  x\nend;\n\
             define function score(p :: <point>, flag :: <boolean>) => <integer>\n  \
             if instance?(keep(p), <point>) then\n    \
             if instance?(keep(flag), <point>) then 0 else 1 end\n  \
             else\n    0\n  end\nend;\n\
             define function spin(n :: <integer>, p :: <point>, acc :: <integer>) => <integer>\n  \
             if n = 0 then acc else spin(n - 1, p, acc + score(p, n < 5) + score(p, #t)) end\nend;\n\
             define function main(n :: <integer>) => <integer>\n  spin(n, make(<point>), 0)\nend;\n\
             define function is-class(x :: <object>) => <integer>\n  \
             if instance?(x, <string>) then 0 else 1 end\nend;\n\
             define function mop(n :: <integer>, acc :: <integer>) => <integer>\n  \
             if n = 0 then acc else \
             mop(n - 1, acc + is-class(class-direct-superclass(object-class(2.5)))) end\nend;",
            RuntimeMode::CompileOnly,
        );
        assert_eq!(artifacts.diagnostics, Vec::new());
        let call = |name: &str, args: &[i64]| {
            let args: Vec<_> = args.iter().copied().map(FunctionValue::Integer).collect();
            artifacts
                .call_function(name, &args, None)
                .expect("call should run")
        };

        let few = call("main", &[10]);
        let many = call("main", &[200]);
        assert_eq!(few.value, FunctionValue::Integer(20));
        assert_eq!(many.value, FunctionValue::Integer(400));
        // 800 boxed objects and booleans: no boxes, and only the one point.
        assert_eq!(many.turn.allocated, few.turn.allocated);
        assert_eq!(many.turn.allocated.boxed_values, 0);

        // MOP queries still allocate metaobjects, but no box wrappers.
        let mop = call("mop", &[50, 0]);
        assert_eq!(mop.value, FunctionValue::Integer(50));
        assert_eq!(mop.turn.allocated.boxed_values, 0);
    }

    #[test]
    fn integer_overflow_is_a_distinct_runtime_outcome() {
        let artifacts = CompilerSession::compile_source(
            "overflow.loko",
            "define function main()\n  9223372036854775807 + 1\nend;",
            RuntimeMode::RunMain,
        );
        let result = artifacts.runtime_result.expect("main should run");
        assert_eq!(result.code(), Some(DiagnosticCode::RuntimeIntegerOverflow));
        let rendered = result.to_string();
        assert!(
            rendered.starts_with("runtime trap: integer overflow (trap at "),
            "{rendered}"
        );
    }

    #[test]
    fn unboxing_checks_the_boxed_class() {
        let artifacts = CompilerSession::compile_source(
            "unbox.loko",
            "define function as-float(x :: <object>) => <float>\n  as(<float>, x)\nend;\n\
             define function from-integer() => <float>\n  as-float(as(<object>, 7))\nend;\n\
             define function from-float() => <float>\n  as-float(as(<object>, 7.5))\nend;",
            RuntimeMode::CompileOnly,
        );
        assert_eq!(artifacts.diagnostics, Vec::new());
        assert_eq!(
            run_scalar(&artifacts, "from-float").ok(),
            Some(FunctionValue::Float(7.5))
        );
        assert!(matches!(
            run_scalar(&artifacts, "from-integer"),
            Err(FunctionCallError::Trap { .. })
        ));
    }

    struct CelsiusHandler;

    impl HostFunctionHandler for CelsiusHandler {
        fn call(
            &mut self,
            _function: &sem::HostFunction,
            arguments: &[FunctionValue],
        ) -> Result<FunctionValue, HostFunctionFailure> {
            let [FunctionValue::Float(fahrenheit)] = arguments else {
                return Err(HostFunctionFailure::new("expected one float"));
            };
            Ok(FunctionValue::Float((fahrenheit - 32.0) * 5.0 / 9.0))
        }
    }

    #[test]
    fn session_float_values_cross_call_and_host_boundaries() {
        let mut registry = sem::HostMetaRegistry::new();
        registry.add_function(sem::HostFunction::new(
            "to-celsius",
            alloc::vec![sem::HostValueKind::Float],
            sem::HostValueKind::Float,
        ));
        let artifacts = CompilerSession::compile_source_with_host_registry(
            "floats.loko",
            "define function identity(x :: <float>) => <float>\n  x\nend;\n\
             define function body-temperature() => <float>\n  to-celsius(98.6)\nend;",
            RuntimeMode::CompileOnly,
            &registry,
        );
        assert_eq!(artifacts.diagnostics, Vec::new());

        let negative_zero = artifacts
            .call_function("identity", &[FunctionValue::Float(-0.0)], None)
            .expect("identity should run")
            .value;
        assert!(
            matches!(negative_zero, FunctionValue::Float(value) if value.to_bits() == (-0.0_f64).to_bits())
        );
        assert_eq!(negative_zero.to_string(), "-0.0");
        assert_eq!(negative_zero.kind(), FunctionValueKind::Float);
        let nan = artifacts
            .call_function("identity", &[FunctionValue::Float(f64::NAN)], None)
            .expect("identity should run")
            .value;
        assert_ne!(nan, nan, "float results use IEEE equality");
        assert_eq!(nan.to_string(), "nan");
        assert!(matches!(
            artifacts.call_function("identity", &[FunctionValue::Integer(1)], None),
            Err(FunctionCallError::ArgumentTypeMismatch {
                actual: FunctionValueKind::Integer,
                ..
            })
        ));

        let celsius = artifacts
            .call_function_with_host("body-temperature", &[], None, &mut CelsiusHandler)
            .expect("host call should run")
            .value;
        assert_eq!(celsius, FunctionValue::Float((98.6 - 32.0) * 5.0 / 9.0));
    }

    #[derive(Default)]
    struct RecordingHostFunctionHandler {
        descriptions: Vec<String>,
    }

    impl HostFunctionHandler for RecordingHostFunctionHandler {
        fn call(
            &mut self,
            function: &sem::HostFunction,
            arguments: &[FunctionValue],
        ) -> Result<FunctionValue, HostFunctionFailure> {
            if function.name != "propose-description" {
                return Err(HostFunctionFailure::new("unexpected host function"));
            }
            let [FunctionValue::String(description)] = arguments else {
                return Err(HostFunctionFailure::new("expected one string"));
            };
            self.descriptions.push(description.clone());
            Ok(FunctionValue::Boolean(true))
        }
    }

    #[test]
    fn session_executes_declared_scalar_host_functions_through_a_handler() {
        let mut registry = sem::HostMetaRegistry::new();
        registry.add_function(sem::HostFunction::new(
            "propose-description",
            alloc::vec![sem::HostValueKind::String],
            sem::HostValueKind::Boolean,
        ));
        let artifacts = CompilerSession::compile_source_with_host_registry(
            "commands.loko",
            "define function dispatch(command :: <string>) => <string>\n  \
               if command = \"describe\" then\n    \
                 if propose-description(\"a luminous glasshouse\") then \
                   \"accepted\" else \"rejected\" end\n  \
               else \"unknown\" end\n\
             end;",
            RuntimeMode::CompileOnly,
            &registry,
        );

        assert_eq!(artifacts.diagnostics, Vec::new());
        assert_eq!(artifacts.sem.host_functions.len(), 1);
        let host_object = eop_object(
            &artifacts.environment_objects(),
            "host-function:propose-description",
        )
        .clone();
        assert_eq!(host_object.kind, EnvironmentKind::HostFunction);
        assert_eq!(host_object.origin, EnvironmentOrigin::HostBacked);
        assert_eq!(host_object.storage, EnvironmentStorage::HostRegistry);
        assert!(
            artifacts
                .dump_stage(DumpStage::Sem)
                .contains("host-call h0")
        );
        assert!(
            artifacts
                .dump_stage(DumpStage::Lowered)
                .contains("host.call h0 propose-description")
        );
        let tape = artifacts.dump_stage(DumpStage::Tape);
        assert!(tape.contains("propose-description"), "{tape}");

        let mut handler = RecordingHostFunctionHandler::default();
        let outcome = artifacts
            .call_function_with_host(
                "dispatch",
                &[FunctionValue::String("describe".into())],
                Some(ExecutionTraceDetail::Boundaries),
                &mut handler,
            )
            .expect("declared host function should execute");
        assert_eq!(outcome.value, FunctionValue::String("accepted".into()));
        assert_eq!(handler.descriptions, ["a luminous glasshouse"]);
        let trace = outcome.trace.expect("trace was requested");
        let proposal = artifacts
            .execution_trace_view(&trace)
            .host_calls()
            .find(|call| call.symbol == Some("propose-description"))
            .expect("the declared host call is recorded");
        assert!(matches!(
            proposal.outcome,
            recording::HostOutcome::Returned {
                values: [recording::RecordedValue::Bool(true)],
                ..
            }
        ));
        assert_eq!(proposal.location.function.name(), Some("dispatch"));

        let Err(FunctionCallError::HostFunctionFailed {
            name,
            failure,
            trace: Some(trace),
        }) = artifacts.call_function(
            "dispatch",
            &[FunctionValue::String("describe".into())],
            Some(ExecutionTraceDetail::Boundaries),
        )
        else {
            panic!("an unhandled host function should fail with a trace");
        };
        assert_eq!(name, "propose-description");
        assert_eq!(failure.message(), "no host-function handler was provided");
        let view = artifacts.execution_trace_view(&trace);
        let failed = view.host_calls().last().expect("failed call is recorded");
        assert_eq!(failed.symbol, Some("propose-description"));
        assert!(matches!(failed.outcome, recording::HostOutcome::Failed(_)));
        assert!(matches!(
            view.outcome().map(|outcome| outcome.result),
            Some(recording::Outcome::Trapped(_))
        ));

        let conflict = CompilerSession::compile_source_with_host_registry(
            "conflict.loko",
            "define function propose-description(value :: <string>) => <boolean>\n  #t\nend;",
            RuntimeMode::CompileOnly,
            &registry,
        );
        assert!(
            conflict
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == DiagnosticCode::DuplicateDefinition)
        );
    }

    #[test]
    fn dispatch_sites_explain_exact_class_selection() {
        let artifacts =
            CompilerSession::compile_source("area.loko", AREA, RuntimeMode::CompileOnly);
        let sites = artifacts.dispatch_sites();

        assert_eq!(sites.len(), 1);
        let site = &sites[0];
        assert_eq!(site.id, "dispatch:d0");
        assert_eq!(site.owner.name, "main");
        assert_eq!(site.owner.kind, DispatchSiteOwnerKind::Function);
        assert_eq!(
            site.exact_static_class,
            Some(class_id(&artifacts, "<circle>"))
        );
        assert!(!site.runtime_needed);
        assert_eq!(
            site.selected_method,
            Some(method_for_class(&artifacts, "<circle>"))
        );
        assert_eq!(site.applicable.len(), 1);
        assert!(site.rejected.is_empty());
        assert!(
            site.related_artifacts
                .iter()
                .any(|report| matches!(report.artifact, QueryArtifact::MethodLookup(_)))
        );
        assert!(
            site.related_artifacts
                .iter()
                .any(|report| matches!(report.artifact, QueryArtifact::GenericDispatcher(_)))
        );
        assert!(
            site.related_artifacts
                .iter()
                .any(|report| report.artifact == QueryArtifact::LoweredModule)
        );
        assert!(
            site.related_artifacts
                .iter()
                .any(|report| report.artifact == QueryArtifact::TapeProgram)
        );

        let list = artifacts.dump_dispatch_sites();
        assert!(list.contains("dispatch:d0"));
        assert!(list.contains("static-args=(object(<circle>))"));
        let explanation = artifacts
            .explain_dispatch_site("dispatch:d0")
            .expect("dispatch site should exist");
        assert!(explanation.contains("owner: function main"));
        assert!(explanation.contains("generic: g0:area"));
        assert!(explanation.contains("static-kinds: (object(<circle>))"));
        assert!(explanation.contains("runtime-needed: false"));
        assert!(explanation.contains("selected"));
        assert!(explanation.contains("dispatcher: area#dispatch"));
        assert!(explanation.contains("related artifacts:"));
    }

    #[test]
    fn dispatch_sites_choose_child_before_parent() {
        let source = "define class <shape> (<object>)\n  slot size :: <integer>, init-value: 1;\nend;\n\
                      define class <circle> (<shape>)\n  slot radius :: <integer>, init-value: 2;\nend;\n\
                      define generic score(x :: <shape>) => <integer>;\n\
                      define method score(s :: <shape>) => <integer>\n  s.size\nend;\n\
                      define method score(c :: <circle>) => <integer>\n  c.radius\nend;\n\
                      define function main()\n  score(make(<circle>))\nend;";
        let artifacts =
            CompilerSession::compile_source("score.loko", source, RuntimeMode::CompileOnly);
        let site = artifacts.dispatch_site("dispatch:d0").expect("site exists");

        assert_eq!(
            site.exact_static_class,
            Some(class_id(&artifacts, "<circle>"))
        );
        assert_eq!(
            site.selected_method,
            Some(method_for_class(&artifacts, "<circle>"))
        );
        assert_eq!(site.applicable.len(), 2);
        assert!(site.applicable[0].selected);
        assert_eq!(
            site.applicable[0].specializer,
            class_id(&artifacts, "<circle>")
        );
        assert_eq!(
            site.applicable[1].specializer,
            class_id(&artifacts, "<shape>")
        );
        assert!(!site.applicable[1].runtime_only);
        let explanation = artifacts.explain_dispatch_site("dispatch:d0").unwrap();
        assert!(explanation.contains("m1:score(c2:<circle>)"));
        assert!(explanation.contains("m0:score(c1:<shape>)"));
        assert!(explanation.contains("shadowed"));
    }

    #[test]
    fn dispatch_sites_mark_parent_typed_calls_runtime_needed() {
        let source = "define class <shape> (<object>)\n  slot size :: <integer>, init-value: 1;\nend;\n\
                      define class <circle> (<shape>)\n  slot radius :: <integer>, init-value: 2;\nend;\n\
                      define generic score(x :: <shape>) => <integer>;\n\
                      define method score(s :: <shape>) => <integer>\n  s.size\nend;\n\
                      define method score(c :: <circle>) => <integer>\n  c.radius\nend;\n\
                      define function call-score(s :: <shape>) => <integer>\n  score(s)\nend;";
        let artifacts =
            CompilerSession::compile_source("score.loko", source, RuntimeMode::CompileOnly);
        let site = artifacts.dispatch_site("dispatch:d0").expect("site exists");

        assert_eq!(
            site.static_argument_kind,
            Some(sem::ValueKind::Object(class_id(&artifacts, "<shape>")))
        );
        assert_eq!(site.exact_static_class, None);
        assert!(site.runtime_needed);
        assert_eq!(site.selected_method, None);
        assert_eq!(site.applicable.len(), 2);
        assert!(
            site.applicable
                .iter()
                .all(|candidate| candidate.runtime_only)
        );
        let explanation = artifacts.explain_dispatch_site("dispatch:d0").unwrap();
        assert!(explanation.contains("runtime-needed: true"));
        assert!(explanation.contains("selected: runtime-needed"));
    }

    #[test]
    fn dispatch_sites_report_rejected_sibling_methods() {
        let source = "define class <shape> (<object>)\n  slot size :: <integer>, init-value: 1;\nend;\n\
                      define class <circle> (<shape>)\n  slot radius :: <integer>, init-value: 2;\nend;\n\
                      define class <square> (<shape>)\n  slot side :: <integer>, init-value: 3;\nend;\n\
                      define generic area(x :: <shape>) => <integer>;\n\
                      define method area(c :: <circle>) => <integer>\n  c.radius\nend;\n\
                      define method area(s :: <square>) => <integer>\n  s.side\nend;\n\
                      define function main()\n  area(make(<circle>))\nend;";
        let artifacts =
            CompilerSession::compile_source("area.loko", source, RuntimeMode::CompileOnly);
        let site = artifacts.dispatch_site("dispatch:d0").expect("site exists");

        assert_eq!(
            site.selected_method,
            Some(method_for_class(&artifacts, "<circle>"))
        );
        assert!(site.rejected.iter().any(|rejected| {
            rejected.specializer == class_id(&artifacts, "<square>")
                && rejected.reason.contains("disjoint")
        }));
        let explanation = artifacts.explain_dispatch_site("dispatch:d0").unwrap();
        assert!(explanation.contains("rejected methods:"));
        assert!(explanation.contains("c3:<square>"));
        assert!(explanation.contains("disjoint"));
    }

    #[test]
    fn dispatch_site_ids_survive_harmless_trivia_edits() {
        let before = CompilerSession::compile_source("area.loko", AREA, RuntimeMode::CompileOnly);
        let after = CompilerSession::compile_source(
            "area.loko",
            "\n// same dispatch with harmless trivia\n\
             define class <circle>   (   <object>   )\n  slot radius :: <integer>;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius   *   c.radius\nend;\n\
             define function main()\n  area( make( <circle>, radius: 10 ) )\nend;",
            RuntimeMode::CompileOnly,
        );
        let before_sites = before.dispatch_sites();
        let after_sites = after.dispatch_sites();

        assert_eq!(before_sites.len(), 1);
        assert_eq!(after_sites.len(), 1);
        assert_eq!(before_sites[0].id, after_sites[0].id);
        assert_eq!(before_sites[0].key, after_sites[0].key);
    }

    #[test]
    fn dispatch_explanations_change_with_method_and_class_facts() {
        let before = "define class <shape> (<object>)\n  slot size :: <integer>, init-value: 1;\nend;\n\
                      define class <circle> (<shape>)\n  slot radius :: <integer>, init-value: 2;\nend;\n\
                      define generic score(x :: <object>) => <integer>;\n\
                      define method score(s :: <shape>) => <integer>\n  s.size\nend;\n\
                      define function main()\n  score(make(<circle>))\nend;";
        let with_child = "define class <shape> (<object>)\n  slot size :: <integer>, init-value: 1;\nend;\n\
                          define class <circle> (<shape>)\n  slot radius :: <integer>, init-value: 2;\nend;\n\
                          define generic score(x :: <object>) => <integer>;\n\
                          define method score(s :: <shape>) => <integer>\n  s.size\nend;\n\
                          define method score(c :: <circle>) => <integer>\n  c.radius\nend;\n\
                          define function main()\n  score(make(<circle>))\nend;";
        let class_changed = "define class <shape> (<object>)\n  slot size :: <integer>, init-value: 1;\nend;\n\
                             define class <circle> (<object>)\n  slot radius :: <integer>, init-value: 2;\nend;\n\
                             define generic score(x :: <object>) => <integer>;\n\
                             define method score(s :: <shape>) => <integer>\n  s.size\nend;\n\
                             define method score(c :: <circle>) => <integer>\n  c.radius\nend;\n\
                             define function main()\n  score(make(<circle>))\nend;";

        let before =
            CompilerSession::compile_source("score.loko", before, RuntimeMode::CompileOnly);
        let with_child =
            CompilerSession::compile_source("score.loko", with_child, RuntimeMode::CompileOnly);
        let class_changed =
            CompilerSession::compile_source("score.loko", class_changed, RuntimeMode::CompileOnly);

        let before_site = before.dispatch_site("dispatch:d0").expect("site exists");
        let with_child_site = with_child
            .dispatch_site("dispatch:d0")
            .expect("site exists");
        let class_changed_site = class_changed
            .dispatch_site("dispatch:d0")
            .expect("site exists");

        assert_eq!(
            before_site.selected_method,
            Some(method_for_class(&before, "<shape>"))
        );
        assert_eq!(
            with_child_site.selected_method,
            Some(method_for_class(&with_child, "<circle>"))
        );
        assert!(class_changed_site.rejected.iter().any(|rejected| {
            rejected.specializer == class_id(&class_changed, "<shape>")
                && rejected.reason.contains("disjoint")
        }));

        let diff = CompilerSession::diff(&before, &with_child);
        let generic = with_child.sem.generics[0].definition;
        assert!(diff.changed.iter().any(|change| {
            change.definition.kind == DefinitionKind::Method
                && change.change == DefinitionChangeKind::Added
        }));
        assert!(
            diff.affected
                .iter()
                .any(|report| report.artifact == QueryArtifact::GenericDispatcher(generic))
        );
    }

    #[test]
    fn session_projects_structured_eop_records() {
        let artifacts = CompilerSession::compile_source("area.loko", AREA, RuntimeMode::RunMain);
        let objects = artifacts.environment_objects();

        let source = eop_object(&objects, &format!("source:{}", artifacts.source.id));
        assert_eq!(source.kind, EnvironmentKind::SourceFile);
        assert_eq!(source.origin, EnvironmentOrigin::SourceAuthored);
        assert_eq!(source.storage, EnvironmentStorage::SourceDefined);
        assert!(source.affordances.contains(EnvironmentAffordance::DumpEop));
        assert_eq!(source.source.source_file, Some(artifacts.source.id));
        assert_eq!(source.source.span, Some(Span::new(0, AREA.len())));
        assert_eq!(
            source.artifacts.keys,
            Vec::from([QueryArtifact::Source.key(artifacts.source.id)])
        );

        let grammar_registry = eop_object(&objects, "grammar-registry:session");
        assert_eq!(grammar_registry.kind, EnvironmentKind::GrammarRegistry);
        assert_eq!(
            grammar_registry.storage,
            EnvironmentStorage::FrontendRegistry
        );
        assert!(
            grammar_registry
                .affordances
                .contains(EnvironmentAffordance::ShowGrammarRegistry)
        );
        assert!(
            grammar_registry
                .inspection
                .detail
                .contains("environments=[base-loko,derived-host]")
        );
        assert!(
            grammar_registry
                .artifacts
                .keys
                .contains(&QueryArtifact::EnvironmentResolution.key(artifacts.source.id))
        );

        let registered_environment =
            eop_object(&objects, "registered-grammar-environment:base-loko");
        assert_eq!(
            registered_environment.kind,
            EnvironmentKind::RegisteredGrammarEnvironment
        );
        assert_eq!(
            registered_environment.storage,
            EnvironmentStorage::FrontendRegistry
        );
        assert!(
            registered_environment
                .artifacts
                .keys
                .contains(&QueryArtifact::GrammarStack.key(artifacts.source.id))
        );
        assert!(
            registered_environment
                .affordances
                .contains(EnvironmentAffordance::ShowGrammars)
        );

        let staged_adapter = eop_object(&objects, "staged-grammar-adapter:records");
        assert_eq!(staged_adapter.kind, EnvironmentKind::StagedGrammarAdapter);
        assert_eq!(staged_adapter.storage, EnvironmentStorage::FrontendRegistry);

        let grammar_extension = eop_object(&objects, "grammar-extension:records");
        assert_eq!(grammar_extension.kind, EnvironmentKind::GrammarExtension);
        assert!(
            grammar_extension
                .artifacts
                .keys
                .contains(&QueryArtifact::EnvironmentResolution.key(artifacts.source.id))
        );
        assert!(
            grammar_extension
                .affordances
                .contains(EnvironmentAffordance::ShowGrammarExtension)
        );

        let language_environment = eop_object(&objects, "language-environment:base-loko");
        assert_eq!(
            language_environment.kind,
            EnvironmentKind::LanguageEnvironment
        );
        assert!(
            language_environment
                .artifacts
                .keys
                .contains(&QueryArtifact::GrammarParse.key(artifacts.source.id))
        );
        assert!(
            language_environment
                .affordances
                .contains(EnvironmentAffordance::ShowGrammars)
        );

        let grammar = eop_object(&objects, "grammar:base-loko");
        assert_eq!(grammar.kind, EnvironmentKind::Grammar);
        assert!(
            grammar
                .artifacts
                .keys
                .contains(&QueryArtifact::GrammarStack.key(artifacts.source.id))
        );
        assert!(
            grammar
                .affordances
                .contains(EnvironmentAffordance::ShowGrammars)
        );
        assert!(
            grammar
                .affordances
                .contains(EnvironmentAffordance::ShowDirectRules)
        );
        assert!(
            grammar
                .affordances
                .contains(EnvironmentAffordance::ShowEffectiveRules)
        );

        let grammar_rule = eop_object(&objects, "grammar-rule:base-loko/function-definition");
        assert_eq!(grammar_rule.kind, EnvironmentKind::GrammarRule);
        assert!(
            grammar_rule
                .artifacts
                .keys
                .contains(&QueryArtifact::GrammarParse.key(artifacts.source.id))
        );

        let effective_rule = eop_object(
            &objects,
            "effective-grammar-rule:base-loko/function-definition",
        );
        assert_eq!(effective_rule.kind, EnvironmentKind::EffectiveGrammarRule);
        assert!(
            effective_rule
                .artifacts
                .keys
                .contains(&QueryArtifact::GrammarParse.key(artifacts.source.id))
        );

        let grammar_activation = eop_object(&objects, "grammar-activation:base-loko/source-file");
        assert_eq!(grammar_activation.kind, EnvironmentKind::GrammarActivation);
        assert_eq!(
            grammar_activation.source.source_file,
            Some(artifacts.source.id)
        );
        assert_eq!(
            grammar_activation.source.span,
            Some(Span::new(0, AREA.len()))
        );

        let root_class = eop_object(&objects, "class:c0");
        assert_eq!(root_class.kind, EnvironmentKind::Class);
        assert_eq!(root_class.origin, EnvironmentOrigin::CompilerGenerated);
        assert_eq!(root_class.storage, EnvironmentStorage::CachedArtifact);
        assert_eq!(root_class.presentation.title, "<object>");

        let class = eop_object(&objects, "class:c1");
        assert_eq!(class.kind, EnvironmentKind::Class);
        assert_eq!(class.origin, EnvironmentOrigin::SourceAuthored);
        assert_eq!(class.storage, EnvironmentStorage::SourceDefined);
        assert_eq!(
            class.source.definition,
            Some(artifacts.sem.classes[1].definition)
        );
        assert_eq!(class.source.span, Some(artifacts.sem.classes[1].span));
        assert!(
            class
                .artifacts
                .keys
                .contains(&QueryArtifact::Bind.key(artifacts.source.id))
        );
        assert!(
            class
                .affordances
                .contains(EnvironmentAffordance::ShowRuntimeLayout)
        );
        assert!(class.inspection.detail.contains("init-plan=[s0:radius"));
        assert!(
            class
                .inspection
                .detail
                .contains("accepted-keywords=[radius:]")
        );
        assert!(
            class
                .inspection
                .detail
                .contains("initialize-methods=deferred")
        );

        let slot = eop_object(&objects, "slot:c1.s0");
        assert_eq!(slot.kind, EnvironmentKind::Slot);
        assert!(
            slot.affordances
                .contains(EnvironmentAffordance::ShowAccessor)
        );

        let accessor = eop_object(&objects, "accessor:c1.s0");
        assert_eq!(accessor.kind, EnvironmentKind::Accessor);
        assert!(
            accessor
                .execution
                .as_ref()
                .is_some_and(|execution| execution.detail.contains("reads=slot:c1.s0"))
        );

        let generic = eop_object(&objects, "generic:g0");
        assert_eq!(generic.kind, EnvironmentKind::Generic);
        assert!(
            generic
                .affordances
                .contains(EnvironmentAffordance::AddMethod)
        );
        assert!(
            generic.artifacts.keys.contains(
                &QueryArtifact::GenericDispatcher(artifacts.sem.generics[0].definition)
                    .key(artifacts.source.id)
            )
        );

        let method = eop_object(&objects, "method:m0");
        assert_eq!(method.kind, EnvironmentKind::Method);
        assert!(
            method
                .execution
                .as_ref()
                .is_some_and(|execution| execution.detail.contains("participates-in=generic:g0"))
        );
        assert!(
            method.artifacts.keys.contains(
                &QueryArtifact::MethodLowering(artifacts.sem.methods[0].definition)
                    .key(artifacts.source.id)
            )
        );

        let function = eop_object(&objects, "function:f0");
        assert_eq!(function.kind, EnvironmentKind::Function);
        assert!(function.affordances.contains(EnvironmentAffordance::Run));

        let dispatch = eop_object(&objects, "dispatch:d0");
        assert_eq!(dispatch.kind, EnvironmentKind::DispatchSite);
        assert!(
            dispatch
                .affordances
                .contains(EnvironmentAffordance::ShowDispatch)
        );
        assert!(dispatch.inspection.detail.contains("generic=g0:area"));
        assert_eq!(
            dispatch.source.definition,
            Some(artifacts.sem.functions[0].definition)
        );
        assert!(
            dispatch
                .artifacts
                .keys
                .contains(&QueryArtifact::TapeProgram.key(artifacts.source.id))
        );

        let runtime_root_class = eop_object(&objects, "runtime-class:c0");
        assert_eq!(runtime_root_class.kind, EnvironmentKind::RuntimeLayoutRow);
        assert_eq!(runtime_root_class.presentation.title, "runtime <object>");

        let runtime_class = eop_object(&objects, "runtime-class:c1");
        assert_eq!(runtime_class.kind, EnvironmentKind::RuntimeLayoutRow);
        assert!(
            runtime_class
                .inspection
                .detail
                .contains("storage-kind=native")
        );

        assert!(
            objects
                .iter()
                .any(|object| object.kind == EnvironmentKind::Artifact
                    && object.storage == EnvironmentStorage::CachedArtifact)
        );
    }

    #[test]
    fn session_dumps_stages_for_selected_environment_objects() {
        let artifacts = CompilerSession::compile_source(
            "functions.loko",
            "define function answer()\n  42\nend;\n\
             define function other()\n  7\nend;",
            RuntimeMode::CompileOnly,
        );

        let sem = artifacts
            .dump_environment_object_stage("function:f0", DumpStage::Sem)
            .expect("function object should dump");
        assert!(sem.contains("sem object function:f0"));
        assert!(sem.contains("name: answer"));
        assert!(!sem.contains("name: other"));

        let lowered = artifacts
            .dump_environment_object_stage("function:f0", DumpStage::Lowered)
            .expect("function object should dump lowered");
        assert!(lowered.contains("lowered object function:f0"));
        assert!(lowered.contains("kind: function:f0"));

        let grammar = artifacts
            .dump_environment_object_stage("grammar:base-loko", DumpStage::Mop)
            .expect("grammar object should dump");
        assert!(grammar.contains("grammar grammar:base-loko"));
        assert!(grammar.contains("direct-rules:"));
        assert!(grammar.contains("base-loko/function-definition"));

        let grammar_rule = artifacts
            .dump_environment_object_stage(
                "grammar-rule:base-loko/function-definition",
                DumpStage::Mop,
            )
            .expect("grammar rule object should dump");
        assert!(grammar_rule.contains("grammar rule grammar-rule:base-loko/function-definition"));
        assert!(grammar_rule.contains("expression"));

        assert!(
            artifacts
                .dump_environment_object_stage("missing:object", DumpStage::Sem)
                .is_none()
        );
    }

    #[test]
    fn session_dumps_semantic_objects_by_kind() {
        let artifacts =
            CompilerSession::compile_source("area.loko", AREA, RuntimeMode::CompileOnly);

        let class = artifacts
            .dump_environment_object_stage("class:c1", DumpStage::Sem)
            .expect("class object should dump");
        assert!(class.contains("sem object class:c1"));
        assert!(class.contains("kind: class"));
        assert!(class.contains("effective-slots: 1"));
        assert!(class.contains("radius :: <integer>"));

        let generic = artifacts
            .dump_environment_object_stage("generic:g0", DumpStage::Sem)
            .expect("generic object should dump");
        assert!(generic.contains("kind: generic"));
        assert!(generic.contains("name: area"));
        assert!(generic.contains("dispatch-params:"));
        assert!(generic.contains("method:m0"));

        let method = artifacts
            .dump_environment_object_stage("method:m0", DumpStage::Sem)
            .expect("method object should dump");
        assert!(method.contains("kind: method"));
        assert!(method.contains("generic: g0 area"));
        assert!(method.contains("specializers: c1:<circle>"));

        let dispatch = artifacts
            .dump_environment_object_stage("dispatch:d0", DumpStage::Sem)
            .expect("dispatch object should dump");
        assert!(dispatch.contains("dispatch dispatch:d0"));
        assert!(dispatch.contains("generic: g0:area"));
        assert!(dispatch.contains("selected: m0:area(c1:<circle>)"));
    }

    #[test]
    fn session_projects_staged_extension_records() {
        let source = "// grammar-activate: calculator\n\
                      // staged-loko: calculator\n\
                      main = 40 + 2\n\
                      // end-staged-loko\n";
        let artifacts =
            CompilerSession::compile_source("staged.loko", source, RuntimeMode::CompileOnly);
        let objects = artifacts.environment_objects();

        let activation = eop_object(&objects, "grammar-extension-activation:calculator/r0");
        assert_eq!(activation.kind, EnvironmentKind::GrammarExtensionActivation);
        assert!(
            activation
                .inspection
                .detail
                .contains("capabilities=[emit-frontend-forms]")
        );
        assert_eq!(activation.source.source_file, Some(artifacts.source.id));
        assert!(activation.source.span.is_some());
        assert!(
            activation
                .artifacts
                .keys
                .contains(&QueryArtifact::GrammarParse.key(artifacts.source.id))
        );

        let package = eop_object(&objects, "grammar-package:calculator");
        assert_eq!(package.kind, EnvironmentKind::GrammarPackage);
        assert!(
            package
                .inspection
                .detail
                .contains("resources=[grammars/calculator.grammar]")
        );

        let resource = eop_object(
            &objects,
            "grammar-resource:calculator/grammars/calculator.grammar",
        );
        assert_eq!(resource.kind, EnvironmentKind::GrammarResource);
        assert!(resource.inspection.detail.contains("rules=20"));

        let action = eop_object(&objects, "grammar-action:calculator/calculator-function");
        assert_eq!(action.kind, EnvironmentKind::GrammarAction);
        assert!(
            action
                .affordances
                .contains(EnvironmentAffordance::ShowAction)
        );
        assert!(
            action
                .affordances
                .contains(EnvironmentAffordance::ShowCaptures)
        );
        assert!(
            action
                .affordances
                .contains(EnvironmentAffordance::ShowActionOutput)
        );
        assert!(action.inspection.detail.contains("action=emit-function"));
        assert_eq!(action.source.source_file, Some(artifacts.source.id));
        assert!(action.source.span.is_some());

        let emission = eop_object(&objects, "grammar-extension-emission:calculator/r0/f0");
        assert_eq!(emission.kind, EnvironmentKind::GrammarExtensionEmission);
        assert!(
            emission
                .inspection
                .detail
                .contains("kind=frontend-function")
        );
        assert!(emission.inspection.detail.contains("generated=function:f0"));
        assert_eq!(emission.source.source_file, Some(artifacts.source.id));
        assert!(emission.source.span.is_some());

        let provenance = eop_object(
            &objects,
            "grammar-extension-provenance:calculator/r0/calculator-function",
        );
        assert_eq!(provenance.kind, EnvironmentKind::GrammarExtensionProvenance);
        assert!(provenance.inspection.detail.contains("nodes=1"));
        assert_eq!(provenance.source.source_file, Some(artifacts.source.id));
        assert!(provenance.source.span.is_some());

        let dump = artifacts.dump_grammar_extensions();
        assert!(dump.contains("grammar extensions"));
        assert!(dump.contains("grammar-extension:calculator"));
        assert!(dump.contains("grammar-extension-emission:calculator/r0/f0"));
        assert!(dump.contains("generated=function:f0"));
        assert!(
            artifacts
                .explain_grammar_extension("grammar-extension:calculator")
                .expect("extension descriptor explanation")
                .contains("capabilities emit-frontend-forms")
        );
        let emission_dump = artifacts
            .dump_environment_object_stage(
                "grammar-extension-emission:calculator/r0/f0",
                DumpStage::Mop,
            )
            .expect("extension emission object should dump");
        assert!(emission_dump.contains("grammar extension emission"));
        assert!(emission_dump.contains("generated function:f0"));

        let action_dump = artifacts
            .explain_grammar_action("grammar-action:calculator/calculator-function")
            .expect("grammar action should explain");
        assert!(action_dump.contains("name emit-function"));
        assert!(action_dump.contains("body capture(expression)"));
        assert!(
            artifacts
                .explain_grammar_action_output("grammar-action:calculator/calculator-function")
                .expect("grammar action output should explain")
                .contains("produces frontend function form")
        );
    }

    #[test]
    fn session_lowers_staged_forms_directly_into_ast() {
        let source = "// grammar-activate: records\n\
                      // staged-loko: records\n\
                      record circle\n\
                        radius: <integer>\n\
                      end\n\
                      // end-staged-loko\n\
                      define function main()\n\
                        let c = make(<circle>, radius: 10);\n\
                        radius(c)\n\
                      end\n";
        let artifacts =
            CompilerSession::compile_source("records.loko", source, RuntimeMode::RunMain);

        assert!(matches!(
            artifacts.runtime_result,
            Some(RuntimeResult::Returned(FunctionValue::Integer(10)))
        ));
        assert_eq!(artifacts.diagnostics, Vec::new());
        assert!(
            artifacts
                .syntax
                .root
                .child_nodes(crate::syntax::CstKind::Class)
                .next()
                .is_none()
        );
        assert_eq!(artifacts.ast.classes.len(), 1);
        assert_eq!(artifacts.ast.classes[0].name, "<circle>");
        assert_eq!(artifacts.ast.classes[0].slots[0].name, "radius");
        assert_eq!(artifacts.sem.classes[0].name, "<object>");
        assert_eq!(artifacts.sem.classes[1].name, "<circle>");
    }

    #[test]
    fn session_dumps_host_backed_metaobjects() {
        let mut registry = sem::HostMetaRegistry::new();
        registry.add_class(
            sem::HostClass::new("<rust-circle>").with_slot(sem::HostSlot::new(
                "radius",
                sem::ValueKind::I64,
                "<integer>",
            )),
        );
        let artifacts = CompilerSession::compile_source_with_host_registry(
            "host.loko",
            "define generic area(x :: <object>) => <integer>;\n\
             define method area(c :: <rust-circle>) => <integer>\n  c.radius * c.radius\nend;",
            RuntimeMode::CompileOnly,
            &registry,
        );

        assert_eq!(artifacts.diagnostics, Vec::new());
        let sem = artifacts.dump_stage(DumpStage::Sem);
        assert!(sem.contains("class c0"));
        assert!(sem.contains("<object> origin=object-root"));
        assert!(sem.contains("class c1"));
        assert!(sem.contains("origin=host-backed(hc0)"));
        assert!(sem.contains("slot s0"));
        assert!(sem.contains("origin=host-backed(hc0.hs0)"));
        let mop = artifacts.dump_stage(DumpStage::Mop);
        assert!(mop.contains("<rust-circle> origin=host-backed(hc0)"));
        assert!(mop.contains("runtime=host-backed"));
        assert!(mop.contains("storage=host-backed"));
        assert!(mop.contains(
            "m0 specializers=(c1:<rust-circle>) specializer-origins=(host-backed(hc0)) method-origin=source"
        ));
        let eop = artifacts.dump_stage(DumpStage::Eop);
        assert!(eop.contains("object class:c0"));
        assert!(eop.contains("origin=compiler-generated storage=cached-artifact"));
        assert!(eop.contains("object class:c1"));
        assert!(eop.contains("origin=host-backed storage=host-registry"));
        assert!(eop.contains("object slot:c1.s0"));
        assert!(eop.contains("storage=host-adapter"));
        assert!(eop.contains("object runtime-class:c1"));

        let objects = artifacts.environment_objects();
        let class = eop_object(&objects, "class:c1");
        assert_eq!(class.origin, EnvironmentOrigin::HostBacked);
        assert_eq!(class.storage, EnvironmentStorage::HostRegistry);
        let slot = eop_object(&objects, "slot:c1.s0");
        assert_eq!(slot.origin, EnvironmentOrigin::HostBacked);
        assert_eq!(slot.storage, EnvironmentStorage::HostAdapter);
    }

    struct ExampleHostProvider;

    impl HostEnvironmentProvider for ExampleHostProvider {
        fn project_environment(&self, sink: &mut dyn EnvironmentObjectSink) {
            sink.emit(EnvironmentObject {
                id: EnvironmentObjectId::new("host:rust-circle"),
                kind: EnvironmentKind::HostObject,
                meta_class: "<host-object>".to_string(),
                origin: EnvironmentOrigin::HostProvided,
                storage: EnvironmentStorage::HostOwned,
                presentation: PresentationFacet::new("Rust circle instance"),
                source: SourceFacet::none(),
                documentation: DocumentationFacet::new(Vec::new()),
                artifacts: ArtifactFacet::new(Vec::new()),
                affordances: AffordanceFacet::new(Vec::from([
                    EnvironmentAffordance::Inspect,
                    EnvironmentAffordance::DumpEop,
                    EnvironmentAffordance::ShowRuntimeLayout,
                ])),
                execution: None,
                inspection: InspectionFacet::new("provider=example type=RustCircle"),
                typing: None,
            });
        }
    }

    #[test]
    fn host_environment_provider_projects_explicit_records() {
        let artifacts =
            CompilerSession::compile_source("area.loko", AREA, RuntimeMode::CompileOnly);
        let objects = artifacts.environment_objects_with_host(&ExampleHostProvider);

        let host = eop_object(&objects, "host:rust-circle");
        assert_eq!(host.kind, EnvironmentKind::HostObject);
        assert_eq!(host.origin, EnvironmentOrigin::HostProvided);
        assert_eq!(host.storage, EnvironmentStorage::HostOwned);
        assert!(host.affordances.contains(EnvironmentAffordance::DumpEop));
        assert!(host.inspection.detail.contains("type=RustCircle"));

        assert!(
            objects
                .iter()
                .any(|object| object.id.as_str() == "class:c0")
        );
    }

    #[test]
    fn session_projects_structured_diagnostic_records() {
        let artifacts = CompilerSession::compile_source(
            "bad.loko",
            "define function main()\n  missing\nend;",
            RuntimeMode::CompileOnly,
        );
        assert_eq!(artifacts.diagnostics.len(), 1);
        let diagnostic = &artifacts.diagnostics[0];
        assert_eq!(diagnostic.code, DiagnosticCode::UnresolvedLocal);
        assert!(
            diagnostic
                .args
                .iter()
                .any(|arg| arg.name == "local" && arg.value.render() == "missing")
        );

        let objects = artifacts.environment_objects();
        let diagnostic_object = eop_object(&objects, "diagnostic:d0");
        assert_eq!(diagnostic_object.kind, EnvironmentKind::Diagnostic);
        assert!(
            diagnostic_object
                .presentation
                .title
                .contains("sem.unresolved-local")
        );
        assert!(
            diagnostic_object
                .inspection
                .detail
                .contains("code=sem.unresolved-local")
        );
        assert!(
            diagnostic_object
                .inspection
                .detail
                .contains("args=[local=missing]")
        );
        assert!(!diagnostic_object.inspection.detail.contains("rendered="));
    }

    #[test]
    fn session_diff_preserves_ids_for_harmless_edit() {
        let before = CompilerSession::compile_source("area.loko", AREA, RuntimeMode::CompileOnly);
        let after = CompilerSession::compile_source(
            "area.loko",
            "\n// same program with trivia edits\n\
             define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius   *   c.radius\nend;\n\
             define function main()\n  area(make(<circle>, radius: 10))\nend;",
            RuntimeMode::CompileOnly,
        );
        let diff = CompilerSession::diff(&before, &after);

        assert!(diff.changed.is_empty());
        assert!(diff.affected.is_empty());
        assert!(
            diff.preserved
                .iter()
                .any(|definition| definition.kind == DefinitionKind::Method)
        );
    }

    #[test]
    fn session_diff_reports_focused_method_artifacts() {
        let before = CompilerSession::compile_source("area.loko", AREA, RuntimeMode::CompileOnly);
        let after = CompilerSession::compile_source(
            "area.loko",
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius + c.radius\nend;\n\
             define function main()\n  area(make(<circle>, radius: 10))\nend;",
            RuntimeMode::CompileOnly,
        );
        let diff = CompilerSession::diff(&before, &after);

        assert!(
            diff.changed
                .iter()
                .any(|change| change.definition.kind == DefinitionKind::Method
                    && change.change == DefinitionChangeKind::Modified)
        );
        assert!(
            diff.affected
                .iter()
                .any(|report| matches!(report.artifact, QueryArtifact::MethodLookup(_)))
        );
        assert!(
            diff.affected
                .iter()
                .any(|report| matches!(report.artifact, QueryArtifact::TapeProgram))
        );
    }

    #[test]
    fn incremental_first_compile_populates_cache_and_repeat_reuses_all() {
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("area.loko", AREA);

        let first = session
            .compile(file, RuntimeMode::RunMain)
            .expect("source should exist");
        assert_eq!(first.recomputed_count(), first.reports.len());
        assert_eq!(first.reused_count(), 0);
        assert!(matches!(
            first.artifacts.runtime_result,
            Some(RuntimeResult::Returned(FunctionValue::Integer(100)))
        ));

        let second = session
            .compile(file, RuntimeMode::RunMain)
            .expect("source should exist");
        assert_eq!(second.reused_count(), second.reports.len());
        assert_eq!(second.recomputed_count(), 0);
    }

    #[test]
    fn incremental_trivia_edit_reuses_most_artifacts() {
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("area.loko", AREA);
        let _ = session
            .compile(file, RuntimeMode::RunMain)
            .expect("source should exist");

        let update = session
            .edit_and_compile(
                file,
                "\n// same definitions, with harmless trivia changes\n\
                 define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                 define generic area(x);\n\
                 define method area(c :: <circle>)\n  c.radius   *   c.radius\nend;\n\
                 define function main()\n  area(make(<circle>, radius: 10))\nend;",
                RuntimeMode::RunMain,
            )
            .expect("source should exist");
        let method = update.artifacts.sem.methods[0].definition;

        assert!(update.reused_count() > update.recomputed_count());
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(method)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(method)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::TapeProgram),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::Source),
            Some(CacheAction::Recomputed)
        );
    }

    #[test]
    fn incremental_method_body_edit_preserves_unrelated_method_artifacts() {
        let before = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                      define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
                      define generic area(x);\n\
                      define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
                      define method area(s :: <square>)\n  s.side * s.side\nend;\n\
                      define function main()\n  area(make(<circle>, radius: 10))\nend;";
        let after = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                     define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
                     define generic area(x);\n\
                     define method area(c :: <circle>)\n  c.radius + c.radius\nend;\n\
                     define method area(s :: <square>)\n  s.side * s.side\nend;\n\
                     define function main()\n  area(make(<circle>, radius: 10))\nend;";
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("area.loko", before);
        let first = session
            .compile(file, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let circle = first.artifacts.sem.methods[0].definition;
        let square = first.artifacts.sem.methods[1].definition;

        let update = session
            .edit_and_compile(file, after, RuntimeMode::CompileOnly)
            .expect("source should exist");

        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(circle)),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(circle)),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(square)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(square)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::TapeProgram),
            Some(CacheAction::Recomputed)
        );
    }

    #[test]
    fn incremental_branch_edit_recomputes_only_affected_method_artifacts() {
        let before = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                      define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
                      define generic area(x);\n\
                      define method area(c :: <circle>)\n  if c.radius > 0 then c.radius * c.radius else 0 end\nend;\n\
                      define method area(s :: <square>)\n  s.side * s.side\nend;\n\
                      define function main()\n  area(make(<circle>, radius: 10))\nend;";
        let after = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                     define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
                     define generic area(x);\n\
                     define method area(c :: <circle>)\n  if c.radius > 0 then c.radius + c.radius else 0 end\nend;\n\
                     define method area(s :: <square>)\n  s.side * s.side\nend;\n\
                     define function main()\n  area(make(<circle>, radius: 10))\nend;";
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("area.loko", before);
        let first = session
            .compile(file, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let circle = first.artifacts.sem.methods[0].definition;
        let square = first.artifacts.sem.methods[1].definition;

        let update = session
            .edit_and_compile(file, after, RuntimeMode::CompileOnly)
            .expect("source should exist");

        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(circle)),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(circle)),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(square)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(square)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::LoweredModule),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::TapeProgram),
            Some(CacheAction::Recomputed)
        );
    }

    #[test]
    fn incremental_restart_handler_edit_recomputes_only_affected_method_artifacts() {
        let before = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                      define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
                      define class <invalid-radius> (<object>)\n  slot fallback :: <integer>;\nend;\n\
                      define generic area(x);\n\
                      define method area(c :: <circle>)\n  handle if c.radius < 0 then signal(make(<invalid-radius>, fallback: 0)) else c.radius * c.radius end with restart e :: <invalid-radius> e.fallback end\nend;\n\
                      define method area(s :: <square>)\n  s.side * s.side\nend;\n\
                      define function main()\n  area(make(<circle>, radius: 0 - 10))\nend;";
        let after = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                     define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
                     define class <invalid-radius> (<object>)\n  slot fallback :: <integer>;\nend;\n\
                     define generic area(x);\n\
                     define method area(c :: <circle>)\n  handle if c.radius < 0 then signal(make(<invalid-radius>, fallback: 0)) else c.radius * c.radius end with restart e :: <invalid-radius> e.fallback + 1 end\nend;\n\
                     define method area(s :: <square>)\n  s.side * s.side\nend;\n\
                     define function main()\n  area(make(<circle>, radius: 0 - 10))\nend;";
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("area.loko", before);
        let first = session
            .compile(file, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let circle = first.artifacts.sem.methods[0].definition;
        let square = first.artifacts.sem.methods[1].definition;

        let update = session
            .edit_and_compile(file, after, RuntimeMode::CompileOnly)
            .expect("source should exist");

        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(circle)),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(circle)),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(square)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(square)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::LoweredModule),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::TapeProgram),
            Some(CacheAction::Recomputed)
        );
    }

    #[test]
    fn incremental_slot_type_edit_recomputes_dependent_artifacts() {
        let before = "define class <flag> (<object>)\n  slot value :: <boolean>;\nend;\n\
                      define generic score(x);\n\
                      define method score(f :: <flag>)\n  if f.value then 1 else 0 end\nend;\n\
                      define function main()\n  score(make(<flag>, value: #t))\nend;";
        let after = "define class <flag> (<object>)\n  slot value :: <integer>;\nend;\n\
                     define generic score(x);\n\
                     define method score(f :: <flag>)\n  f.value\nend;\n\
                     define function main()\n  score(make(<flag>, value: 1))\nend;";
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("flag.loko", before);
        let first = session
            .compile(file, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let method = first.artifacts.sem.methods[0].definition;

        let update = session
            .edit_and_compile(file, after, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let diff = update.diff.as_ref().expect("edit should produce a diff");

        assert!(diff.changed.iter().any(|change| {
            change.definition.kind == DefinitionKind::Slot
                && change.change == DefinitionChangeKind::Modified
        }));
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(method)),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(method)),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::LoweredModule),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::TapeProgram),
            Some(CacheAction::Recomputed)
        );
        assert!(
            update
                .artifacts
                .dump_stage(DumpStage::Sem)
                .contains("value :: <integer>")
        );
    }

    #[test]
    fn incremental_slot_keyword_edit_reuses_lowering_when_runtime_layout_is_unchanged() {
        let before = "define class <circle> (<object>)\n  slot radius :: <integer>, init-keyword: radius:;\nend;\n\
                      define function main()\n  make(<circle>, radius: 10).radius\nend;";
        let after = "define class <circle> (<object>)\n  slot radius :: <integer>, init-keyword: r:;\nend;\n\
                     define function main()\n  make(<circle>, r: 10).radius\nend;";
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("circle.loko", before);
        let _ = session
            .compile(file, RuntimeMode::CompileOnly)
            .expect("source should exist");

        let update = session
            .edit_and_compile(file, after, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let diff = update.diff.as_ref().expect("edit should produce a diff");

        assert!(diff.changed.iter().any(|change| {
            change.definition.kind == DefinitionKind::Slot
                && change.change == DefinitionChangeKind::Modified
        }));
        assert_eq!(
            cache_action(&update, QueryArtifact::Bind),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::LoweredModule),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::TapeProgram),
            Some(CacheAction::Reused)
        );
        assert!(
            update
                .artifacts
                .dump_stage(DumpStage::Sem)
                .contains("init-keyword: r:")
        );
    }

    #[test]
    fn session_diff_preserves_ids_for_slot_declaration_trivia() {
        let before = CompilerSession::compile_source(
            "circle.loko",
            "define class <circle> (<object>)\n  slot radius :: <integer>, init-value: 10;\nend;\n\
             define function main()\n  make(<circle>).radius\nend;",
            RuntimeMode::CompileOnly,
        );
        let after = CompilerSession::compile_source(
            "circle.loko",
            "\n// same slot facts with trivia edits\n\
             define class <circle> (<object>)\n  slot radius   ::   <integer>,   init-value:   10;\nend;\n\
             define function main()\n  make(<circle>).radius\nend;",
            RuntimeMode::CompileOnly,
        );
        let diff = CompilerSession::diff(&before, &after);

        assert!(diff.changed.is_empty());
        assert!(diff.affected.is_empty());
        assert!(diff.preserved.iter().any(|definition| {
            definition.kind == DefinitionKind::Slot && definition.name == "<circle>.radius"
        }));
        assert_eq!(
            before.sem.classes[0].initialization_plan,
            after.sem.classes[0].initialization_plan
        );
        assert!(
            after
                .dump_stage(DumpStage::Sem)
                .contains("field s0 source=default")
        );
    }

    #[test]
    fn session_diff_preserves_ids_for_superclass_trivia() {
        let before = CompilerSession::compile_source(
            "point.loko",
            "define class <shape> (<object>)\n  slot x :: <integer>, init-value: 1;\nend;\n\
             define class <point> (<shape>)\n  slot y :: <integer>, init-value: 2;\nend;\n\
             define function main()\n  make(<point>).x\nend;",
            RuntimeMode::CompileOnly,
        );
        let after = CompilerSession::compile_source(
            "point.loko",
            "\n// same inheritance facts with trivia edits\n\
             define class <shape> (<object>)\n  slot x   ::   <integer>, init-value: 1;\nend;\n\
             define class <point>   (   <shape>   )\n  slot y :: <integer>, init-value: 2;\nend;\n\
             define function main()\n  make(<point>).x\nend;",
            RuntimeMode::CompileOnly,
        );
        let diff = CompilerSession::diff(&before, &after);

        assert!(diff.changed.is_empty());
        assert!(diff.affected.is_empty());
        assert!(diff.preserved.iter().any(|definition| {
            definition.kind == DefinitionKind::Class && definition.name == "<point>"
        }));
    }

    #[test]
    fn parent_slot_edit_affects_child_finalization_and_executable_artifacts() {
        let before = "define class <shape> (<object>)\n  slot x :: <integer>, init-value: 1;\nend;\n\
                      define class <point> (<shape>)\n  slot y :: <integer>, init-value: 2;\nend;\n\
                      define function main()\n  make(<point>).x\nend;";
        let after = "define class <shape> (<object>)\n  slot x :: <integer>, init-value: 3;\nend;\n\
                     define class <point> (<shape>)\n  slot y :: <integer>, init-value: 2;\nend;\n\
                     define function main()\n  make(<point>).x\nend;";
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("point.loko", before);
        let _ = session
            .compile(file, RuntimeMode::CompileOnly)
            .expect("source should exist");

        let update = session
            .edit_and_compile(file, after, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let diff = update.diff.as_ref().expect("edit should produce a diff");

        assert!(diff.changed.iter().any(|change| {
            change.definition.kind == DefinitionKind::Slot
                && change.definition.name == "<shape>.x"
                && change.change == DefinitionChangeKind::Modified
        }));
        assert!(diff.changed.iter().any(|change| {
            change.definition.kind == DefinitionKind::Class
                && change.definition.name == "<point>"
                && change.change == DefinitionChangeKind::Modified
        }));
        assert_eq!(
            cache_action(&update, QueryArtifact::LoweredModule),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::TapeProgram),
            Some(CacheAction::Recomputed)
        );
        assert!(
            update
                .artifacts
                .dump_stage(DumpStage::Mop)
                .contains("source=inherited")
        );
    }

    #[test]
    fn parent_method_edit_affects_subclass_dispatch_artifacts() {
        let before = "define class <shape> (<object>)\n  slot size :: <integer>, init-value: 1;\nend;\n\
                      define class <circle> (<shape>)\n  slot radius :: <integer>, init-value: 2;\nend;\n\
                      define generic score(x :: <shape>) => <integer>;\n\
                      define method score(s :: <shape>) => <integer>\n  s.size\nend;\n\
                      define function main()\n  score(make(<circle>))\nend;";
        let after = "define class <shape> (<object>)\n  slot size :: <integer>, init-value: 1;\nend;\n\
                     define class <circle> (<shape>)\n  slot radius :: <integer>, init-value: 2;\nend;\n\
                     define generic score(x :: <shape>) => <integer>;\n\
                     define method score(s :: <shape>) => <integer>\n  s.size + 1\nend;\n\
                     define function main()\n  score(make(<circle>))\nend;";
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("score.loko", before);
        let first = session
            .compile(file, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let method = first.artifacts.sem.methods[0].definition;
        let generic = first.artifacts.sem.generics[0].definition;

        let update = session
            .edit_and_compile(file, after, RuntimeMode::CompileOnly)
            .expect("source should exist");

        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLookup(method)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(method)),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(method)),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::GenericDispatcher(generic)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::LoweredModule),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::TapeProgram),
            Some(CacheAction::Recomputed)
        );
    }

    #[test]
    fn child_slot_edit_does_not_mark_parent_definitions_changed() {
        let before = CompilerSession::compile_source(
            "point.loko",
            "define class <shape> (<object>)\n  slot x :: <integer>, init-value: 1;\nend;\n\
             define class <point> (<shape>)\n  slot y :: <integer>, init-value: 2;\nend;\n\
             define function main()\n  make(<point>).y\nend;",
            RuntimeMode::CompileOnly,
        );
        let after = CompilerSession::compile_source(
            "point.loko",
            "define class <shape> (<object>)\n  slot x :: <integer>, init-value: 1;\nend;\n\
             define class <point> (<shape>)\n  slot y :: <integer>, init-value: 3;\nend;\n\
             define function main()\n  make(<point>).y\nend;",
            RuntimeMode::CompileOnly,
        );
        let diff = CompilerSession::diff(&before, &after);

        assert!(!diff.changed.iter().any(|change| {
            change.definition.name == "<shape>" || change.definition.name == "<shape>.x"
        }));
        assert!(diff.changed.iter().any(|change| {
            change.definition.kind == DefinitionKind::Slot && change.definition.name == "<point>.y"
        }));
    }

    #[test]
    fn adding_subclass_preserves_parent_definition_ids() {
        let before = CompilerSession::compile_source(
            "shape.loko",
            "define class <shape> (<object>)\n  slot x :: <integer>, init-value: 1;\nend;\n\
             define function main()\n  make(<shape>).x\nend;",
            RuntimeMode::CompileOnly,
        );
        let after = CompilerSession::compile_source(
            "shape.loko",
            "define class <shape> (<object>)\n  slot x :: <integer>, init-value: 1;\nend;\n\
             define class <point> (<shape>)\n  slot y :: <integer>, init-value: 2;\nend;\n\
             define function main()\n  make(<shape>).x\nend;",
            RuntimeMode::CompileOnly,
        );
        let diff = CompilerSession::diff(&before, &after);

        assert!(diff.preserved.iter().any(|definition| {
            definition.kind == DefinitionKind::Class && definition.name == "<shape>"
        }));
        assert!(diff.preserved.iter().any(|definition| {
            definition.kind == DefinitionKind::Slot && definition.name == "<shape>.x"
        }));
        assert!(!diff.changed.iter().any(|change| {
            change.definition.name == "<shape>" || change.definition.name == "<shape>.x"
        }));
        assert!(diff.changed.iter().any(|change| {
            change.definition.kind == DefinitionKind::Class
                && change.definition.name == "<point>"
                && change.change == DefinitionChangeKind::Added
        }));
    }

    #[test]
    fn incremental_slot_init_value_edit_recomputes_executable_artifacts() {
        let before = "define class <circle> (<object>)\n  slot radius :: <integer>, init-value: 10;\nend;\n\
                      define function main()\n  make(<circle>).radius\nend;";
        let after = "define class <circle> (<object>)\n  slot radius :: <integer>, init-value: 11;\nend;\n\
                     define function main()\n  make(<circle>).radius\nend;";
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("circle.loko", before);
        let _ = session
            .compile(file, RuntimeMode::CompileOnly)
            .expect("source should exist");

        let update = session
            .edit_and_compile(file, after, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let diff = update.diff.as_ref().expect("edit should produce a diff");

        assert!(diff.changed.iter().any(|change| {
            change.definition.kind == DefinitionKind::Slot
                && change.change == DefinitionChangeKind::Modified
        }));
        assert_eq!(
            cache_action(&update, QueryArtifact::Bind),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::LoweredModule),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::TapeProgram),
            Some(CacheAction::Recomputed)
        );
        assert!(
            update
                .artifacts
                .dump_stage(DumpStage::Sem)
                .contains("init-value=yes")
        );
    }

    #[test]
    fn incremental_slot_mutability_edit_recomputes_executable_artifacts() {
        let before = "define class <circle> (<object>)\n  slot radius :: <integer>, init-value: 10;\nend;\n\
                      define function main()\n  make(<circle>).radius\nend;";
        let after = "define class <circle> (<object>)\n  constant slot radius :: <integer>, init-value: 10;\nend;\n\
                     define function main()\n  make(<circle>).radius\nend;";
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("circle.loko", before);
        let _ = session
            .compile(file, RuntimeMode::CompileOnly)
            .expect("source should exist");

        let update = session
            .edit_and_compile(file, after, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let diff = update.diff.as_ref().expect("edit should produce a diff");

        assert!(diff.changed.iter().any(|change| {
            change.definition.kind == DefinitionKind::Slot
                && change.change == DefinitionChangeKind::Modified
        }));
        assert_eq!(
            cache_action(&update, QueryArtifact::Bind),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::LoweredModule),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::TapeProgram),
            Some(CacheAction::Recomputed)
        );
        assert!(
            update
                .artifacts
                .dump_stage(DumpStage::Sem)
                .contains("mutability=constant")
        );
    }

    #[test]
    fn incremental_definition_edit_marks_broad_dependents_affected() {
        let before = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                      define generic area(x);\n\
                      define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
                      define function main()\n  area(make(<circle>, radius: 10))\nend;";
        let after = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                     define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
                     define generic area(x);\n\
                     define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
                     define method area(s :: <square>)\n  s.side * s.side\nend;\n\
                     define function main()\n  area(make(<circle>, radius: 10))\nend;";
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("area.loko", before);
        let _ = session
            .compile(file, RuntimeMode::CompileOnly)
            .expect("source should exist");

        let update = session
            .edit_and_compile(file, after, RuntimeMode::CompileOnly)
            .expect("source should exist");

        assert!(update.invalidated_count() > 0);
        assert!(cache_report(&update, QueryArtifact::Bind).is_some_and(|report| report.affected));
        assert!(
            cache_report(&update, QueryArtifact::LoweredModule)
                .is_some_and(|report| report.affected)
        );
        assert!(
            cache_report(&update, QueryArtifact::TapeProgram).is_some_and(|report| report.affected)
        );
    }

    #[test]
    fn incremental_class_reference_edit_recomputes_only_dependent_artifacts() {
        let before = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                      define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
                      define generic area(x);\n\
                      define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
                      define method area(s :: <square>)\n  s.side * s.side\nend;\n\
                      define function main()\n  let klass = <circle>;\n  area(make(klass, radius: 10))\nend;";
        let after = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                     define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
                     define generic area(x);\n\
                     define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
                     define method area(s :: <square>)\n  s.side * s.side\nend;\n\
                     define function main()\n  let klass = <square>;\n  area(make(klass, side: 9))\nend;";
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("area.loko", before);
        let first = session
            .compile(file, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let circle = first.artifacts.sem.methods[0].definition;
        let square = first.artifacts.sem.methods[1].definition;

        let update = session
            .edit_and_compile(file, after, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let diff = update.diff.as_ref().expect("edit should produce a diff");

        assert!(diff.changed.iter().any(|change| {
            change.definition.kind == DefinitionKind::Function
                && change.change == DefinitionChangeKind::Modified
        }));
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(circle)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(circle)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(square)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(square)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::LoweredModule),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::TapeProgram),
            Some(CacheAction::Recomputed)
        );
        assert!(
            update
                .artifacts
                .dump_stage(DumpStage::Sem)
                .contains("class-object c2")
        );
    }

    #[test]
    fn incremental_callable_target_edit_recomputes_only_function_artifacts() {
        let before = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                      define generic area(x);\n\
                      define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
                      define function inc(x)\n  x + 1\nend;\n\
                      define function dec(x)\n  x - 1\nend;\n\
                      define function main()\n  let f = inc;\n  apply(f, 10) + area(make(<circle>, radius: 2))\nend;";
        let after = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                     define generic area(x);\n\
                     define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
                     define function inc(x)\n  x + 1\nend;\n\
                     define function dec(x)\n  x - 1\nend;\n\
                     define function main()\n  let f = dec;\n  apply(f, 10) + area(make(<circle>, radius: 2))\nend;";
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("callable.loko", before);
        let first = session
            .compile(file, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let method = first.artifacts.sem.methods[0].definition;

        let update = session
            .edit_and_compile(file, after, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let diff = update.diff.as_ref().expect("edit should produce a diff");

        assert!(diff.changed.iter().any(|change| {
            change.definition.kind == DefinitionKind::Function
                && change.definition.name == "main"
                && change.change == DefinitionChangeKind::Modified
        }));
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(method)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(method)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::LoweredModule),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::TapeProgram),
            Some(CacheAction::Recomputed)
        );
        assert!(
            update
                .artifacts
                .dump_stage(DumpStage::Sem)
                .contains("known-callable function f1")
        );
    }

    #[test]
    fn incremental_function_signature_edit_recomputes_callers_without_method_recompute() {
        let before = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                      define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
                      define generic area(x);\n\
                      define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
                      define method area(s :: <square>)\n  s.side * s.side\nend;\n\
                      define function hold(x)\n  x\nend;\n\
                      define function main()\n  let ignored = hold(7);\n  area(make(<circle>, radius: 10))\nend;";
        let after = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                     define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
                     define generic area(x);\n\
                     define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
                     define method area(s :: <square>)\n  s.side * s.side\nend;\n\
                     define function hold(x :: <object>) => <object>\n  x\nend;\n\
                     define function main()\n  let ignored = hold(7);\n  area(make(<circle>, radius: 10))\nend;";
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("area.loko", before);
        let first = session
            .compile(file, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let circle = first.artifacts.sem.methods[0].definition;
        let square = first.artifacts.sem.methods[1].definition;

        let update = session
            .edit_and_compile(file, after, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let diff = update.diff.as_ref().expect("edit should produce a diff");

        assert!(diff.changed.iter().any(|change| {
            change.definition.kind == DefinitionKind::Function
                && change.definition.name == "hold"
                && change.change == DefinitionChangeKind::Modified
        }));
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(circle)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(circle)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(square)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(square)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::LoweredModule),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::TapeProgram),
            Some(CacheAction::Recomputed)
        );
        assert!(
            update
                .artifacts
                .dump_stage(DumpStage::Sem)
                .contains("hold(x:object-top) result=object-top return=object-top")
        );
        assert!(
            update
                .artifacts
                .dump_stage(DumpStage::Lowered)
                .contains("box.any")
        );
    }

    fn cache_action(update: &IncrementalUpdate, artifact: QueryArtifact) -> Option<CacheAction> {
        cache_report(update, artifact).map(|report| report.action)
    }

    fn cache_report(
        update: &IncrementalUpdate,
        artifact: QueryArtifact,
    ) -> Option<&CacheArtifactReport> {
        update
            .reports
            .iter()
            .find(|report| report.artifact == artifact)
    }

    const MUTATION_FIXTURE: &str = "\
define class <circle> (<object>)
  slot radius :: <integer>;
end;

define class <square> (<object>)
  slot side :: <integer>;
end;

define class <triangle> (<object>)
  slot base :: <integer>;
end;

define generic area(x);

define method area(c :: <circle>)
  c.radius * c.radius
end;

define function score_circle()
  area(make(<circle>, radius: 10))
end;

define function score_square()
  area(make(<square>, side: 7))
end;

define function score_triangle()
  area(make(<triangle>, base: 9))
end;

define function unrelated_constant()
  4242
end;
";

    const MUTATION_TRIANGLE_METHOD: &str = "\
define method area(t :: <triangle>)
  t.base * t.base
end;
";

    const MUTATION_SQUARE_METHOD: &str = "\
define method area(s :: <square>)
  s.side * s.side
end;
";

    fn mutation_fixture() -> SessionArtifacts {
        CompilerSession::compile_source(
            "mutation_fixture.loko",
            MUTATION_FIXTURE,
            RuntimeMode::CompileOnly,
        )
    }

    fn generic_id(artifacts: &SessionArtifacts, name: &str) -> sem::GenericFunctionId {
        artifacts
            .sem
            .generics
            .iter()
            .find(|generic| generic.name == name)
            .map(|generic| generic.id)
            .expect("generic should exist")
    }

    fn artifact_keys(artifacts: &SessionArtifacts) -> Vec<ArtifactKey> {
        artifacts
            .artifact_reports()
            .into_iter()
            .map(|report| report.key)
            .collect()
    }

    #[test]
    fn apply_add_method_inserts_method_and_routes_dispatch_to_it() {
        let mut artifacts = mutation_fixture();
        let area = generic_id(&artifacts, "area");
        let circle_method_count_before = artifacts
            .sem
            .methods
            .iter()
            .filter(|method| method.generic == area)
            .count();
        assert_eq!(circle_method_count_before, 1);

        let circle_result = run_scalar(&artifacts, "score_circle");
        assert!(matches!(circle_result, Ok(FunctionValue::Integer(100))));

        let outcome = artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_SQUARE_METHOD.into(),
            })
            .expect("mutation should succeed");

        assert_eq!(outcome.mutation_id, 0);
        assert_eq!(
            outcome.recompute_strategy,
            RecomputeStrategy::FullCandidateRebuild
        );
        assert!(!outcome.affected_artifacts.is_empty());

        let inserted = &artifacts.sem.methods[outcome.inserted_method.0 as usize];
        assert_eq!(inserted.generic, area);
        assert!(matches!(
            inserted.origin,
            sem::MethodOrigin::Session { sequence: 0 }
        ));

        assert_eq!(
            artifacts
                .sem
                .methods
                .iter()
                .filter(|method| method.generic == area)
                .count(),
            2
        );

        let circle_after = run_scalar(&artifacts, "score_circle");
        assert!(matches!(circle_after, Ok(FunctionValue::Integer(100))));
        let square_after = run_scalar(&artifacts, "score_square");
        assert!(matches!(square_after, Ok(FunctionValue::Integer(49))));

        assert_eq!(artifacts.mutations().len(), 1);
    }

    #[test]
    fn numeric_generics_are_sealed_over_real() {
        let mut artifacts = CompilerSession::compile_source(
            "sealed.loko",
            "define class <point> (<object>)\n  slot x :: <integer>, init-value: 1;\nend;\n\
             define function bump(x :: <object>) => <object>\n  x + 1\nend;",
            RuntimeMode::CompileOnly,
        );
        assert_eq!(artifacts.diagnostics, Vec::new());
        let plus = artifacts
            .sem
            .generics
            .iter()
            .find(|generic| generic.name == "+")
            .expect("`+` generic is created by the dispatching operator");
        let (plus_id, origin) = (plus.id, plus.origin);
        let class = |name: &str| {
            artifacts
                .sem
                .classes
                .iter()
                .find(|class| class.name == name)
                .map(|class| class.id)
        };
        let (integer, float, point) = (
            class("<integer>"),
            class("<double-float>"),
            class("<point>"),
        );
        assert!(!origin.admits_method(&artifacts.sem, &[integer, integer]));
        assert!(!origin.admits_method(&artifacts.sem, &[float, integer]));
        assert!(origin.admits_method(&artifacts.sem, &[point, integer]));

        let result = artifacts.apply_mutation(SessionMutation::AddMethod {
            generic: plus_id,
            method_source: "define method plus(x :: <integer>, y :: <integer>)\n  0\nend;".into(),
        });
        assert!(matches!(
            result,
            Err(MutationError::SealedGeneric { generic }) if generic == plus_id
        ));
        assert_eq!(artifacts.mutations().len(), 0);
    }

    #[test]
    fn apply_add_method_rejects_extra_class_definition() {
        let mut artifacts = mutation_fixture();
        let area = generic_id(&artifacts, "area");

        // The mutation source defines a fresh class alongside the method.
        // <pentagon> is not in the fixture, so this is a genuine class
        // addition the affordance must reject.
        let extra_class_and_method = "\
define class <pentagon> (<object>)
  slot edges :: <integer>;
end;

define method area(p :: <pentagon>)
  p.edges
end;
";

        let result = artifacts.apply_mutation(SessionMutation::AddMethod {
            generic: area,
            method_source: extra_class_and_method.into(),
        });

        // Class mutations are out of scope for this slice; the candidate AST
        // gained two definitions, so the structural-delta guard rejects.
        assert!(matches!(result, Err(MutationError::NotAMethodDefinition)));
        assert_eq!(artifacts.mutations().len(), 0);
        assert!(
            !artifacts
                .sem
                .classes
                .iter()
                .any(|class| class.name == "<pentagon>"),
            "rejected class addition must not leak into the live session"
        );
    }

    #[test]
    fn apply_add_method_preserves_session_origin_across_multiple_mutations() {
        let mut artifacts = mutation_fixture();
        let area = generic_id(&artifacts, "area");

        let first = artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_SQUARE_METHOD.into(),
            })
            .expect("first mutation should succeed");
        assert_eq!(first.mutation_id, 0);

        let second = artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_TRIANGLE_METHOD.into(),
            })
            .expect("second mutation should succeed");
        assert_eq!(second.mutation_id, 1);
        assert_eq!(artifacts.mutations().len(), 2);

        // Both inserted methods keep their MethodOrigin::Session marker
        // after the second candidate-and-swap rebuild. Without the
        // re-tagging fix, the first method would silently revert to
        // MethodOrigin::Source because the recompile parses its appended
        // source as ordinary input.
        let square = &artifacts.sem.methods[first.inserted_method.0 as usize];
        assert!(
            matches!(square.origin, sem::MethodOrigin::Session { sequence: 0 }),
            "first session method should retain sequence 0, got {:?}",
            square.origin
        );
        let triangle = &artifacts.sem.methods[second.inserted_method.0 as usize];
        assert!(
            matches!(triangle.origin, sem::MethodOrigin::Session { sequence: 1 }),
            "second session method should carry sequence 1, got {:?}",
            triangle.origin
        );

        // Source-origin method survives both rebuilds unchanged.
        let circle_method = artifacts
            .sem
            .methods
            .iter()
            .find(|method| {
                method.generic == area && matches!(method.origin, sem::MethodOrigin::Source)
            })
            .expect("source-origin circle method should still exist");
        assert!(matches!(circle_method.origin, sem::MethodOrigin::Source));

        // All three dispatch paths work after the two mutations.
        let circle_result = run_scalar(&artifacts, "score_circle");
        assert!(matches!(circle_result, Ok(FunctionValue::Integer(100))));
        let square_result = run_scalar(&artifacts, "score_square");
        assert!(matches!(square_result, Ok(FunctionValue::Integer(49))));
        let triangle_result = run_scalar(&artifacts, "score_triangle");
        assert!(matches!(triangle_result, Ok(FunctionValue::Integer(81))));
    }

    #[test]
    fn apply_mutation_records_one_log_entry_per_successful_mutation() {
        let mut artifacts = mutation_fixture();
        let area = generic_id(&artifacts, "area");
        assert!(artifacts.mutations().is_empty());

        let first_outcome = artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_SQUARE_METHOD.into(),
            })
            .expect("first mutation should succeed");
        assert_eq!(artifacts.mutations().len(), 1);
        let first_entry = &artifacts.mutations()[0];
        assert_eq!(first_entry.mutation_id, 0);
        assert_eq!(first_entry.outcome.mutation_id, first_outcome.mutation_id);
        assert_eq!(
            first_entry.outcome.inserted_method,
            first_outcome.inserted_method
        );
        match &first_entry.request {
            SessionMutation::AddMethod {
                generic: g,
                method_source: src,
            } => {
                assert_eq!(*g, area);
                assert!(src.contains("<square>"));
            }
        }

        let second_outcome = artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_TRIANGLE_METHOD.into(),
            })
            .expect("second mutation should succeed");
        assert_eq!(artifacts.mutations().len(), 2);
        let entries = artifacts.mutations();
        assert_eq!(entries[1].mutation_id, 1);
        assert_eq!(entries[1].outcome.mutation_id, second_outcome.mutation_id);
        assert_eq!(
            entries[1].outcome.inserted_method,
            second_outcome.inserted_method
        );
        // The first entry survives unchanged across the second
        // candidate-and-swap rebuild.
        assert_eq!(entries[0].mutation_id, 0);
        assert_eq!(
            entries[0].outcome.inserted_method,
            first_outcome.inserted_method
        );
    }

    #[test]
    fn apply_mutation_failed_attempt_does_not_append_to_log() {
        let mut artifacts = mutation_fixture();
        let area = generic_id(&artifacts, "area");

        let first = artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_SQUARE_METHOD.into(),
            })
            .expect("first mutation should succeed");
        assert_eq!(artifacts.mutations().len(), 1);

        // Garbage source: should be rejected as a parse failure and must
        // not leave a log entry behind.
        let err = artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: "define method !!! garbage".into(),
            })
            .expect_err("garbage source should be rejected");
        assert!(matches!(err, MutationError::ParseFailed { .. }));
        assert_eq!(artifacts.mutations().len(), 1);
        assert_eq!(artifacts.mutations()[0].mutation_id, first.mutation_id);

        // Duplicate-specializer: also a rejection mode; same invariant.
        let err = artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: "define method area(s :: <square>)\n  s.side\nend;".into(),
            })
            .expect_err("duplicate specializer should be rejected");
        assert!(matches!(err, MutationError::DuplicateSpecializer { .. }));
        assert_eq!(artifacts.mutations().len(), 1);
    }

    #[test]
    fn mutation_log_drives_provenance_after_external_origin_tampering() {
        // The log is the source of truth for MethodOrigin::Session. Even if
        // a caller hand-edits the origin field between mutations, the next
        // candidate-and-swap rebuild restores it from the log entry.
        let mut artifacts = mutation_fixture();
        let area = generic_id(&artifacts, "area");

        let first = artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_SQUARE_METHOD.into(),
            })
            .expect("first mutation should succeed");
        artifacts.sem.methods[first.inserted_method.0 as usize].origin = sem::MethodOrigin::Source;

        artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_TRIANGLE_METHOD.into(),
            })
            .expect("second mutation should succeed");

        let square = &artifacts.sem.methods[first.inserted_method.0 as usize];
        assert!(
            matches!(square.origin, sem::MethodOrigin::Session { sequence: 0 }),
            "log-driven retag should restore the tampered origin; got {:?}",
            square.origin
        );
    }

    #[test]
    fn mutation_log_projects_one_eop_record_per_entry() {
        let mut artifacts = mutation_fixture();
        let area = generic_id(&artifacts, "area");

        // No mutations yet: no mutation:m* objects exist.
        let initial_objects = artifacts.environment_objects();
        assert!(
            !initial_objects
                .iter()
                .any(|object| object.id.as_str().starts_with("mutation:")),
            "no mutation EOP records should exist before any mutation is applied"
        );

        artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_SQUARE_METHOD.into(),
            })
            .expect("first mutation should succeed");

        let after_first = artifacts.environment_objects();
        let first = eop_object(&after_first, "mutation:m0");
        assert_eq!(first.kind, EnvironmentKind::Mutation);
        assert_eq!(first.origin, EnvironmentOrigin::Session);
        assert_eq!(first.storage, EnvironmentStorage::TransientInMemory);
        assert_eq!(first.meta_class, "<mutation>");
        let inspection = first.inspection.detail.as_str();
        assert!(inspection.contains("mutation-id=0"));
        assert!(inspection.contains("request=add-method"));
        assert!(inspection.contains("strategy=full-candidate-rebuild"));
        // The slice deliberately reports the strategy honestly; nothing in
        // the EOP surface should claim cache reuse yet.
        assert!(!inspection.contains("reused"));

        artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_TRIANGLE_METHOD.into(),
            })
            .expect("second mutation should succeed");

        let after_second = artifacts.environment_objects();
        let mutation_count = after_second
            .iter()
            .filter(|object| object.id.as_str().starts_with("mutation:"))
            .count();
        assert_eq!(mutation_count, 2);
        let second = eop_object(&after_second, "mutation:m1");
        assert_eq!(second.kind, EnvironmentKind::Mutation);
        assert!(second.inspection.detail.as_str().contains("mutation-id=1"));
    }

    #[test]
    fn rewind_to_first_id_keeps_first_and_drops_second_mutation() {
        let mut artifacts = mutation_fixture();
        let area = generic_id(&artifacts, "area");

        let first = artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_SQUARE_METHOD.into(),
            })
            .expect("first mutation should succeed");
        artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_TRIANGLE_METHOD.into(),
            })
            .expect("second mutation should succeed");
        assert_eq!(artifacts.mutations().len(), 2);

        let outcome = artifacts
            .rewind_to(Some(first.mutation_id))
            .expect("rewind should succeed");
        assert_eq!(outcome.retained_count, 1);
        assert_eq!(outcome.dropped.len(), 1);
        assert_eq!(outcome.dropped[0].mutation_id, 1);
        assert_eq!(
            outcome.recompute_strategy,
            RecomputeStrategy::FullCandidateRebuild
        );

        // The first session method survives with its provenance intact.
        assert_eq!(artifacts.mutations().len(), 1);
        let square = &artifacts.sem.methods[first.inserted_method.0 as usize];
        assert!(matches!(
            square.origin,
            sem::MethodOrigin::Session { sequence: 0 }
        ));

        // Dispatch through the surviving square method still works; the
        // triangle path is gone, so score_triangle no longer finds an
        // applicable method and traps at the runtime dispatcher.
        let square_result = run_scalar(&artifacts, "score_square");
        assert!(matches!(square_result, Ok(FunctionValue::Integer(49))));
        let circle_result = run_scalar(&artifacts, "score_circle");
        assert!(matches!(circle_result, Ok(FunctionValue::Integer(100))));
        let triangle_result = run_scalar(&artifacts, "score_triangle");
        assert!(matches!(
            triangle_result,
            Err(FunctionCallError::Trap { .. })
        ));
    }

    #[test]
    fn rewind_to_pristine_drops_all_mutations() {
        let mut artifacts = mutation_fixture();
        let area = generic_id(&artifacts, "area");

        artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_SQUARE_METHOD.into(),
            })
            .expect("first mutation should succeed");
        artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_TRIANGLE_METHOD.into(),
            })
            .expect("second mutation should succeed");

        let outcome = artifacts
            .rewind_to(None)
            .expect("rewind to pristine should succeed");
        assert_eq!(outcome.retained_count, 0);
        assert_eq!(outcome.dropped.len(), 2);

        assert!(artifacts.mutations().is_empty());
        // No session-origin methods remain on the area generic; the only
        // method on area is the pristine source method (circle).
        let area_methods: Vec<_> = artifacts
            .sem
            .methods
            .iter()
            .filter(|method| method.generic == area)
            .collect();
        assert_eq!(area_methods.len(), 1);
        assert!(matches!(area_methods[0].origin, sem::MethodOrigin::Source));

        let circle_result = run_scalar(&artifacts, "score_circle");
        assert!(matches!(circle_result, Ok(FunctionValue::Integer(100))));
        let square_result = run_scalar(&artifacts, "score_square");
        assert!(matches!(square_result, Err(FunctionCallError::Trap { .. })));
    }

    #[test]
    fn rewind_to_out_of_range_target_returns_error_and_preserves_state() {
        let mut artifacts = mutation_fixture();
        let area = generic_id(&artifacts, "area");

        artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_SQUARE_METHOD.into(),
            })
            .expect("first mutation should succeed");
        let before_log = artifacts.mutations().to_vec();
        let before_methods = artifacts.sem.methods.clone();
        let before_artifact_keys = artifact_keys(&artifacts);

        let err = artifacts
            .rewind_to(Some(99))
            .expect_err("out-of-range target should be rejected");
        match err {
            RewindError::TargetOutOfRange {
                target,
                current_count,
            } => {
                assert_eq!(target, 99);
                assert_eq!(current_count, 1);
            }
            other => panic!("unexpected rewind error: {other:?}"),
        }

        // Live session is byte-for-byte unchanged.
        assert_eq!(artifacts.mutations(), &before_log[..]);
        assert_eq!(artifacts.sem.methods, before_methods);
        assert_eq!(artifact_keys(&artifacts), before_artifact_keys);
    }

    #[test]
    fn rewind_does_not_reuse_mutation_ids_for_subsequent_apply() {
        let mut artifacts = mutation_fixture();
        let area = generic_id(&artifacts, "area");

        let first = artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_SQUARE_METHOD.into(),
            })
            .expect("first mutation should succeed");
        let dropped = artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_TRIANGLE_METHOD.into(),
            })
            .expect("second mutation should succeed");
        artifacts
            .rewind_to(Some(first.mutation_id))
            .expect("rewind should succeed");

        // Stable session event ids: the dropped id (1) is permanently
        // retired. The next apply gets id 2, not the recycled id 1.
        let next = artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_TRIANGLE_METHOD.into(),
            })
            .expect("post-rewind mutation should succeed");
        assert_ne!(
            next.mutation_id, dropped.mutation_id,
            "the dropped mutation id must not be reused"
        );
        assert_eq!(next.mutation_id, 2);

        // The current log carries [id 0, id 2] with a deliberate gap where
        // the dropped id 1 used to be.
        let log_ids: Vec<u32> = artifacts
            .mutations()
            .iter()
            .map(|entry| entry.mutation_id)
            .collect();
        assert_eq!(log_ids, Vec::from([0, 2]));

        // Provenance reflects the stable ids: the retained square method
        // still carries Session { 0 } and the new triangle method carries
        // Session { 2 }, not Session { 1 }.
        let square = &artifacts.sem.methods[first.inserted_method.0 as usize];
        assert!(matches!(
            square.origin,
            sem::MethodOrigin::Session { sequence: 0 }
        ));
        let triangle = &artifacts.sem.methods[next.inserted_method.0 as usize];
        assert!(matches!(
            triangle.origin,
            sem::MethodOrigin::Session { sequence: 2 }
        ));
    }

    #[test]
    fn rewind_to_dropped_id_returns_target_out_of_range() {
        // A dropped id is permanently retired; rewinding to it after the
        // drop must surface TargetOutOfRange rather than silently treating
        // the absent id as "rewind everything <= id".
        let mut artifacts = mutation_fixture();
        let area = generic_id(&artifacts, "area");

        let first = artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_SQUARE_METHOD.into(),
            })
            .expect("first mutation should succeed");
        let second = artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_TRIANGLE_METHOD.into(),
            })
            .expect("second mutation should succeed");
        artifacts
            .rewind_to(Some(first.mutation_id))
            .expect("rewind should succeed");

        // id `second.mutation_id` was dropped by the rewind above. The
        // next rewind_to that target must fail.
        let err = artifacts
            .rewind_to(Some(second.mutation_id))
            .expect_err("rewinding to a dropped id should fail");
        assert!(
            matches!(err, RewindError::TargetOutOfRange { target, .. } if target == second.mutation_id)
        );
        // State unchanged after the failed rewind.
        assert_eq!(artifacts.mutations().len(), 1);
        assert_eq!(artifacts.mutations()[0].mutation_id, first.mutation_id);
    }

    #[test]
    fn rewind_shrinks_mutation_log_eop_records() {
        let mut artifacts = mutation_fixture();
        let area = generic_id(&artifacts, "area");

        artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_SQUARE_METHOD.into(),
            })
            .expect("first mutation should succeed");
        artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_TRIANGLE_METHOD.into(),
            })
            .expect("second mutation should succeed");
        assert_eq!(
            artifacts
                .environment_objects()
                .iter()
                .filter(|object| object.id.as_str().starts_with("mutation:"))
                .count(),
            2
        );

        artifacts.rewind_to(Some(0)).expect("rewind should succeed");

        let after_objects = artifacts.environment_objects();
        let mutation_ids: Vec<_> = after_objects
            .iter()
            .filter_map(|object| {
                let id = object.id.as_str();
                id.starts_with("mutation:").then(|| id.to_string())
            })
            .collect();
        assert_eq!(mutation_ids, Vec::from(["mutation:m0".to_string()]));
    }

    fn build_source_file(text: &str) -> SourceFile {
        SourceFile::named("mutation_fixture.loko", text)
    }

    fn fixture_with_two_session_mutations() -> SessionArtifacts {
        let mut artifacts = mutation_fixture();
        let area = generic_id(&artifacts, "area");
        artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_SQUARE_METHOD.into(),
            })
            .expect("first mutation should succeed");
        artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_TRIANGLE_METHOD.into(),
            })
            .expect("second mutation should succeed");
        artifacts
    }

    #[test]
    fn replay_against_pristine_base_reproduces_full_log_and_dispatch() {
        let artifacts = fixture_with_two_session_mutations();
        let pristine = build_source_file(&artifacts.pristine_source);
        let snapshot_log = artifacts.mutations().to_vec();
        let snapshot_sem_methods = artifacts.sem.methods.clone();

        let outcome = artifacts.replay_against(&pristine);

        assert!(
            outcome.failed.is_none(),
            "replay against pristine should succeed: {:?}",
            outcome.failed
        );
        assert_eq!(outcome.applied.len(), 2);
        assert_eq!(outcome.applied[0].mutation_id, 0);
        assert_eq!(outcome.applied[1].mutation_id, 1);

        // Original session is unchanged.
        assert_eq!(artifacts.mutations(), snapshot_log.as_slice());
        assert_eq!(artifacts.sem.methods, snapshot_sem_methods);

        // Ephemeral artifacts reproduce dispatch behavior.
        let square = run_scalar(&outcome.artifacts, "score_square");
        assert!(matches!(square, Ok(FunctionValue::Integer(49))));
        let triangle = run_scalar(&outcome.artifacts, "score_triangle");
        assert!(matches!(triangle, Ok(FunctionValue::Integer(81))));
    }

    #[test]
    fn replay_against_harmless_base_edit_still_replays_full_log() {
        let artifacts = fixture_with_two_session_mutations();
        // Add an unrelated function and a comment to the base. Neither
        // affects the mutations' targets.
        let edited = format!(
            "{}\n\n// harmless\n\ndefine function answer()\n  42\nend;\n",
            artifacts.pristine_source
        );
        let new_base = build_source_file(&edited);

        let outcome = artifacts.replay_against(&new_base);

        assert!(
            outcome.failed.is_none(),
            "harmless edit should still replay: {:?}",
            outcome.failed
        );
        assert_eq!(outcome.applied.len(), 2);

        let square = run_scalar(&outcome.artifacts, "score_square");
        assert!(matches!(square, Ok(FunctionValue::Integer(49))));
        let triangle = run_scalar(&outcome.artifacts, "score_triangle");
        assert!(matches!(triangle, Ok(FunctionValue::Integer(81))));
        // The unrelated function is reachable on the ephemeral artifacts.
        let answer = run_scalar(&outcome.artifacts, "answer");
        assert!(matches!(answer, Ok(FunctionValue::Integer(42))));
    }

    #[test]
    fn replay_against_base_without_target_generic_fails_at_first_mutation() {
        let artifacts = fixture_with_two_session_mutations();
        // New base has no classes and no generics; sem.generics is empty,
        // so the original GenericFunctionId(0) is out of range. (Leaving
        // the fixture's classes in would synthesize setter generics that
        // occupy id 0, turning this into the GenericNameMismatch case
        // covered by the swap-name test below.)
        let new_text = "\
define function noop()
  0
end;
";
        let new_base = build_source_file(new_text);
        let snapshot_log = artifacts.mutations().to_vec();

        let outcome = artifacts.replay_against(&new_base);

        let failure = outcome
            .failed
            .expect("removed-generic base should surface a replay failure");
        // First mutation targets the area generic; with area gone, the
        // first entry fails. mutation_id 0, log_index 0, cause is
        // UnknownGeneric because the original GenericFunctionId no
        // longer resolves in the new base's sem module.
        assert_eq!(failure.mutation_id, 0);
        assert_eq!(failure.log_index, 0);
        assert!(
            matches!(failure.cause, MutationError::UnknownGeneric { .. }),
            "expected UnknownGeneric, got {:?}",
            failure.cause
        );
        assert!(outcome.applied.is_empty());

        // Original session is unchanged after the failed replay.
        assert_eq!(artifacts.mutations(), snapshot_log.as_slice());
    }

    #[test]
    fn replay_against_base_with_reordered_generic_id_fails_with_name_mismatch() {
        // Construct a new base where the area generic is still present but
        // a fresh generic `score` is declared before it, pushing area to
        // id 1. The replay still carries the original
        // GenericFunctionId(0) on each entry; in the new sem id 0 is now
        // `score`, while the method source resolves `area` to id 1.
        // 1 != 0 triggers GenericNameMismatch, which is the right
        // failure to surface: a silent application onto whatever generic
        // happens to sit at id 0 would be a worse outcome than a
        // structured "you reordered generics; replay can't tell whether
        // you meant the old id or the same-named one" error.
        let artifacts = fixture_with_two_session_mutations();
        let new_text = "\
define class <circle> (<object>)
  slot radius :: <integer>;
end;

define class <square> (<object>)
  slot side :: <integer>;
end;

define class <triangle> (<object>)
  slot base :: <integer>;
end;

define generic score(x);

define generic area(x);

define method score(c :: <circle>)
  c.radius
end;

define method area(c :: <circle>)
  c.radius * c.radius
end;
";
        let new_base = build_source_file(new_text);

        let outcome = artifacts.replay_against(&new_base);

        let failure = outcome
            .failed
            .expect("reordered-generic base should surface a replay failure");
        assert_eq!(failure.mutation_id, 0);
        assert_eq!(failure.log_index, 0);
        match &failure.cause {
            MutationError::GenericNameMismatch {
                expected, actual, ..
            } => {
                assert_eq!(expected, "score");
                assert_eq!(actual, "area");
            }
            other => panic!("expected GenericNameMismatch, got {other:?}"),
        }
        assert!(outcome.applied.is_empty());
    }

    #[test]
    fn replay_against_base_without_specializer_class_fails_at_second_mutation() {
        let artifacts = fixture_with_two_session_mutations();
        // New base removes the <triangle> class. The first mutation
        // (area on <square>) still applies; the second (area on
        // <triangle>) does not.
        let new_text = "\
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

define function score_circle()
  area(make(<circle>, radius: 10))
end;

define function score_square()
  area(make(<square>, side: 7))
end;
";
        let new_base = build_source_file(new_text);
        let snapshot_log = artifacts.mutations().to_vec();

        let outcome = artifacts.replay_against(&new_base);

        let failure = outcome
            .failed
            .expect("removed-class base should surface a replay failure");
        assert_eq!(failure.mutation_id, 1);
        assert_eq!(failure.log_index, 1);
        // Cause is SemanticFailed and carries an UnresolvedClass
        // diagnostic naming the missing <triangle> class. The error
        // taxonomy is the valuable part of replay: callers can
        // distinguish "this generic is gone" from "this class is gone"
        // without parsing diagnostic prose.
        match &failure.cause {
            MutationError::SemanticFailed { diagnostics } => {
                assert!(
                    diagnostics
                        .iter()
                        .any(|d| d.code == DiagnosticCode::UnresolvedClass),
                    "expected UnresolvedClass diagnostic, got {:?}",
                    diagnostics.iter().map(|d| d.code).collect::<Vec<_>>()
                );
            }
            other => panic!("expected SemanticFailed, got {other:?}"),
        }
        // The first mutation succeeded against the edited base.
        assert_eq!(outcome.applied.len(), 1);
        assert_eq!(outcome.applied[0].mutation_id, 0);

        // The ephemeral session has the first mutation applied; dispatch
        // for square works there even though the original session is
        // unchanged.
        let square = run_scalar(&outcome.artifacts, "score_square");
        assert!(matches!(square, Ok(FunctionValue::Integer(49))));

        assert_eq!(artifacts.mutations(), snapshot_log.as_slice());
    }

    #[test]
    fn replay_against_base_that_already_defines_square_method_fails_with_duplicate() {
        // The session's mutation log adds area(<square>) at id 0. If the
        // replay's new base already defines area(<square>) as a source
        // method, the replayed entry collides with the pre-existing
        // method on the same specializer vector. The apply path surfaces
        // DuplicateSpecializer, and replay must propagate that cause
        // unchanged - the error taxonomy is the load-bearing part of
        // replay, and DuplicateSpecializer is listed in eop_mutation.md
        // as a concrete replay failure mode.
        let artifacts = fixture_with_two_session_mutations();
        let new_text = "\
define class <circle> (<object>)
  slot radius :: <integer>;
end;

define class <square> (<object>)
  slot side :: <integer>;
end;

define class <triangle> (<object>)
  slot base :: <integer>;
end;

define generic area(x);

define method area(c :: <circle>)
  c.radius * c.radius
end;

define method area(s :: <square>)
  s.side + 1
end;
";
        let new_base = build_source_file(new_text);
        let snapshot_log = artifacts.mutations().to_vec();

        let outcome = artifacts.replay_against(&new_base);

        let failure = outcome
            .failed
            .expect("duplicate-square base should surface a replay failure");
        assert_eq!(failure.mutation_id, 0);
        assert_eq!(failure.log_index, 0);
        assert!(
            matches!(failure.cause, MutationError::DuplicateSpecializer { .. }),
            "expected DuplicateSpecializer, got {:?}",
            failure.cause
        );
        assert!(outcome.applied.is_empty());
        assert_eq!(artifacts.mutations(), snapshot_log.as_slice());
    }

    #[test]
    fn replay_preserves_original_mutation_ids_and_finalizes_next_id_counter() {
        // Apply two mutations, rewind to the first, apply a third. The
        // live log is [id 0, id 2] with a gap at id 1 (the rewind dropped
        // id 1; the post-rewind apply got id 2 from the preserved
        // counter).
        let mut artifacts = mutation_fixture();
        let area = generic_id(&artifacts, "area");
        artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_SQUARE_METHOD.into(),
            })
            .expect("first mutation should succeed");
        artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_TRIANGLE_METHOD.into(),
            })
            .expect("second mutation should succeed");
        artifacts.rewind_to(Some(0)).expect("rewind should succeed");
        let next = artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_TRIANGLE_METHOD.into(),
            })
            .expect("post-rewind mutation should succeed");
        assert_eq!(next.mutation_id, 2);
        let log_ids: Vec<u32> = artifacts
            .mutations()
            .iter()
            .map(|entry| entry.mutation_id)
            .collect();
        assert_eq!(log_ids, Vec::from([0u32, 2u32]));

        // Replay against the pristine base. The replayed log should
        // preserve the same ids (0 and 2), not reassign sequential ids.
        let pristine = build_source_file(&artifacts.pristine_source);
        let outcome = artifacts.replay_against(&pristine);

        assert!(
            outcome.failed.is_none(),
            "pristine replay should succeed: {:?}",
            outcome.failed
        );
        let replayed_ids: Vec<u32> = outcome
            .applied
            .iter()
            .map(|entry| entry.mutation_id)
            .collect();
        assert_eq!(replayed_ids, Vec::from([0u32, 2u32]));
        let ephemeral_ids: Vec<u32> = outcome
            .artifacts
            .mutations()
            .iter()
            .map(|entry| entry.mutation_id)
            .collect();
        assert_eq!(ephemeral_ids, Vec::from([0u32, 2u32]));

        // The ephemeral session's next-id counter is finalized to
        // max(applied_ids) + 1 = 3, not retained_count = 2. A subsequent
        // apply on the adopted artifacts would therefore get id 3; this
        // test asserts the counter directly to avoid having to invent a
        // fresh mutation that does not collide with the replayed log
        // (all three classes' area methods are already present, so any
        // ordinary apply on the area generic would be rejected).
        assert_eq!(outcome.artifacts.next_mutation_id, 3);
    }

    #[test]
    fn apply_add_method_rejects_parse_failure_and_preserves_state() {
        let mut artifacts = mutation_fixture();
        let area = generic_id(&artifacts, "area");
        let before_methods = artifacts.sem.methods.clone();
        let before_keys = artifact_keys(&artifacts);
        let before_circle = run_scalar(&artifacts, "score_circle");

        let err = artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: "define method !!! garbage".into(),
            })
            .expect_err("garbage source should be rejected");
        assert!(matches!(err, MutationError::ParseFailed { .. }));

        assert_eq!(artifacts.sem.methods, before_methods);
        assert_eq!(artifact_keys(&artifacts), before_keys);
        let after_circle = run_scalar(&artifacts, "score_circle");
        assert!(matches!(before_circle, Ok(FunctionValue::Integer(100))));
        assert!(matches!(after_circle, Ok(FunctionValue::Integer(100))));
        assert_eq!(artifacts.mutations().len(), 0);
    }

    #[test]
    fn apply_add_method_rejects_semantic_failure_and_preserves_state() {
        let mut artifacts = mutation_fixture();
        let area = generic_id(&artifacts, "area");
        let before_methods = artifacts.sem.methods.clone();
        let before_keys = artifact_keys(&artifacts);

        let err = artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: "define method area(x :: <does-not-exist>)\n  42\nend;".into(),
            })
            .expect_err("unknown class should be rejected");
        assert!(matches!(err, MutationError::SemanticFailed { .. }));

        assert_eq!(artifacts.sem.methods, before_methods);
        assert_eq!(artifact_keys(&artifacts), before_keys);
        assert_eq!(artifacts.mutations().len(), 0);
    }

    #[test]
    fn apply_add_method_rejects_unknown_generic_id() {
        let mut artifacts = mutation_fixture();
        let missing = sem::GenericFunctionId(u32::MAX);
        let before_methods = artifacts.sem.methods.clone();

        let err = artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: missing,
                method_source: MUTATION_SQUARE_METHOD.into(),
            })
            .expect_err("unknown generic should be rejected");
        assert!(matches!(err, MutationError::UnknownGeneric { generic } if generic == missing));
        assert_eq!(artifacts.sem.methods, before_methods);
        assert_eq!(artifacts.mutations().len(), 0);
    }

    #[test]
    fn apply_add_method_rejects_duplicate_specializer() {
        let mut artifacts = mutation_fixture();
        let area = generic_id(&artifacts, "area");
        let before_methods = artifacts.sem.methods.clone();
        let before_keys = artifact_keys(&artifacts);

        // Duplicate of the existing area(<circle>) method.
        let err = artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: "define method area(c :: <circle>)\n  c.radius\nend;".into(),
            })
            .expect_err("duplicate specializer should be rejected");
        assert!(matches!(err, MutationError::DuplicateSpecializer { .. }));

        assert_eq!(artifacts.sem.methods, before_methods);
        assert_eq!(artifact_keys(&artifacts), before_keys);
        assert_eq!(artifacts.mutations().len(), 0);
    }

    #[test]
    fn apply_add_method_affected_artifacts_include_new_method_and_affected_generic() {
        let mut artifacts = mutation_fixture();
        let area = generic_id(&artifacts, "area");
        let area_definition = artifacts.sem.generics[area.0 as usize].definition;

        let outcome = artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_SQUARE_METHOD.into(),
            })
            .expect("mutation should succeed");

        let new_method_definition =
            artifacts.sem.methods[outcome.inserted_method.0 as usize].definition;

        assert!(
            outcome
                .affected_artifacts
                .contains(&QueryArtifact::MethodLookup(new_method_definition))
        );
        assert!(
            outcome
                .affected_artifacts
                .contains(&QueryArtifact::MethodLowering(new_method_definition))
        );
        assert!(
            outcome
                .affected_artifacts
                .contains(&QueryArtifact::MethodTape(new_method_definition))
        );
        assert!(
            outcome
                .affected_artifacts
                .contains(&QueryArtifact::GenericDispatcher(area_definition))
        );
        assert!(
            outcome
                .affected_artifacts
                .contains(&QueryArtifact::LoweredModule)
        );
        assert!(
            outcome
                .affected_artifacts
                .contains(&QueryArtifact::TapeProgram)
        );
        assert!(
            outcome
                .affected_artifacts
                .contains(&QueryArtifact::RuntimeResult)
        );

        // The grammar environment is untouched by an add-method mutation,
        // so EnvironmentResolution and GrammarStack must not be reported.
        assert!(
            !outcome
                .affected_artifacts
                .contains(&QueryArtifact::EnvironmentResolution)
        );
        assert!(
            !outcome
                .affected_artifacts
                .contains(&QueryArtifact::GrammarStack)
        );
    }

    #[test]
    fn apply_add_method_affected_artifacts_exclude_unrelated_method_artifacts() {
        let mut artifacts = mutation_fixture();
        let area = generic_id(&artifacts, "area");

        // Capture pre-mutation per-method definitions for the original
        // source-origin method and the synthesized slot setters.
        let other_method_definitions: Vec<DefinitionId> = artifacts
            .sem
            .methods
            .iter()
            .map(|method| method.definition)
            .collect();

        let outcome = artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_SQUARE_METHOD.into(),
            })
            .expect("mutation should succeed");

        for definition in other_method_definitions {
            assert!(
                !outcome
                    .affected_artifacts
                    .contains(&QueryArtifact::MethodLookup(definition)),
                "unrelated method {definition:?} should not be in affected_artifacts MethodLookup"
            );
            assert!(
                !outcome
                    .affected_artifacts
                    .contains(&QueryArtifact::MethodLowering(definition)),
                "unrelated method {definition:?} should not be in affected_artifacts MethodLowering"
            );
            assert!(
                !outcome
                    .affected_artifacts
                    .contains(&QueryArtifact::MethodTape(definition)),
                "unrelated method {definition:?} should not be in affected_artifacts MethodTape"
            );
        }
    }

    #[test]
    fn apply_add_method_leaves_unrelated_lowered_outputs_byte_equal() {
        let mut artifacts = mutation_fixture();
        let area = generic_id(&artifacts, "area");
        let original_function_count = artifacts.sem.functions.len();
        let original_ast_method_count = artifacts.ast.methods.len();
        let unrelated_function_id = artifacts
            .sem
            .functions
            .iter()
            .position(|function| function.name == "unrelated_constant")
            .map(|index| index as u32)
            .expect("fixture must contain an unrelated function");

        let before_lowered = artifacts
            .lowered
            .as_ref()
            .expect("baseline should lower cleanly")
            .clone();

        let _ = artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: MUTATION_SQUARE_METHOD.into(),
            })
            .expect("mutation should succeed");

        let after_lowered = artifacts
            .lowered
            .as_ref()
            .expect("post-mutation should lower cleanly");

        // A function that never invokes the affected generic stays
        // byte-equal at the same FunctionId. score_circle / score_square
        // call area() and so legitimately re-link to a shifted dispatcher
        // id; they are not in the unrelated set for this mutation.
        let before_unrelated = &before_lowered.functions[unrelated_function_id as usize];
        let after_unrelated = &after_lowered.functions[unrelated_function_id as usize];
        assert_eq!(
            before_unrelated, after_unrelated,
            "an unrelated user function must be byte-equal across an add-method mutation"
        );

        // Original source-origin method bodies sit at indices
        // [F .. F + M_orig_ast]. They are not touched by adding a sibling
        // method on the same generic, so they must also be byte-equal.
        let original_methods_end = original_function_count + original_ast_method_count;
        assert_eq!(
            &before_lowered.functions[original_function_count..original_methods_end],
            &after_lowered.functions[original_function_count..original_methods_end],
            "unrelated source-origin methods must be byte-equal across an add-method mutation"
        );
    }
}
