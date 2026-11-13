//! Structured diagnostics for Interloko compiler stages.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::syntax::Span;

/// Diagnostic severity.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Severity {
    /// Advisory finding. The compiler produced its artifacts for the affected
    /// input; tools may surface the finding but must not treat it as failure.
    Warning,
    /// The compiler could not produce a valid artifact for the affected input.
    Error,
}

impl Severity {
    /// Returns the stable lowercase label used in dumps and EOP records.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
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
    /// A float literal is too large to be represented as a finite `f64`.
    AstFloatLiteralOutOfRange,
    /// A literal with an integer type suffix has a fraction or exponent.
    AstLiteralSuffixMismatch,
    /// A string literal is unterminated or contains an unsupported escape.
    AstMalformedStringLiteral,
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
    /// `:=` was used with something other than an object slot target.
    AstInvalidSlotAssignment,
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
    /// A top-level constant initializer does not produce a runtime value.
    ConstantInitializerTypeMismatch,
    /// A top-level constant initializer does not satisfy its type annotation.
    ConstantTypeMismatch,
    /// A local `let` initializer does not satisfy its type annotation.
    LocalTypeMismatch,
    /// A scalar expression context received zero or multiple values.
    ScalarResultArityMismatch,
    /// A generic method specializer does not satisfy the generic contract.
    GenericMethodSpecializerTypeMismatch,
    /// A method definition does not match the generic arity.
    GenericMethodArityMismatch,
    /// A generic has overlapping applicable methods with no unique most-specific method.
    AmbiguousGenericMethodDispatch,
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
    /// An arithmetic operand is not a number.
    ArithmeticOperandTypeMismatch,
    /// `/` was applied to integers; division is defined for floats only.
    IntegerDivisionUnsupported,
    /// A wrapping operator's operands are not two values of one fixed-width integer type.
    WrappingOperandTypeMismatch,
    /// A bitwise function received an operand that is not an integer.
    IntegerOperandTypeMismatch,
    /// A constant shift count is outside the shifted type's width.
    ShiftCountOutOfRange,
    /// An abstract numeric class such as `<real>` was used as a value type.
    AbstractNumericTypeUnsupported,
    /// A method specializes on a limited type its generic does not declare.
    LimitedTypeSpecializer,
    /// A comparison operand is not a number.
    ComparisonOperandTypeMismatch,
    /// `apply` is missing its callable target.
    ApplyArityMismatch,
    /// `apply` target is not a known callable.
    ApplyTargetTypeMismatch,
    /// A signal expression is not a known condition object.
    SignalConditionTypeMismatch,
    /// A handle body and restart handler have incompatible result kinds.
    HandleRestartResultMismatch,
    /// A block body and its exit procedure have incompatible result kinds.
    BlockExitResultMismatch,
    /// A restart guard does not produce one `<boolean>`.
    RestartGuardTypeMismatch,
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
    /// A built-in value class was passed to `make`.
    BuiltinValueMake,
    /// A construction keyword appears more than once.
    DuplicateInitKeyword,
    /// A construction keyword is not accepted by the class.
    UnknownInitKeyword,
    /// A slot initializer has the wrong type.
    SlotInitializerTypeMismatch,
    /// A slot assignment has the wrong type.
    SlotAssignmentTypeMismatch,
    /// A slot assignment targets a read-only slot.
    SlotAssignmentReadOnly,
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
    /// Soft typing: no method of a generic can apply to a call's arguments.
    TypingDispatchNoApplicableMethod,
    /// Soft typing: a generic call relies on runtime classes to find a method.
    TypingDispatchRuntimeChecked,
    /// Soft typing: a checked cast always fails when reached.
    TypingCastAlwaysFails,
    /// Soft typing: a checked cast relies on the value's runtime class.
    TypingCastRuntimeChecked,
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
    /// `main` has parameters.
    RuntimeMainHasParameters,
    /// Runtime execution trapped.
    RuntimeTrap,
    /// `<integer>` arithmetic overflowed the 64-bit fixnum range at runtime.
    RuntimeIntegerOverflow,
    /// A declared host function failed during a session-driven run.
    RuntimeHostFunctionFailed,
    /// `main` does not return exactly one supported scalar value.
    RuntimeUnsupportedResult,
    /// A signalled condition found no applicable live handler.
    RuntimeUnhandledCondition,
    /// A block exit procedure was called after its block returned.
    RuntimeExpiredExit,
    /// A condition chooser aborted the run.
    RuntimeAborted,
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
        Self::AstFloatLiteralOutOfRange,
        Self::AstLiteralSuffixMismatch,
        Self::AstMalformedStringLiteral,
        Self::AstSlotOptionRequiresKeyword,
        Self::AstSlotInitValueRequiresExpression,
        Self::AstUnsupportedSlotOption,
        Self::AstInvalidMethodSpecializer,
        Self::AstInvalidInstanceOf,
        Self::AstInvalidCast,
        Self::AstInvalidSignal,
        Self::AstInvalidSlotAssignment,
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
        Self::ConstantInitializerTypeMismatch,
        Self::ConstantTypeMismatch,
        Self::LocalTypeMismatch,
        Self::ScalarResultArityMismatch,
        Self::GenericMethodSpecializerTypeMismatch,
        Self::GenericMethodArityMismatch,
        Self::AmbiguousGenericMethodDispatch,
        Self::GenericMethodResultTypeMismatch,
        Self::MethodReturnTypeMismatch,
        Self::GenericArgumentTypeMismatch,
        Self::IfConditionTypeMismatch,
        Self::IfBranchTypeMismatch,
        Self::ArithmeticOperandTypeMismatch,
        Self::IntegerDivisionUnsupported,
        Self::WrappingOperandTypeMismatch,
        Self::IntegerOperandTypeMismatch,
        Self::ShiftCountOutOfRange,
        Self::AbstractNumericTypeUnsupported,
        Self::LimitedTypeSpecializer,
        Self::ComparisonOperandTypeMismatch,
        Self::ApplyArityMismatch,
        Self::ApplyTargetTypeMismatch,
        Self::SignalConditionTypeMismatch,
        Self::HandleRestartResultMismatch,
        Self::BlockExitResultMismatch,
        Self::RestartGuardTypeMismatch,
        Self::ClassOfTypeMismatch,
        Self::InstanceOfTypeMismatch,
        Self::InstanceOfImpossible,
        Self::CastTypeMismatch,
        Self::CastImpossible,
        Self::MakeClassArgumentTypeMismatch,
        Self::HostBackedMake,
        Self::BuiltinValueMake,
        Self::DuplicateInitKeyword,
        Self::UnknownInitKeyword,
        Self::SlotInitializerTypeMismatch,
        Self::SlotAssignmentTypeMismatch,
        Self::SlotAssignmentReadOnly,
        Self::SlotDefaultTypeMismatch,
        Self::MissingRequiredInitKeyword,
        Self::MissingSlotInitializer,
        Self::UnknownSlot,
        Self::SlotReceiverTypeMismatch,
        Self::TypingDispatchNoApplicableMethod,
        Self::TypingDispatchRuntimeChecked,
        Self::TypingCastAlwaysFails,
        Self::TypingCastRuntimeChecked,
        Self::LowerInvalidSemantic,
        Self::LowerBlockedByDiagnostics,
        Self::TapeInvalidLowered,
        Self::TapeBlockedByDiagnostics,
        Self::TapeBuildError,
        Self::RuntimeMissingMain,
        Self::RuntimeMainHasParameters,
        Self::RuntimeTrap,
        Self::RuntimeIntegerOverflow,
        Self::RuntimeHostFunctionFailed,
        Self::RuntimeUnsupportedResult,
        Self::RuntimeUnhandledCondition,
        Self::RuntimeExpiredExit,
        Self::RuntimeAborted,
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
            Self::AstFloatLiteralOutOfRange => "ast.float-literal-out-of-range",
            Self::AstLiteralSuffixMismatch => "ast.literal-suffix-mismatch",
            Self::AstMalformedStringLiteral => "ast.malformed-string-literal",
            Self::AstSlotOptionRequiresKeyword => "ast.slot-option-requires-keyword",
            Self::AstSlotInitValueRequiresExpression => "ast.slot-init-value-requires-expression",
            Self::AstUnsupportedSlotOption => "ast.unsupported-slot-option",
            Self::AstInvalidMethodSpecializer => "ast.invalid-method-specializer",
            Self::AstInvalidInstanceOf => "ast.invalid-instance-of",
            Self::AstInvalidCast => "ast.invalid-cast",
            Self::AstInvalidSignal => "ast.invalid-signal",
            Self::AstInvalidSlotAssignment => "ast.invalid-slot-assignment",
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
            Self::ConstantInitializerTypeMismatch => "sem.constant-initializer-type-mismatch",
            Self::ConstantTypeMismatch => "sem.constant-type-mismatch",
            Self::LocalTypeMismatch => "sem.local-type-mismatch",
            Self::ScalarResultArityMismatch => "sem.scalar-result-arity-mismatch",
            Self::GenericMethodSpecializerTypeMismatch => {
                "sem.generic-method-specializer-type-mismatch"
            }
            Self::GenericMethodArityMismatch => "sem.generic-method-arity-mismatch",
            Self::AmbiguousGenericMethodDispatch => "sem.ambiguous-generic-method-dispatch",
            Self::GenericMethodResultTypeMismatch => "sem.generic-method-result-type-mismatch",
            Self::MethodReturnTypeMismatch => "sem.method-return-type-mismatch",
            Self::GenericArgumentTypeMismatch => "sem.generic-argument-type-mismatch",
            Self::IfConditionTypeMismatch => "sem.if-condition-type-mismatch",
            Self::IfBranchTypeMismatch => "sem.if-branch-type-mismatch",
            Self::ArithmeticOperandTypeMismatch => "sem.arithmetic-operand-type-mismatch",
            Self::IntegerDivisionUnsupported => "sem.integer-division-unsupported",
            Self::WrappingOperandTypeMismatch => "sem.wrapping-operand-type-mismatch",
            Self::IntegerOperandTypeMismatch => "sem.integer-operand-type-mismatch",
            Self::ShiftCountOutOfRange => "sem.shift-count-out-of-range",
            Self::AbstractNumericTypeUnsupported => "sem.abstract-numeric-type-unsupported",
            Self::LimitedTypeSpecializer => "sem.limited-type-specializer",
            Self::ComparisonOperandTypeMismatch => "sem.comparison-operand-type-mismatch",
            Self::ApplyArityMismatch => "sem.apply-arity-mismatch",
            Self::ApplyTargetTypeMismatch => "sem.apply-target-type-mismatch",
            Self::SignalConditionTypeMismatch => "sem.signal-condition-type-mismatch",
            Self::HandleRestartResultMismatch => "sem.handle-restart-result-mismatch",
            Self::BlockExitResultMismatch => "sem.block-exit-result-mismatch",
            Self::RestartGuardTypeMismatch => "sem.restart-guard-type-mismatch",
            Self::ClassOfTypeMismatch => "sem.class-of-type-mismatch",
            Self::InstanceOfTypeMismatch => "sem.instance-of-type-mismatch",
            Self::InstanceOfImpossible => "sem.instance-of-impossible",
            Self::CastTypeMismatch => "sem.cast-type-mismatch",
            Self::CastImpossible => "sem.cast-impossible",
            Self::MakeClassArgumentTypeMismatch => "sem.make-class-argument-type-mismatch",
            Self::HostBackedMake => "sem.host-backed-make",
            Self::BuiltinValueMake => "sem.builtin-value-make",
            Self::DuplicateInitKeyword => "sem.duplicate-init-keyword",
            Self::UnknownInitKeyword => "sem.unknown-init-keyword",
            Self::SlotInitializerTypeMismatch => "sem.slot-initializer-type-mismatch",
            Self::SlotAssignmentTypeMismatch => "sem.slot-assignment-type-mismatch",
            Self::SlotAssignmentReadOnly => "sem.slot-assignment-read-only",
            Self::SlotDefaultTypeMismatch => "sem.slot-default-type-mismatch",
            Self::MissingRequiredInitKeyword => "sem.missing-required-init-keyword",
            Self::MissingSlotInitializer => "sem.missing-slot-initializer",
            Self::UnknownSlot => "sem.unknown-slot",
            Self::SlotReceiverTypeMismatch => "sem.slot-receiver-type-mismatch",
            Self::TypingDispatchNoApplicableMethod => "typing.dispatch-no-applicable-method",
            Self::TypingDispatchRuntimeChecked => "typing.dispatch-runtime-checked",
            Self::TypingCastAlwaysFails => "typing.cast-always-fails",
            Self::TypingCastRuntimeChecked => "typing.cast-runtime-checked",
            Self::LowerInvalidSemantic => "lower.invalid-semantic",
            Self::LowerBlockedByDiagnostics => "lower.blocked-by-diagnostics",
            Self::TapeInvalidLowered => "tape.invalid-lowered",
            Self::TapeBlockedByDiagnostics => "tape.blocked-by-diagnostics",
            Self::TapeBuildError => "tape.build-error",
            Self::RuntimeMissingMain => "runtime.missing-main",
            Self::RuntimeMainHasParameters => "runtime.main-has-parameters",
            Self::RuntimeTrap => "runtime.trap",
            Self::RuntimeIntegerOverflow => "runtime.integer-overflow",
            Self::RuntimeHostFunctionFailed => "runtime.host-function-failed",
            Self::RuntimeUnsupportedResult => "runtime.unsupported-result",
            Self::RuntimeUnhandledCondition => "runtime.unhandled-condition",
            Self::RuntimeExpiredExit => "runtime.expired-exit",
            Self::RuntimeAborted => "runtime.aborted",
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
            "ast.float-literal-out-of-range" => Self::AstFloatLiteralOutOfRange,
            "ast.literal-suffix-mismatch" => Self::AstLiteralSuffixMismatch,
            "ast.malformed-string-literal" => Self::AstMalformedStringLiteral,
            "ast.slot-option-requires-keyword" => Self::AstSlotOptionRequiresKeyword,
            "ast.slot-init-value-requires-expression" => Self::AstSlotInitValueRequiresExpression,
            "ast.unsupported-slot-option" => Self::AstUnsupportedSlotOption,
            "ast.invalid-method-specializer" => Self::AstInvalidMethodSpecializer,
            "ast.invalid-instance-of" => Self::AstInvalidInstanceOf,
            "ast.invalid-cast" => Self::AstInvalidCast,
            "ast.invalid-signal" => Self::AstInvalidSignal,
            "ast.invalid-slot-assignment" => Self::AstInvalidSlotAssignment,
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
            "sem.constant-initializer-type-mismatch" => Self::ConstantInitializerTypeMismatch,
            "sem.constant-type-mismatch" => Self::ConstantTypeMismatch,
            "sem.local-type-mismatch" => Self::LocalTypeMismatch,
            "sem.scalar-result-arity-mismatch" => Self::ScalarResultArityMismatch,
            "sem.generic-method-specializer-type-mismatch" => {
                Self::GenericMethodSpecializerTypeMismatch
            }
            "sem.generic-method-arity-mismatch" => Self::GenericMethodArityMismatch,
            "sem.ambiguous-generic-method-dispatch" => Self::AmbiguousGenericMethodDispatch,
            "sem.generic-method-result-type-mismatch" => Self::GenericMethodResultTypeMismatch,
            "sem.method-return-type-mismatch" => Self::MethodReturnTypeMismatch,
            "sem.generic-argument-type-mismatch" => Self::GenericArgumentTypeMismatch,
            "sem.if-condition-type-mismatch" => Self::IfConditionTypeMismatch,
            "sem.if-branch-type-mismatch" => Self::IfBranchTypeMismatch,
            "sem.arithmetic-operand-type-mismatch" => Self::ArithmeticOperandTypeMismatch,
            "sem.integer-division-unsupported" => Self::IntegerDivisionUnsupported,
            "sem.wrapping-operand-type-mismatch" => Self::WrappingOperandTypeMismatch,
            "sem.integer-operand-type-mismatch" => Self::IntegerOperandTypeMismatch,
            "sem.shift-count-out-of-range" => Self::ShiftCountOutOfRange,
            "sem.abstract-numeric-type-unsupported" => Self::AbstractNumericTypeUnsupported,
            "sem.limited-type-specializer" => Self::LimitedTypeSpecializer,
            "sem.comparison-operand-type-mismatch" => Self::ComparisonOperandTypeMismatch,
            "sem.apply-arity-mismatch" => Self::ApplyArityMismatch,
            "sem.apply-target-type-mismatch" => Self::ApplyTargetTypeMismatch,
            "sem.signal-condition-type-mismatch" => Self::SignalConditionTypeMismatch,
            "sem.handle-restart-result-mismatch" => Self::HandleRestartResultMismatch,
            "sem.block-exit-result-mismatch" => Self::BlockExitResultMismatch,
            "sem.restart-guard-type-mismatch" => Self::RestartGuardTypeMismatch,
            "sem.class-of-type-mismatch" => Self::ClassOfTypeMismatch,
            "sem.instance-of-type-mismatch" => Self::InstanceOfTypeMismatch,
            "sem.instance-of-impossible" => Self::InstanceOfImpossible,
            "sem.cast-type-mismatch" => Self::CastTypeMismatch,
            "sem.cast-impossible" => Self::CastImpossible,
            "sem.make-class-argument-type-mismatch" => Self::MakeClassArgumentTypeMismatch,
            "sem.host-backed-make" => Self::HostBackedMake,
            "sem.builtin-value-make" => Self::BuiltinValueMake,
            "sem.duplicate-init-keyword" => Self::DuplicateInitKeyword,
            "sem.unknown-init-keyword" => Self::UnknownInitKeyword,
            "sem.slot-initializer-type-mismatch" => Self::SlotInitializerTypeMismatch,
            "sem.slot-assignment-type-mismatch" => Self::SlotAssignmentTypeMismatch,
            "sem.slot-assignment-read-only" => Self::SlotAssignmentReadOnly,
            "sem.slot-default-type-mismatch" => Self::SlotDefaultTypeMismatch,
            "sem.missing-required-init-keyword" => Self::MissingRequiredInitKeyword,
            "sem.missing-slot-initializer" => Self::MissingSlotInitializer,
            "sem.unknown-slot" => Self::UnknownSlot,
            "sem.slot-receiver-type-mismatch" => Self::SlotReceiverTypeMismatch,
            "typing.dispatch-no-applicable-method" => Self::TypingDispatchNoApplicableMethod,
            "typing.dispatch-runtime-checked" => Self::TypingDispatchRuntimeChecked,
            "typing.cast-always-fails" => Self::TypingCastAlwaysFails,
            "typing.cast-runtime-checked" => Self::TypingCastRuntimeChecked,
            "lower.invalid-semantic" => Self::LowerInvalidSemantic,
            "lower.blocked-by-diagnostics" => Self::LowerBlockedByDiagnostics,
            "tape.invalid-lowered" => Self::TapeInvalidLowered,
            "tape.blocked-by-diagnostics" => Self::TapeBlockedByDiagnostics,
            "tape.build-error" => Self::TapeBuildError,
            "runtime.missing-main" => Self::RuntimeMissingMain,
            "runtime.main-has-parameters" => Self::RuntimeMainHasParameters,
            "runtime.trap" => Self::RuntimeTrap,
            "runtime.integer-overflow" => Self::RuntimeIntegerOverflow,
            "runtime.host-function-failed" => Self::RuntimeHostFunctionFailed,
            "runtime.unsupported-result" => Self::RuntimeUnsupportedResult,
            "runtime.unhandled-condition" => Self::RuntimeUnhandledCondition,
            "runtime.expired-exit" => Self::RuntimeExpiredExit,
            "runtime.aborted" => Self::RuntimeAborted,
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

    /// Creates an advisory warning diagnostic with an explicit stable code.
    #[must_use]
    pub fn warning(span: Span, code: DiagnosticCode) -> Self {
        Self {
            severity: Severity::Warning,
            ..Self::error(span, code)
        }
    }

    /// Returns true when this diagnostic marks its input as failed.
    #[must_use]
    pub const fn is_error(&self) -> bool {
        matches!(self.severity, Severity::Error)
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
    fn warnings_are_not_errors() {
        let warning = Diagnostic::warning(Span::new(1, 3), DiagnosticCode::Uncategorized);
        let error = Diagnostic::error(Span::new(1, 3), DiagnosticCode::Uncategorized);

        assert_eq!(warning.severity, Severity::Warning);
        assert!(!warning.is_error());
        assert!(error.is_error());
        assert_eq!(warning.labels, error.labels);
        assert_eq!(format!("{}", Severity::Warning), "warning");
        assert_eq!(Severity::Error.as_str(), "error");
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
            crate::session::RuntimeResult::MissingMain.code(),
            Some(DiagnosticCode::RuntimeMissingMain)
        );
        assert_eq!(
            crate::session::RuntimeResult::MainHasParameters.code(),
            Some(DiagnosticCode::RuntimeMainHasParameters)
        );
        let trap = |trap| crate::session::RuntimeResult::Trap {
            trap: execution_tape::vm::TrapInfo {
                unit: execution_tape::value::UnitId(0),
                func: execution_tape::value::FuncId(0),
                pc: 0,
                span_id: None,
                trap,
            },
        };
        assert_eq!(
            trap(execution_tape::vm::Trap::TrapCode(
                crate::lower::LokoTrap::IntegerOverflow.code()
            ))
            .code(),
            Some(DiagnosticCode::RuntimeIntegerOverflow)
        );
        assert_eq!(
            trap(execution_tape::vm::Trap::TrapCode(
                crate::lower::LokoTrap::InexactFloatToInteger.code()
            ))
            .code(),
            Some(DiagnosticCode::RuntimeTrap)
        );
        assert_eq!(
            trap(execution_tape::vm::Trap::TrapCode(99)).code(),
            Some(DiagnosticCode::RuntimeTrap)
        );
    }
}
