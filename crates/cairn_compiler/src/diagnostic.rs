//! Structured diagnostics for Cairn compiler stages.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::syntax::Span;

/// Diagnostic severity.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Severity {
    /// The compiler could not produce a valid artifact for the affected input.
    Error,
}

/// Stable diagnostic identity used by tests, tools, EOP records, and future
/// localized renderers.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticCode {
    /// Diagnostic text has not been assigned a more specific code yet.
    Uncategorized,
    /// A parser expected an expression.
    SyntaxExpectedExpression,
    /// A parser expected a specific token or syntactic form.
    SyntaxExpectedToken,
    /// An integer literal does not fit in the supported integer range.
    AstIntegerLiteralOutOfRange,
    /// A slot option is missing its keyword value.
    AstSlotOptionRequiresKeyword,
    /// A slot `init-value:` option is missing its expression.
    AstSlotInitValueRequiresExpression,
    /// A slot option is not supported.
    AstUnsupportedSlotOption,
    /// A method declaration has the wrong first-slice specializer shape.
    AstInvalidMethodSpecializer,
    /// `instance?` has the wrong syntactic argument shape.
    AstInvalidInstanceOf,
    /// `as` has the wrong syntactic argument shape.
    AstInvalidCast,
    /// `signal` has the wrong syntactic argument shape.
    AstInvalidSignal,
    /// A source definition duplicates an existing definition.
    DuplicateDefinition,
    /// A class inherits from an unknown superclass.
    UnresolvedSuperclass,
    /// A class tries to inherit from a primitive type.
    InvalidPrimitiveSuperclass,
    /// A class tries to inherit from a host-backed class.
    InvalidHostBackedSuperclass,
    /// A class inheritance cycle was detected.
    ClassInheritanceCycle,
    /// A finalized class has a duplicate effective slot name.
    DuplicateEffectiveSlot,
    /// A finalized class has a duplicate effective init keyword.
    DuplicateEffectiveInitKeyword,
    /// A class declares duplicate direct slot init keywords.
    DuplicateSlotInitKeyword,
    /// A finalized class has a duplicate effective accessor.
    DuplicateEffectiveAccessor,
    /// A host registry declares a class that conflicts with a source class.
    HostClassConflict,
    /// A class, class-object type, or slot type could not be resolved.
    UnresolvedClass,
    /// A generic function could not be resolved.
    UnresolvedGeneric,
    /// A function or generic call target could not be resolved.
    UnresolvedFunctionOrGeneric,
    /// A local binding could not be resolved.
    UnresolvedLocal,
    /// A parameter, local, or restart binding duplicates an existing local.
    DuplicateLocal,
    /// A callable value is ambiguous.
    AmbiguousCallable,
    /// A function or callable was called with the wrong number of arguments.
    FunctionArityMismatch,
    /// A direct function argument has the wrong type.
    FunctionArgumentTypeMismatch,
    /// A direct function body does not satisfy its result annotation.
    FunctionReturnTypeMismatch,
    /// A generic method specializer does not satisfy the generic contract.
    GenericMethodSpecializerTypeMismatch,
    /// A method result annotation does not satisfy the generic contract.
    GenericMethodResultTypeMismatch,
    /// A method body does not satisfy its result annotation.
    MethodReturnTypeMismatch,
    /// A generic call argument does not satisfy the generic contract.
    GenericArgumentTypeMismatch,
    /// `if` condition is not boolean.
    IfConditionTypeMismatch,
    /// `if` branch result kinds differ.
    IfBranchTypeMismatch,
    /// Arithmetic operands are not integers.
    ArithmeticOperandTypeMismatch,
    /// Comparison operands are not integers.
    ComparisonOperandTypeMismatch,
    /// `apply` is missing its callable target.
    ApplyArityMismatch,
    /// `apply` cannot use slot accessors yet.
    ApplyAccessorUnsupported,
    /// `apply` target is not a known callable.
    ApplyTargetTypeMismatch,
    /// A signal expression is not a known condition object.
    SignalConditionTypeMismatch,
    /// A signal has no active restart.
    SignalRequiresRestart,
    /// A signal condition is not handled by the active restart.
    SignalRestartMismatch,
    /// A handle body and restart handler have incompatible result kinds.
    HandleRestartResultMismatch,
    /// `class-of` received an invalid value.
    ClassOfTypeMismatch,
    /// `instance?` received an invalid value.
    InstanceOfTypeMismatch,
    /// An `instance?` test is statically impossible.
    InstanceOfImpossible,
    /// `as` received an invalid value.
    CastTypeMismatch,
    /// An `as` cast is statically impossible.
    CastImpossible,
    /// `make` was called with a non-class object argument.
    MakeClassArgumentTypeMismatch,
    /// A host-backed class was passed to `make`.
    HostBackedMake,
    /// A construction keyword appears more than once.
    DuplicateInitKeyword,
    /// A construction keyword is not accepted by the class.
    UnknownInitKeyword,
    /// A slot initializer has the wrong type.
    SlotInitializerTypeMismatch,
    /// A slot default `init-value:` has the wrong type.
    SlotDefaultTypeMismatch,
    /// A required init keyword is missing.
    MissingRequiredInitKeyword,
    /// A non-defaulted slot is missing an initializer.
    MissingSlotInitializer,
    /// A slot read targets an unknown slot.
    UnknownSlot,
    /// A slot read receiver is not an object.
    SlotReceiverTypeMismatch,
    /// Lowered IR failed an internal semantic invariant.
    LowerInvalidSemantic,
    /// Lowering stopped because earlier diagnostics exist.
    LowerBlockedByDiagnostics,
    /// Tape emission failed an internal lowered-IR invariant.
    TapeInvalidLowered,
    /// Tape emission stopped because earlier diagnostics exist.
    TapeBlockedByDiagnostics,
    /// The execution_tape builder or verifier rejected the emitted program.
    TapeBuildError,
    /// No zero-argument `main` function is available.
    RuntimeMissingMain,
    /// A requested function is missing.
    RuntimeMissingFunction,
    /// `main` has parameters.
    RuntimeMainHasParameters,
    /// Runtime execution trapped.
    RuntimeTrap,
    /// `main` returned a non-integer result.
    RuntimeMainReturnedNonI64,
    /// A requested function returned a non-integer result.
    RuntimeFunctionReturnedNonI64,
    /// A session source file id is unknown.
    SessionUnknownSource,
}

impl DiagnosticCode {
    /// All stable diagnostic codes known to this compiler build.
    pub const ALL: &'static [Self] = &[
        Self::Uncategorized,
        Self::SyntaxExpectedExpression,
        Self::SyntaxExpectedToken,
        Self::AstIntegerLiteralOutOfRange,
        Self::AstSlotOptionRequiresKeyword,
        Self::AstSlotInitValueRequiresExpression,
        Self::AstUnsupportedSlotOption,
        Self::AstInvalidMethodSpecializer,
        Self::AstInvalidInstanceOf,
        Self::AstInvalidCast,
        Self::AstInvalidSignal,
        Self::DuplicateDefinition,
        Self::UnresolvedSuperclass,
        Self::InvalidPrimitiveSuperclass,
        Self::InvalidHostBackedSuperclass,
        Self::ClassInheritanceCycle,
        Self::DuplicateEffectiveSlot,
        Self::DuplicateEffectiveInitKeyword,
        Self::DuplicateSlotInitKeyword,
        Self::DuplicateEffectiveAccessor,
        Self::HostClassConflict,
        Self::UnresolvedClass,
        Self::UnresolvedGeneric,
        Self::UnresolvedFunctionOrGeneric,
        Self::UnresolvedLocal,
        Self::DuplicateLocal,
        Self::AmbiguousCallable,
        Self::FunctionArityMismatch,
        Self::FunctionArgumentTypeMismatch,
        Self::FunctionReturnTypeMismatch,
        Self::GenericMethodSpecializerTypeMismatch,
        Self::GenericMethodResultTypeMismatch,
        Self::MethodReturnTypeMismatch,
        Self::GenericArgumentTypeMismatch,
        Self::IfConditionTypeMismatch,
        Self::IfBranchTypeMismatch,
        Self::ArithmeticOperandTypeMismatch,
        Self::ComparisonOperandTypeMismatch,
        Self::ApplyArityMismatch,
        Self::ApplyAccessorUnsupported,
        Self::ApplyTargetTypeMismatch,
        Self::SignalConditionTypeMismatch,
        Self::SignalRequiresRestart,
        Self::SignalRestartMismatch,
        Self::HandleRestartResultMismatch,
        Self::ClassOfTypeMismatch,
        Self::InstanceOfTypeMismatch,
        Self::InstanceOfImpossible,
        Self::CastTypeMismatch,
        Self::CastImpossible,
        Self::MakeClassArgumentTypeMismatch,
        Self::HostBackedMake,
        Self::DuplicateInitKeyword,
        Self::UnknownInitKeyword,
        Self::SlotInitializerTypeMismatch,
        Self::SlotDefaultTypeMismatch,
        Self::MissingRequiredInitKeyword,
        Self::MissingSlotInitializer,
        Self::UnknownSlot,
        Self::SlotReceiverTypeMismatch,
        Self::LowerInvalidSemantic,
        Self::LowerBlockedByDiagnostics,
        Self::TapeInvalidLowered,
        Self::TapeBlockedByDiagnostics,
        Self::TapeBuildError,
        Self::RuntimeMissingMain,
        Self::RuntimeMissingFunction,
        Self::RuntimeMainHasParameters,
        Self::RuntimeTrap,
        Self::RuntimeMainReturnedNonI64,
        Self::RuntimeFunctionReturnedNonI64,
        Self::SessionUnknownSource,
    ];

    /// Stable kebab-case text for this diagnostic code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Uncategorized => "uncategorized",
            Self::SyntaxExpectedExpression => "syntax.expected-expression",
            Self::SyntaxExpectedToken => "syntax.expected-token",
            Self::AstIntegerLiteralOutOfRange => "ast.integer-literal-out-of-range",
            Self::AstSlotOptionRequiresKeyword => "ast.slot-option-requires-keyword",
            Self::AstSlotInitValueRequiresExpression => "ast.slot-init-value-requires-expression",
            Self::AstUnsupportedSlotOption => "ast.unsupported-slot-option",
            Self::AstInvalidMethodSpecializer => "ast.invalid-method-specializer",
            Self::AstInvalidInstanceOf => "ast.invalid-instance-of",
            Self::AstInvalidCast => "ast.invalid-cast",
            Self::AstInvalidSignal => "ast.invalid-signal",
            Self::DuplicateDefinition => "sem.duplicate-definition",
            Self::UnresolvedSuperclass => "sem.unresolved-superclass",
            Self::InvalidPrimitiveSuperclass => "sem.invalid-primitive-superclass",
            Self::InvalidHostBackedSuperclass => "sem.invalid-host-backed-superclass",
            Self::ClassInheritanceCycle => "sem.class-inheritance-cycle",
            Self::DuplicateEffectiveSlot => "sem.duplicate-effective-slot",
            Self::DuplicateEffectiveInitKeyword => "sem.duplicate-effective-init-keyword",
            Self::DuplicateSlotInitKeyword => "sem.duplicate-slot-init-keyword",
            Self::DuplicateEffectiveAccessor => "sem.duplicate-effective-accessor",
            Self::HostClassConflict => "sem.host-class-conflict",
            Self::UnresolvedClass => "sem.unresolved-class",
            Self::UnresolvedGeneric => "sem.unresolved-generic",
            Self::UnresolvedFunctionOrGeneric => "sem.unresolved-function-or-generic",
            Self::UnresolvedLocal => "sem.unresolved-local",
            Self::DuplicateLocal => "sem.duplicate-local",
            Self::AmbiguousCallable => "sem.ambiguous-callable",
            Self::FunctionArityMismatch => "sem.function-arity-mismatch",
            Self::FunctionArgumentTypeMismatch => "sem.function-argument-type-mismatch",
            Self::FunctionReturnTypeMismatch => "sem.function-return-type-mismatch",
            Self::GenericMethodSpecializerTypeMismatch => {
                "sem.generic-method-specializer-type-mismatch"
            }
            Self::GenericMethodResultTypeMismatch => "sem.generic-method-result-type-mismatch",
            Self::MethodReturnTypeMismatch => "sem.method-return-type-mismatch",
            Self::GenericArgumentTypeMismatch => "sem.generic-argument-type-mismatch",
            Self::IfConditionTypeMismatch => "sem.if-condition-type-mismatch",
            Self::IfBranchTypeMismatch => "sem.if-branch-type-mismatch",
            Self::ArithmeticOperandTypeMismatch => "sem.arithmetic-operand-type-mismatch",
            Self::ComparisonOperandTypeMismatch => "sem.comparison-operand-type-mismatch",
            Self::ApplyArityMismatch => "sem.apply-arity-mismatch",
            Self::ApplyAccessorUnsupported => "sem.apply-accessor-unsupported",
            Self::ApplyTargetTypeMismatch => "sem.apply-target-type-mismatch",
            Self::SignalConditionTypeMismatch => "sem.signal-condition-type-mismatch",
            Self::SignalRequiresRestart => "sem.signal-requires-restart",
            Self::SignalRestartMismatch => "sem.signal-restart-mismatch",
            Self::HandleRestartResultMismatch => "sem.handle-restart-result-mismatch",
            Self::ClassOfTypeMismatch => "sem.class-of-type-mismatch",
            Self::InstanceOfTypeMismatch => "sem.instance-of-type-mismatch",
            Self::InstanceOfImpossible => "sem.instance-of-impossible",
            Self::CastTypeMismatch => "sem.cast-type-mismatch",
            Self::CastImpossible => "sem.cast-impossible",
            Self::MakeClassArgumentTypeMismatch => "sem.make-class-argument-type-mismatch",
            Self::HostBackedMake => "sem.host-backed-make",
            Self::DuplicateInitKeyword => "sem.duplicate-init-keyword",
            Self::UnknownInitKeyword => "sem.unknown-init-keyword",
            Self::SlotInitializerTypeMismatch => "sem.slot-initializer-type-mismatch",
            Self::SlotDefaultTypeMismatch => "sem.slot-default-type-mismatch",
            Self::MissingRequiredInitKeyword => "sem.missing-required-init-keyword",
            Self::MissingSlotInitializer => "sem.missing-slot-initializer",
            Self::UnknownSlot => "sem.unknown-slot",
            Self::SlotReceiverTypeMismatch => "sem.slot-receiver-type-mismatch",
            Self::LowerInvalidSemantic => "lower.invalid-semantic",
            Self::LowerBlockedByDiagnostics => "lower.blocked-by-diagnostics",
            Self::TapeInvalidLowered => "tape.invalid-lowered",
            Self::TapeBlockedByDiagnostics => "tape.blocked-by-diagnostics",
            Self::TapeBuildError => "tape.build-error",
            Self::RuntimeMissingMain => "runtime.missing-main",
            Self::RuntimeMissingFunction => "runtime.missing-function",
            Self::RuntimeMainHasParameters => "runtime.main-has-parameters",
            Self::RuntimeTrap => "runtime.trap",
            Self::RuntimeMainReturnedNonI64 => "runtime.main-returned-non-i64",
            Self::RuntimeFunctionReturnedNonI64 => "runtime.function-returned-non-i64",
            Self::SessionUnknownSource => "session.unknown-source",
        }
    }

    /// Parses a stable diagnostic code string.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "uncategorized" => Self::Uncategorized,
            "syntax.expected-expression" => Self::SyntaxExpectedExpression,
            "syntax.expected-token" => Self::SyntaxExpectedToken,
            "ast.integer-literal-out-of-range" => Self::AstIntegerLiteralOutOfRange,
            "ast.slot-option-requires-keyword" => Self::AstSlotOptionRequiresKeyword,
            "ast.slot-init-value-requires-expression" => Self::AstSlotInitValueRequiresExpression,
            "ast.unsupported-slot-option" => Self::AstUnsupportedSlotOption,
            "ast.invalid-method-specializer" => Self::AstInvalidMethodSpecializer,
            "ast.invalid-instance-of" => Self::AstInvalidInstanceOf,
            "ast.invalid-cast" => Self::AstInvalidCast,
            "ast.invalid-signal" => Self::AstInvalidSignal,
            "sem.duplicate-definition" => Self::DuplicateDefinition,
            "sem.unresolved-superclass" => Self::UnresolvedSuperclass,
            "sem.invalid-primitive-superclass" => Self::InvalidPrimitiveSuperclass,
            "sem.invalid-host-backed-superclass" => Self::InvalidHostBackedSuperclass,
            "sem.class-inheritance-cycle" => Self::ClassInheritanceCycle,
            "sem.duplicate-effective-slot" => Self::DuplicateEffectiveSlot,
            "sem.duplicate-effective-init-keyword" => Self::DuplicateEffectiveInitKeyword,
            "sem.duplicate-slot-init-keyword" => Self::DuplicateSlotInitKeyword,
            "sem.duplicate-effective-accessor" => Self::DuplicateEffectiveAccessor,
            "sem.host-class-conflict" => Self::HostClassConflict,
            "sem.unresolved-class" => Self::UnresolvedClass,
            "sem.unresolved-generic" => Self::UnresolvedGeneric,
            "sem.unresolved-function-or-generic" => Self::UnresolvedFunctionOrGeneric,
            "sem.unresolved-local" => Self::UnresolvedLocal,
            "sem.duplicate-local" => Self::DuplicateLocal,
            "sem.ambiguous-callable" => Self::AmbiguousCallable,
            "sem.function-arity-mismatch" => Self::FunctionArityMismatch,
            "sem.function-argument-type-mismatch" => Self::FunctionArgumentTypeMismatch,
            "sem.function-return-type-mismatch" => Self::FunctionReturnTypeMismatch,
            "sem.generic-method-specializer-type-mismatch" => {
                Self::GenericMethodSpecializerTypeMismatch
            }
            "sem.generic-method-result-type-mismatch" => Self::GenericMethodResultTypeMismatch,
            "sem.method-return-type-mismatch" => Self::MethodReturnTypeMismatch,
            "sem.generic-argument-type-mismatch" => Self::GenericArgumentTypeMismatch,
            "sem.if-condition-type-mismatch" => Self::IfConditionTypeMismatch,
            "sem.if-branch-type-mismatch" => Self::IfBranchTypeMismatch,
            "sem.arithmetic-operand-type-mismatch" => Self::ArithmeticOperandTypeMismatch,
            "sem.comparison-operand-type-mismatch" => Self::ComparisonOperandTypeMismatch,
            "sem.apply-arity-mismatch" => Self::ApplyArityMismatch,
            "sem.apply-accessor-unsupported" => Self::ApplyAccessorUnsupported,
            "sem.apply-target-type-mismatch" => Self::ApplyTargetTypeMismatch,
            "sem.signal-condition-type-mismatch" => Self::SignalConditionTypeMismatch,
            "sem.signal-requires-restart" => Self::SignalRequiresRestart,
            "sem.signal-restart-mismatch" => Self::SignalRestartMismatch,
            "sem.handle-restart-result-mismatch" => Self::HandleRestartResultMismatch,
            "sem.class-of-type-mismatch" => Self::ClassOfTypeMismatch,
            "sem.instance-of-type-mismatch" => Self::InstanceOfTypeMismatch,
            "sem.instance-of-impossible" => Self::InstanceOfImpossible,
            "sem.cast-type-mismatch" => Self::CastTypeMismatch,
            "sem.cast-impossible" => Self::CastImpossible,
            "sem.make-class-argument-type-mismatch" => Self::MakeClassArgumentTypeMismatch,
            "sem.host-backed-make" => Self::HostBackedMake,
            "sem.duplicate-init-keyword" => Self::DuplicateInitKeyword,
            "sem.unknown-init-keyword" => Self::UnknownInitKeyword,
            "sem.slot-initializer-type-mismatch" => Self::SlotInitializerTypeMismatch,
            "sem.slot-default-type-mismatch" => Self::SlotDefaultTypeMismatch,
            "sem.missing-required-init-keyword" => Self::MissingRequiredInitKeyword,
            "sem.missing-slot-initializer" => Self::MissingSlotInitializer,
            "sem.unknown-slot" => Self::UnknownSlot,
            "sem.slot-receiver-type-mismatch" => Self::SlotReceiverTypeMismatch,
            "lower.invalid-semantic" => Self::LowerInvalidSemantic,
            "lower.blocked-by-diagnostics" => Self::LowerBlockedByDiagnostics,
            "tape.invalid-lowered" => Self::TapeInvalidLowered,
            "tape.blocked-by-diagnostics" => Self::TapeBlockedByDiagnostics,
            "tape.build-error" => Self::TapeBuildError,
            "runtime.missing-main" => Self::RuntimeMissingMain,
            "runtime.missing-function" => Self::RuntimeMissingFunction,
            "runtime.main-has-parameters" => Self::RuntimeMainHasParameters,
            "runtime.trap" => Self::RuntimeTrap,
            "runtime.main-returned-non-i64" => Self::RuntimeMainReturnedNonI64,
            "runtime.function-returned-non-i64" => Self::RuntimeFunctionReturnedNonI64,
            "session.unknown-source" => Self::SessionUnknownSource,
            _ => return None,
        })
    }
}

impl fmt::Display for DiagnosticCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Source label style for a diagnostic span.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticLabelStyle {
    /// Primary label. This is the main location for the diagnostic.
    Primary,
    /// Secondary label with supporting context.
    Secondary,
}

/// A source label attached to a diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticLabel {
    /// Label role.
    pub style: DiagnosticLabelStyle,
    /// Labeled source span.
    pub span: Span,
    /// Optional human-readable label text.
    pub message: Option<String>,
}

impl DiagnosticLabel {
    /// Creates a primary label.
    #[must_use]
    pub const fn primary(span: Span) -> Self {
        Self {
            style: DiagnosticLabelStyle::Primary,
            span,
            message: None,
        }
    }

    /// Creates a secondary label.
    #[must_use]
    pub fn secondary(span: Span, message: impl Into<String>) -> Self {
        Self {
            style: DiagnosticLabelStyle::Secondary,
            span,
            message: Some(message.into()),
        }
    }
}

/// Structured value stored on a diagnostic argument.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticArgValue {
    /// Textual value such as a name, keyword, or kind.
    Text(String),
    /// Stable compiler, artifact, source, or environment id.
    Id(String),
    /// Signed numeric value such as an expected or actual count.
    Number(i64),
    /// Unsigned numeric value such as a source offset.
    Unsigned(u64),
}

impl DiagnosticArgValue {
    /// Returns a displayable string for this value.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::Text(value) => value.clone(),
            Self::Id(value) => value.clone(),
            Self::Number(value) => format!("{value}"),
            Self::Unsigned(value) => format!("{value}"),
        }
    }
}

/// Named diagnostic argument consumed by renderers and tools.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticArg {
    /// Stable argument name.
    pub name: String,
    /// Argument value.
    pub value: DiagnosticArgValue,
}

impl DiagnosticArg {
    /// Creates a text argument.
    #[must_use]
    pub fn text(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: DiagnosticArgValue::Text(value.into()),
        }
    }

    /// Creates a stable ID argument.
    #[must_use]
    pub fn id(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: DiagnosticArgValue::Id(value.into()),
        }
    }

    /// Creates a signed numeric argument.
    #[must_use]
    pub fn number(name: impl Into<String>, value: i64) -> Self {
        Self {
            name: name.into(),
            value: DiagnosticArgValue::Number(value),
        }
    }

    /// Creates an unsigned numeric argument.
    #[must_use]
    pub fn unsigned(name: impl Into<String>, value: u64) -> Self {
        Self {
            name: name.into(),
            value: DiagnosticArgValue::Unsigned(value),
        }
    }
}

/// Extra diagnostic note for renderers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticNote {
    /// Stable note text or fact identifier for presentation renderers.
    pub message: String,
}

/// A compiler diagnostic with a primary source span.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    /// Severity level.
    pub severity: Severity,
    /// Stable diagnostic code.
    pub code: DiagnosticCode,
    /// Primary source span.
    pub span: Span,
    /// Source labels attached to this diagnostic.
    pub labels: Vec<DiagnosticLabel>,
    /// Structured message arguments.
    pub args: Vec<DiagnosticArg>,
    /// Additional notes.
    pub notes: Vec<DiagnosticNote>,
}

impl Diagnostic {
    /// Creates an error diagnostic with an explicit stable code.
    #[must_use]
    pub fn error(span: Span, code: DiagnosticCode) -> Self {
        Self {
            severity: Severity::Error,
            code,
            span,
            labels: Vec::from([DiagnosticLabel::primary(span)]),
            args: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// Adds one structured argument.
    #[must_use]
    pub fn with_arg(mut self, arg: DiagnosticArg) -> Self {
        self.args.push(arg);
        self
    }

    /// Adds structured arguments.
    #[must_use]
    pub fn with_args(mut self, args: Vec<DiagnosticArg>) -> Self {
        self.args.extend(args);
        self
    }

    /// Adds a source label.
    #[must_use]
    pub fn with_label(mut self, label: DiagnosticLabel) -> Self {
        self.labels.push(label);
        self
    }

    /// Adds a note.
    #[must_use]
    pub fn with_note(mut self, message: impl Into<String>) -> Self {
        self.notes.push(DiagnosticNote {
            message: message.into(),
        });
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_codes_and_args_drive_structured_diagnostics() {
        let uncategorized = Diagnostic::error(Span::new(2, 5), DiagnosticCode::Uncategorized);

        assert_eq!(uncategorized.code, DiagnosticCode::Uncategorized);
        assert!(uncategorized.args.is_empty());

        let diagnostic =
            Diagnostic::error(Span::new(2, 5), DiagnosticCode::SlotInitializerTypeMismatch)
                .with_arg(DiagnosticArg::text("slot", "valid?"))
                .with_arg(DiagnosticArg::text("expected", "<boolean>"))
                .with_arg(DiagnosticArg::text("actual", "<integer>"));

        assert_eq!(diagnostic.code, DiagnosticCode::SlotInitializerTypeMismatch);
        assert_eq!(
            diagnostic.code.as_str(),
            "sem.slot-initializer-type-mismatch"
        );
        assert_eq!(diagnostic.labels.len(), 1);
        assert!(
            diagnostic
                .args
                .iter()
                .any(|arg| arg.name == "expected" && arg.value.render() == "<boolean>")
        );
        assert!(
            diagnostic
                .args
                .iter()
                .any(|arg| arg.name == "actual" && arg.value.render() == "<integer>")
        );
        let id_arg = DiagnosticArg::id("artifact", "artabc");
        assert_eq!(id_arg.value.render(), "artabc");
    }

    #[test]
    fn parses_stable_code_strings() {
        for code in DiagnosticCode::ALL {
            assert_eq!(DiagnosticCode::parse(code.as_str()), Some(*code));
        }
        assert_eq!(
            DiagnosticCode::parse("sem.unresolved-local"),
            Some(DiagnosticCode::UnresolvedLocal)
        );
        assert_eq!(DiagnosticCode::parse("missing"), None);
    }

    #[test]
    fn non_diagnostic_errors_expose_stable_codes() {
        assert_eq!(
            crate::lower::LowerError::InvalidSemantic {
                message: String::new(),
            }
            .code(),
            DiagnosticCode::LowerInvalidSemantic
        );
        assert_eq!(
            crate::tape::TapeError::Build {
                message: String::new(),
            }
            .code(),
            DiagnosticCode::TapeBuildError
        );
        assert_eq!(
            crate::driver::CompileError::MissingMain.code(),
            DiagnosticCode::RuntimeMissingMain
        );
        assert_eq!(
            crate::session::RuntimeResult::MainHasParameters.code(),
            Some(DiagnosticCode::RuntimeMainHasParameters)
        );
    }
}
