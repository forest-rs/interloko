//! Structured diagnostics for Interloko compiler stages.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::syntax::Span;
use interloko_model::condition::{CONSTANT_CYCLE_CODE, NO_APPLICABLE_METHOD_CODE, TypeErrorKind};

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
    /// Internal error: the lexer split a name or number somewhere the grammar
    /// did not, so the compiler would have read different text than the
    /// grammar matched.
    SyntaxLexerDisagreement,
    /// An integer literal does not fit in the supported integer range.
    AstIntegerLiteralOutOfRange,
    /// A float literal is too large to be represented as a finite `f64`.
    AstFloatLiteralOutOfRange,
    /// A literal with an integer type suffix has a fraction or exponent.
    AstLiteralSuffixMismatch,
    /// A string literal is unterminated or contains an unsupported escape.
    AstMalformedStringLiteral,
    /// A character literal is unterminated, holds other than one character
    /// or escape, or uses an unsupported escape.
    AstMalformedCharacterLiteral,
    /// A slot option is missing its keyword value.
    AstSlotOptionRequiresKeyword,
    /// A slot `init-value:` option is missing its expression.
    AstSlotInitValueRequiresExpression,
    /// A slot option is not supported.
    AstUnsupportedSlotOption,
    /// A method declaration has the wrong first-slice specializer shape.
    AstInvalidMethodSpecializer,
    /// A method, generic or restart parameter, which must name a class, is
    /// annotated with a class object or callable type.
    AstAnnotationNotClass,
    /// `instance?` has the wrong syntactic argument shape.
    AstInvalidInstanceOf,
    /// `as` has the wrong syntactic argument shape.
    AstInvalidCast,
    /// `limited(...)` written as an expression writes no type.
    AstInvalidTypeLiteral,
    /// `signal` has the wrong syntactic argument shape.
    AstInvalidSignal,
    /// `:=` was used with something other than a local name or an object slot.
    AstInvalidSlotAssignment,
    /// An anonymous function declares `#key` parameters, which no call
    /// through a callable value can pass yet.
    AstAnonymousKeywordParameter,
    /// A definition repeats an adjective, as in `define sealed sealed class`.
    AstDuplicateAdjective,
    /// A definition has two adjectives that contradict each other, such as
    /// `sealed` and `open`, or `abstract` and `concrete`.
    AstConflictingAdjectives,
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
    /// A hyphenated name could not be resolved, but splits at a hyphen into
    /// operands that do resolve, such as `x-1` with `x` bound. A hyphen is a
    /// name character, so the likely intent is the spaced form, `x - 1`.
    UnresolvedHyphenatedLocal,
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
    /// A generic call passes a number where no method of the generic can
    /// accept one, because every method specializes that argument on a
    /// class that numbers are not instances of.
    NoApplicableMethod,
    /// A generic call passes a number to a generic that has no methods, so
    /// none can accept it.
    GenericWithoutMethods,
    /// An `if`, `elseif`, `unless`, or `case` test is not boolean.
    IfConditionTypeMismatch,
    /// The arms of an `if` or `case` produce results that do not join.
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
    /// `limited(...)` limits a type other than `<integer>`.
    LimitedTypeBase,
    /// `limited(...)` has an option other than one `min:` and one `max:`.
    LimitedTypeOption,
    /// A `limited(...)` bound is not an `<integer>` literal.
    LimitedTypeBound,
    /// A `limited(...)` type's `min:` is above its `max:`, so no value has it.
    EmptyLimitedType,
    /// A method specializes a limited-type generic parameter on a class instead of restating the limited type.
    LimitedParameterSpecializer,
    /// A comparison operand is not a number.
    ComparisonOperandTypeMismatch,
    /// `apply` is missing its callable target.
    ApplyArityMismatch,
    /// `apply` target is not a function, generic, or callable value.
    ApplyTargetTypeMismatch,
    /// A call through a `<callable>` value cannot check at runtime that its
    /// results are the kinds the call site relies on.
    UncheckedCallableResult,
    /// A call through an untyped `<callable>` value has no context, such
    /// as a declared binding or result type, that says what it returns.
    UntypedCallableResult,
    /// A callable's parameter is narrower than the expected signature's, so
    /// it does not accept everything its callers may pass.
    CallableParameterVariance,
    /// A callable's result is wider than the expected signature's, so it may
    /// return what its callers do not expect.
    CallableResultVariance,
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
    /// An `instance?` test is statically impossible.
    InstanceOfImpossible,
    /// An `as` cast is statically impossible.
    CastImpossible,
    /// A `select`'s `by` test is not a function.
    SelectTestTypeMismatch,
    /// `make` was called with a non-class object argument.
    MakeClassArgumentTypeMismatch,
    /// A host-backed class was passed to `make`.
    HostBackedMake,
    /// A built-in value class was passed to `make`.
    BuiltinValueMake,
    /// `make` names an abstract class, which has no direct instances.
    AbstractClassMake,
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
    /// A local assignment's value does not have the local's type.
    LocalAssignmentTypeMismatch,
    /// `:=` names something other than a `let` local.
    AssignmentTargetNotAssignable,
    /// A call passes a keyword argument its callee does not take.
    UnexpectedKeywordArgument,
    /// A call passes the same keyword argument twice.
    DuplicateKeywordArgument,
    /// A definition binds a name the kernel reserves, `$unsupplied`.
    ReservedName,
    /// A keyword parameter of a type other than `<object>` declares no
    /// default, so a call that does not pass it has no value for it.
    KeywordParameterNeedsDefault,
    /// A method declares a keyword parameter its generic does not.
    MethodKeywordNotInGeneric,
    /// A method gives one of its generic's keyword parameters a default:
    /// defaults belong to the generic.
    MethodKeywordDefault,
    /// A method declares a keyword parameter with a type other than its
    /// generic's.
    MethodKeywordTypeMismatch,
    /// A function or generic that takes keyword parameters is used as a
    /// callable value, whose calls pass no keywords yet.
    KeywordCallableValue,
    /// A call through a callable value passes a keyword argument, which
    /// callable values do not take yet.
    CallableKeywordArgument,
    /// A method of a built-in collection generic specializes on a kernel
    /// class the generic already has a kernel method for.
    CollectionMethodOverridesKernel,
    /// A numeric `for` clause's start, bound or step is not an `<integer>`.
    ForClauseTypeMismatch,
    /// A slot default `init-value:` has the wrong type.
    SlotDefaultTypeMismatch,
    /// A slot's `init-value` binds locals, which slot defaults cannot hold yet.
    SlotDefaultBindsLocals,
    /// A constant's initializer reads the constant itself.
    ConstantInitializerCycle,
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
    /// Soft typing: a generic call always finds an applicable method.
    TypingDispatchProved,
    /// Soft typing: a checked cast always succeeds.
    TypingCastAlwaysSucceeds,
    /// Soft typing: an `instance?` test is always true.
    TypingInstanceAlwaysTrue,
    /// Soft typing: an `instance?` test is decided by the value's runtime class.
    TypingInstanceRuntimeChecked,
    /// Soft typing: an `instance?` test is always false.
    TypingInstanceAlwaysFalse,
    /// Soft typing: a value is always within a limited integer range.
    TypingRangeAlwaysWithin,
    /// Soft typing: whether a value is within a limited integer range is checked at runtime.
    TypingRangeRuntimeChecked,
    /// Soft typing: a value is never within a limited integer range.
    TypingRangeAlwaysOutside,
    /// Soft typing: an integer operation never traps.
    TypingOverflowImpossible,
    /// Soft typing: an integer operation may trap, so its runtime check decides.
    TypingOverflowRuntimeChecked,
    /// Soft typing: an integer operation always traps when reached.
    TypingOverflowCertain,
    /// Strict typing could not prove that a send in a strict definition
    /// finds a method, so it would be left to the runtime.
    TypingUnprovedDispatch,
    /// Strict typing could not prove a cast the compiler inserted in a
    /// strict definition, such as the check of a value returned through a
    /// `<callable>`.
    TypingUnprovedCast,
    /// Strict typing could not prove that a value reaching a limited
    /// integer declaration in a strict definition is in its range.
    TypingUnprovedRange,
    /// Strict typing could not prove that integer arithmetic in a strict
    /// definition stays within its kind, under `strict.overflow=proved`.
    TypingUnprovedOverflow,
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
    /// The runtime ended a run for a reason the session does not classify.
    RuntimeFailed,
    /// A runtime cast check failed: a value is not an instance of the class
    /// the cast expects. An unhandled `<type-error>` of
    /// [`TypeErrorKind::Cast`] ends the run with this code.
    TypeErrorCast,
    /// A callable's entry check failed: an argument is not of its
    /// parameter's class, or an exit value not of its block's class. See
    /// [`TypeErrorKind::Entry`].
    TypeErrorEntry,
    /// A runtime range check failed: a number is outside a limited type's
    /// range. See [`TypeErrorKind::Range`].
    TypeErrorRange,
    /// Integer arithmetic overflowed its kind. See
    /// [`TypeErrorKind::Overflow`].
    TypeErrorOverflow,
    /// A generic send found no method for its arguments' classes. An
    /// unhandled `<no-applicable-method>` the runtime signalled ends the run
    /// with this code, [`NO_APPLICABLE_METHOD_CODE`].
    DispatchNoApplicableMethod,
    /// A constant was read while its own initializer was running. An
    /// unhandled `<constant-cycle-error>` ends the run with this code,
    /// [`CONSTANT_CYCLE_CODE`].
    ConstantReadDuringInitialization,
    /// A session source file id is unknown.
    SessionUnknownSource,
}

impl DiagnosticCode {
    /// All stable diagnostic codes known to this compiler build.
    pub const ALL: &'static [Self] = &[
        Self::Uncategorized,
        Self::SyntaxExpectedExpression,
        Self::SyntaxExpectedToken,
        Self::SyntaxLexerDisagreement,
        Self::AstIntegerLiteralOutOfRange,
        Self::AstFloatLiteralOutOfRange,
        Self::AstLiteralSuffixMismatch,
        Self::AstMalformedStringLiteral,
        Self::AstMalformedCharacterLiteral,
        Self::AstSlotOptionRequiresKeyword,
        Self::AstSlotInitValueRequiresExpression,
        Self::AstUnsupportedSlotOption,
        Self::AstInvalidMethodSpecializer,
        Self::AstAnnotationNotClass,
        Self::AstInvalidInstanceOf,
        Self::AstInvalidCast,
        Self::AstInvalidTypeLiteral,
        Self::AstInvalidSignal,
        Self::AstInvalidSlotAssignment,
        Self::AstAnonymousKeywordParameter,
        Self::AstDuplicateAdjective,
        Self::AstConflictingAdjectives,
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
        Self::UnresolvedHyphenatedLocal,
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
        Self::NoApplicableMethod,
        Self::GenericWithoutMethods,
        Self::IfConditionTypeMismatch,
        Self::IfBranchTypeMismatch,
        Self::ArithmeticOperandTypeMismatch,
        Self::IntegerDivisionUnsupported,
        Self::WrappingOperandTypeMismatch,
        Self::IntegerOperandTypeMismatch,
        Self::ShiftCountOutOfRange,
        Self::AbstractNumericTypeUnsupported,
        Self::LimitedTypeSpecializer,
        Self::LimitedTypeBase,
        Self::LimitedTypeOption,
        Self::LimitedTypeBound,
        Self::EmptyLimitedType,
        Self::LimitedParameterSpecializer,
        Self::ComparisonOperandTypeMismatch,
        Self::ApplyArityMismatch,
        Self::ApplyTargetTypeMismatch,
        Self::UncheckedCallableResult,
        Self::UntypedCallableResult,
        Self::CallableParameterVariance,
        Self::CallableResultVariance,
        Self::SignalConditionTypeMismatch,
        Self::HandleRestartResultMismatch,
        Self::BlockExitResultMismatch,
        Self::RestartGuardTypeMismatch,
        Self::ClassOfTypeMismatch,
        Self::InstanceOfImpossible,
        Self::CastImpossible,
        Self::SelectTestTypeMismatch,
        Self::MakeClassArgumentTypeMismatch,
        Self::HostBackedMake,
        Self::BuiltinValueMake,
        Self::AbstractClassMake,
        Self::DuplicateInitKeyword,
        Self::UnknownInitKeyword,
        Self::SlotInitializerTypeMismatch,
        Self::SlotAssignmentTypeMismatch,
        Self::SlotAssignmentReadOnly,
        Self::LocalAssignmentTypeMismatch,
        Self::AssignmentTargetNotAssignable,
        Self::UnexpectedKeywordArgument,
        Self::DuplicateKeywordArgument,
        Self::ReservedName,
        Self::KeywordParameterNeedsDefault,
        Self::MethodKeywordNotInGeneric,
        Self::MethodKeywordDefault,
        Self::MethodKeywordTypeMismatch,
        Self::KeywordCallableValue,
        Self::CallableKeywordArgument,
        Self::CollectionMethodOverridesKernel,
        Self::ForClauseTypeMismatch,
        Self::SlotDefaultTypeMismatch,
        Self::SlotDefaultBindsLocals,
        Self::ConstantInitializerCycle,
        Self::MissingRequiredInitKeyword,
        Self::MissingSlotInitializer,
        Self::UnknownSlot,
        Self::SlotReceiverTypeMismatch,
        Self::TypingDispatchNoApplicableMethod,
        Self::TypingDispatchRuntimeChecked,
        Self::TypingCastAlwaysFails,
        Self::TypingCastRuntimeChecked,
        Self::TypingDispatchProved,
        Self::TypingCastAlwaysSucceeds,
        Self::TypingInstanceAlwaysTrue,
        Self::TypingInstanceRuntimeChecked,
        Self::TypingInstanceAlwaysFalse,
        Self::TypingRangeAlwaysWithin,
        Self::TypingRangeRuntimeChecked,
        Self::TypingRangeAlwaysOutside,
        Self::TypingOverflowImpossible,
        Self::TypingOverflowRuntimeChecked,
        Self::TypingOverflowCertain,
        Self::TypingUnprovedDispatch,
        Self::TypingUnprovedCast,
        Self::TypingUnprovedRange,
        Self::TypingUnprovedOverflow,
        Self::LowerInvalidSemantic,
        Self::LowerBlockedByDiagnostics,
        Self::TapeInvalidLowered,
        Self::TapeBlockedByDiagnostics,
        Self::TapeBuildError,
        Self::RuntimeMissingMain,
        Self::RuntimeMainHasParameters,
        Self::RuntimeTrap,
        Self::RuntimeHostFunctionFailed,
        Self::RuntimeUnsupportedResult,
        Self::RuntimeUnhandledCondition,
        Self::RuntimeExpiredExit,
        Self::RuntimeAborted,
        Self::RuntimeFailed,
        Self::TypeErrorCast,
        Self::TypeErrorEntry,
        Self::TypeErrorRange,
        Self::TypeErrorOverflow,
        Self::DispatchNoApplicableMethod,
        Self::ConstantReadDuringInitialization,
        Self::SessionUnknownSource,
    ];

    /// Stable kebab-case text for this diagnostic code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Uncategorized => "uncategorized",
            Self::SyntaxExpectedExpression => "syntax.expected-expression",
            Self::SyntaxExpectedToken => "syntax.expected-token",
            Self::SyntaxLexerDisagreement => "syntax.lexer-disagreement",
            Self::AstIntegerLiteralOutOfRange => "ast.integer-literal-out-of-range",
            Self::AstFloatLiteralOutOfRange => "ast.float-literal-out-of-range",
            Self::AstLiteralSuffixMismatch => "ast.literal-suffix-mismatch",
            Self::AstMalformedStringLiteral => "ast.malformed-string-literal",
            Self::AstMalformedCharacterLiteral => "ast.malformed-character-literal",
            Self::AstSlotOptionRequiresKeyword => "ast.slot-option-requires-keyword",
            Self::AstSlotInitValueRequiresExpression => "ast.slot-init-value-requires-expression",
            Self::AstUnsupportedSlotOption => "ast.unsupported-slot-option",
            Self::AstInvalidMethodSpecializer => "ast.invalid-method-specializer",
            Self::AstAnnotationNotClass => "ast.annotation-not-class",
            Self::AstInvalidInstanceOf => "ast.invalid-instance-of",
            Self::AstInvalidCast => "ast.invalid-cast",
            Self::AstInvalidTypeLiteral => "ast.invalid-type-literal",
            Self::AstInvalidSignal => "ast.invalid-signal",
            Self::AstInvalidSlotAssignment => "ast.invalid-slot-assignment",
            Self::AstAnonymousKeywordParameter => "ast.anonymous-keyword-parameter",
            Self::AstDuplicateAdjective => "ast.duplicate-adjective",
            Self::AstConflictingAdjectives => "ast.conflicting-adjectives",
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
            Self::UnresolvedHyphenatedLocal => "sem.unresolved-hyphenated-local",
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
            Self::NoApplicableMethod => "sem.no-applicable-method",
            Self::GenericWithoutMethods => "sem.generic-without-methods",
            Self::IfConditionTypeMismatch => "sem.if-condition-type-mismatch",
            Self::IfBranchTypeMismatch => "sem.if-branch-type-mismatch",
            Self::ArithmeticOperandTypeMismatch => "sem.arithmetic-operand-type-mismatch",
            Self::IntegerDivisionUnsupported => "sem.integer-division-unsupported",
            Self::WrappingOperandTypeMismatch => "sem.wrapping-operand-type-mismatch",
            Self::IntegerOperandTypeMismatch => "sem.integer-operand-type-mismatch",
            Self::ShiftCountOutOfRange => "sem.shift-count-out-of-range",
            Self::AbstractNumericTypeUnsupported => "sem.abstract-numeric-type-unsupported",
            Self::LimitedTypeSpecializer => "sem.limited-type-specializer",
            Self::LimitedTypeBase => "sem.limited-type-base",
            Self::LimitedTypeOption => "sem.limited-type-option",
            Self::LimitedTypeBound => "sem.limited-type-bound",
            Self::EmptyLimitedType => "sem.empty-limited-type",
            Self::LimitedParameterSpecializer => "sem.limited-parameter-specializer",
            Self::ComparisonOperandTypeMismatch => "sem.comparison-operand-type-mismatch",
            Self::ApplyArityMismatch => "sem.apply-arity-mismatch",
            Self::ApplyTargetTypeMismatch => "sem.apply-target-type-mismatch",
            Self::UncheckedCallableResult => "sem.unchecked-callable-result",
            Self::UntypedCallableResult => "sem.untyped-callable-result",
            Self::CallableParameterVariance => "sem.callable-parameter-variance",
            Self::CallableResultVariance => "sem.callable-result-variance",
            Self::SignalConditionTypeMismatch => "sem.signal-condition-type-mismatch",
            Self::HandleRestartResultMismatch => "sem.handle-restart-result-mismatch",
            Self::BlockExitResultMismatch => "sem.block-exit-result-mismatch",
            Self::RestartGuardTypeMismatch => "sem.restart-guard-type-mismatch",
            Self::ClassOfTypeMismatch => "sem.class-of-type-mismatch",
            Self::InstanceOfImpossible => "sem.instance-of-impossible",
            Self::CastImpossible => "sem.cast-impossible",
            Self::SelectTestTypeMismatch => "sem.select-test-type-mismatch",
            Self::MakeClassArgumentTypeMismatch => "sem.make-class-argument-type-mismatch",
            Self::HostBackedMake => "sem.host-backed-make",
            Self::BuiltinValueMake => "sem.builtin-value-make",
            Self::AbstractClassMake => "sem.abstract-class-make",
            Self::DuplicateInitKeyword => "sem.duplicate-init-keyword",
            Self::UnknownInitKeyword => "sem.unknown-init-keyword",
            Self::SlotInitializerTypeMismatch => "sem.slot-initializer-type-mismatch",
            Self::SlotAssignmentTypeMismatch => "sem.slot-assignment-type-mismatch",
            Self::SlotAssignmentReadOnly => "sem.slot-assignment-read-only",
            Self::LocalAssignmentTypeMismatch => "sem.local-assignment-type-mismatch",
            Self::AssignmentTargetNotAssignable => "sem.assignment-target-not-assignable",
            Self::UnexpectedKeywordArgument => "sem.unexpected-keyword-argument",
            Self::DuplicateKeywordArgument => "sem.duplicate-keyword-argument",
            Self::ReservedName => "sem.reserved-name",
            Self::KeywordParameterNeedsDefault => "sem.keyword-parameter-needs-default",
            Self::MethodKeywordNotInGeneric => "sem.method-keyword-not-in-generic",
            Self::MethodKeywordDefault => "sem.method-keyword-default",
            Self::MethodKeywordTypeMismatch => "sem.method-keyword-type-mismatch",
            Self::KeywordCallableValue => "sem.keyword-callable-value",
            Self::CallableKeywordArgument => "sem.callable-keyword-argument",
            Self::CollectionMethodOverridesKernel => "sem.collection-method-overrides-kernel",
            Self::ForClauseTypeMismatch => "sem.for-clause-type-mismatch",
            Self::SlotDefaultTypeMismatch => "sem.slot-default-type-mismatch",
            Self::SlotDefaultBindsLocals => "sem.slot-default-binds-locals",
            Self::ConstantInitializerCycle => "sem.constant-initializer-cycle",
            Self::MissingRequiredInitKeyword => "sem.missing-required-init-keyword",
            Self::MissingSlotInitializer => "sem.missing-slot-initializer",
            Self::UnknownSlot => "sem.unknown-slot",
            Self::SlotReceiverTypeMismatch => "sem.slot-receiver-type-mismatch",
            Self::TypingDispatchNoApplicableMethod => "typing.dispatch-no-applicable-method",
            Self::TypingDispatchRuntimeChecked => "typing.dispatch-runtime-checked",
            Self::TypingCastAlwaysFails => "typing.cast-always-fails",
            Self::TypingCastRuntimeChecked => "typing.cast-runtime-checked",
            Self::TypingDispatchProved => "typing.dispatch-proved",
            Self::TypingCastAlwaysSucceeds => "typing.cast-always-succeeds",
            Self::TypingInstanceAlwaysTrue => "typing.instance-always-true",
            Self::TypingInstanceRuntimeChecked => "typing.instance-runtime-checked",
            Self::TypingInstanceAlwaysFalse => "typing.instance-always-false",
            Self::TypingRangeAlwaysWithin => "typing.range-always-within",
            Self::TypingRangeRuntimeChecked => "typing.range-runtime-checked",
            Self::TypingRangeAlwaysOutside => "typing.range-always-outside",
            Self::TypingOverflowImpossible => "typing.overflow-impossible",
            Self::TypingOverflowRuntimeChecked => "typing.overflow-runtime-checked",
            Self::TypingOverflowCertain => "typing.overflow-certain",
            Self::TypingUnprovedDispatch => "typing.unproved-dispatch",
            Self::TypingUnprovedCast => "typing.unproved-cast",
            Self::TypingUnprovedRange => "typing.unproved-range",
            Self::TypingUnprovedOverflow => "typing.unproved-overflow",
            Self::LowerInvalidSemantic => "lower.invalid-semantic",
            Self::LowerBlockedByDiagnostics => "lower.blocked-by-diagnostics",
            Self::TapeInvalidLowered => "tape.invalid-lowered",
            Self::TapeBlockedByDiagnostics => "tape.blocked-by-diagnostics",
            Self::TapeBuildError => "tape.build-error",
            Self::RuntimeMissingMain => "runtime.missing-main",
            Self::RuntimeMainHasParameters => "runtime.main-has-parameters",
            Self::RuntimeTrap => "runtime.trap",
            Self::RuntimeHostFunctionFailed => "runtime.host-function-failed",
            Self::RuntimeUnsupportedResult => "runtime.unsupported-result",
            Self::RuntimeUnhandledCondition => "runtime.unhandled-condition",
            Self::RuntimeExpiredExit => "runtime.expired-exit",
            Self::RuntimeAborted => "runtime.aborted",
            Self::RuntimeFailed => "runtime.failed",
            Self::TypeErrorCast => TypeErrorKind::Cast.code(),
            Self::TypeErrorEntry => TypeErrorKind::Entry.code(),
            Self::TypeErrorRange => TypeErrorKind::Range.code(),
            Self::TypeErrorOverflow => TypeErrorKind::Overflow.code(),
            Self::DispatchNoApplicableMethod => NO_APPLICABLE_METHOD_CODE,
            Self::ConstantReadDuringInitialization => CONSTANT_CYCLE_CODE,
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
            "syntax.lexer-disagreement" => Self::SyntaxLexerDisagreement,
            "ast.integer-literal-out-of-range" => Self::AstIntegerLiteralOutOfRange,
            "ast.float-literal-out-of-range" => Self::AstFloatLiteralOutOfRange,
            "ast.literal-suffix-mismatch" => Self::AstLiteralSuffixMismatch,
            "ast.malformed-string-literal" => Self::AstMalformedStringLiteral,
            "ast.malformed-character-literal" => Self::AstMalformedCharacterLiteral,
            "ast.slot-option-requires-keyword" => Self::AstSlotOptionRequiresKeyword,
            "ast.slot-init-value-requires-expression" => Self::AstSlotInitValueRequiresExpression,
            "ast.unsupported-slot-option" => Self::AstUnsupportedSlotOption,
            "ast.invalid-method-specializer" => Self::AstInvalidMethodSpecializer,
            "ast.annotation-not-class" => Self::AstAnnotationNotClass,
            "ast.invalid-instance-of" => Self::AstInvalidInstanceOf,
            "ast.invalid-cast" => Self::AstInvalidCast,
            "ast.invalid-type-literal" => Self::AstInvalidTypeLiteral,
            "ast.invalid-signal" => Self::AstInvalidSignal,
            "ast.invalid-slot-assignment" => Self::AstInvalidSlotAssignment,
            "ast.anonymous-keyword-parameter" => Self::AstAnonymousKeywordParameter,
            "ast.duplicate-adjective" => Self::AstDuplicateAdjective,
            "ast.conflicting-adjectives" => Self::AstConflictingAdjectives,
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
            "sem.unresolved-hyphenated-local" => Self::UnresolvedHyphenatedLocal,
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
            "sem.no-applicable-method" => Self::NoApplicableMethod,
            "sem.generic-without-methods" => Self::GenericWithoutMethods,
            "sem.if-condition-type-mismatch" => Self::IfConditionTypeMismatch,
            "sem.if-branch-type-mismatch" => Self::IfBranchTypeMismatch,
            "sem.arithmetic-operand-type-mismatch" => Self::ArithmeticOperandTypeMismatch,
            "sem.integer-division-unsupported" => Self::IntegerDivisionUnsupported,
            "sem.wrapping-operand-type-mismatch" => Self::WrappingOperandTypeMismatch,
            "sem.integer-operand-type-mismatch" => Self::IntegerOperandTypeMismatch,
            "sem.shift-count-out-of-range" => Self::ShiftCountOutOfRange,
            "sem.abstract-numeric-type-unsupported" => Self::AbstractNumericTypeUnsupported,
            "sem.limited-type-specializer" => Self::LimitedTypeSpecializer,
            "sem.limited-type-base" => Self::LimitedTypeBase,
            "sem.limited-type-option" => Self::LimitedTypeOption,
            "sem.limited-type-bound" => Self::LimitedTypeBound,
            "sem.empty-limited-type" => Self::EmptyLimitedType,
            "sem.limited-parameter-specializer" => Self::LimitedParameterSpecializer,
            "sem.comparison-operand-type-mismatch" => Self::ComparisonOperandTypeMismatch,
            "sem.apply-arity-mismatch" => Self::ApplyArityMismatch,
            "sem.apply-target-type-mismatch" => Self::ApplyTargetTypeMismatch,
            "sem.unchecked-callable-result" => Self::UncheckedCallableResult,
            "sem.untyped-callable-result" => Self::UntypedCallableResult,
            "sem.callable-parameter-variance" => Self::CallableParameterVariance,
            "sem.callable-result-variance" => Self::CallableResultVariance,
            "sem.signal-condition-type-mismatch" => Self::SignalConditionTypeMismatch,
            "sem.handle-restart-result-mismatch" => Self::HandleRestartResultMismatch,
            "sem.block-exit-result-mismatch" => Self::BlockExitResultMismatch,
            "sem.restart-guard-type-mismatch" => Self::RestartGuardTypeMismatch,
            "sem.class-of-type-mismatch" => Self::ClassOfTypeMismatch,
            "sem.instance-of-impossible" => Self::InstanceOfImpossible,
            "sem.cast-impossible" => Self::CastImpossible,
            "sem.select-test-type-mismatch" => Self::SelectTestTypeMismatch,
            "sem.make-class-argument-type-mismatch" => Self::MakeClassArgumentTypeMismatch,
            "sem.host-backed-make" => Self::HostBackedMake,
            "sem.builtin-value-make" => Self::BuiltinValueMake,
            "sem.abstract-class-make" => Self::AbstractClassMake,
            "sem.duplicate-init-keyword" => Self::DuplicateInitKeyword,
            "sem.unknown-init-keyword" => Self::UnknownInitKeyword,
            "sem.slot-initializer-type-mismatch" => Self::SlotInitializerTypeMismatch,
            "sem.slot-assignment-type-mismatch" => Self::SlotAssignmentTypeMismatch,
            "sem.slot-assignment-read-only" => Self::SlotAssignmentReadOnly,
            "sem.local-assignment-type-mismatch" => Self::LocalAssignmentTypeMismatch,
            "sem.assignment-target-not-assignable" => Self::AssignmentTargetNotAssignable,
            "sem.unexpected-keyword-argument" => Self::UnexpectedKeywordArgument,
            "sem.duplicate-keyword-argument" => Self::DuplicateKeywordArgument,
            "sem.reserved-name" => Self::ReservedName,
            "sem.keyword-parameter-needs-default" => Self::KeywordParameterNeedsDefault,
            "sem.method-keyword-not-in-generic" => Self::MethodKeywordNotInGeneric,
            "sem.method-keyword-default" => Self::MethodKeywordDefault,
            "sem.method-keyword-type-mismatch" => Self::MethodKeywordTypeMismatch,
            "sem.keyword-callable-value" => Self::KeywordCallableValue,
            "sem.callable-keyword-argument" => Self::CallableKeywordArgument,
            "sem.collection-method-overrides-kernel" => Self::CollectionMethodOverridesKernel,
            "sem.for-clause-type-mismatch" => Self::ForClauseTypeMismatch,
            "sem.slot-default-type-mismatch" => Self::SlotDefaultTypeMismatch,
            "sem.slot-default-binds-locals" => Self::SlotDefaultBindsLocals,
            "sem.constant-initializer-cycle" => Self::ConstantInitializerCycle,
            "sem.missing-required-init-keyword" => Self::MissingRequiredInitKeyword,
            "sem.missing-slot-initializer" => Self::MissingSlotInitializer,
            "sem.unknown-slot" => Self::UnknownSlot,
            "sem.slot-receiver-type-mismatch" => Self::SlotReceiverTypeMismatch,
            "typing.dispatch-no-applicable-method" => Self::TypingDispatchNoApplicableMethod,
            "typing.dispatch-runtime-checked" => Self::TypingDispatchRuntimeChecked,
            "typing.cast-always-fails" => Self::TypingCastAlwaysFails,
            "typing.cast-runtime-checked" => Self::TypingCastRuntimeChecked,
            "typing.dispatch-proved" => Self::TypingDispatchProved,
            "typing.cast-always-succeeds" => Self::TypingCastAlwaysSucceeds,
            "typing.instance-always-true" => Self::TypingInstanceAlwaysTrue,
            "typing.instance-runtime-checked" => Self::TypingInstanceRuntimeChecked,
            "typing.instance-always-false" => Self::TypingInstanceAlwaysFalse,
            "typing.range-always-within" => Self::TypingRangeAlwaysWithin,
            "typing.range-runtime-checked" => Self::TypingRangeRuntimeChecked,
            "typing.range-always-outside" => Self::TypingRangeAlwaysOutside,
            "typing.overflow-impossible" => Self::TypingOverflowImpossible,
            "typing.overflow-runtime-checked" => Self::TypingOverflowRuntimeChecked,
            "typing.overflow-certain" => Self::TypingOverflowCertain,
            "typing.unproved-dispatch" => Self::TypingUnprovedDispatch,
            "typing.unproved-cast" => Self::TypingUnprovedCast,
            "typing.unproved-range" => Self::TypingUnprovedRange,
            "typing.unproved-overflow" => Self::TypingUnprovedOverflow,
            "lower.invalid-semantic" => Self::LowerInvalidSemantic,
            "lower.blocked-by-diagnostics" => Self::LowerBlockedByDiagnostics,
            "tape.invalid-lowered" => Self::TapeInvalidLowered,
            "tape.blocked-by-diagnostics" => Self::TapeBlockedByDiagnostics,
            "tape.build-error" => Self::TapeBuildError,
            "runtime.missing-main" => Self::RuntimeMissingMain,
            "runtime.main-has-parameters" => Self::RuntimeMainHasParameters,
            "runtime.trap" => Self::RuntimeTrap,
            "runtime.host-function-failed" => Self::RuntimeHostFunctionFailed,
            "runtime.unsupported-result" => Self::RuntimeUnsupportedResult,
            "runtime.unhandled-condition" => Self::RuntimeUnhandledCondition,
            "runtime.expired-exit" => Self::RuntimeExpiredExit,
            "runtime.aborted" => Self::RuntimeAborted,
            "runtime.failed" => Self::RuntimeFailed,
            "type-error.cast" => Self::TypeErrorCast,
            "type-error.entry" => Self::TypeErrorEntry,
            "type-error.range" => Self::TypeErrorRange,
            "type-error.overflow" => Self::TypeErrorOverflow,
            "dispatch.no-applicable-method" => Self::DispatchNoApplicableMethod,
            "constant.read-during-initialization" => Self::ConstantReadDuringInitialization,
            "session.unknown-source" => Self::SessionUnknownSource,
            _ => return None,
        })
    }
}

impl From<TypeErrorKind> for DiagnosticCode {
    /// The code an unhandled `<type-error>` of `kind` ends a run with: its
    /// own stable code.
    fn from(kind: TypeErrorKind) -> Self {
        match kind {
            TypeErrorKind::Cast => Self::TypeErrorCast,
            TypeErrorKind::Entry => Self::TypeErrorEntry,
            TypeErrorKind::Range => Self::TypeErrorRange,
            TypeErrorKind::Overflow => Self::TypeErrorOverflow,
        }
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
                span: None,
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
        let failed = crate::session::RuntimeResult::Failed {
            detail: "the VM paused with unsupported run status Future".into(),
        };
        assert_eq!(failed.code(), Some(DiagnosticCode::RuntimeFailed));
        assert_eq!(
            alloc::string::ToString::to_string(&failed),
            "runtime failed: the VM paused with unsupported run status Future"
        );
        let trap = |trap| crate::session::RuntimeResult::Trap {
            trap: execution_tape::vm::TrapInfo {
                unit: execution_tape::value::UnitId(0),
                func: execution_tape::value::FuncId(0),
                pc: execution_tape::program::Pc(0),
                span_id: None,
                trap,
            },
        };
        assert_eq!(
            trap(execution_tape::vm::Trap::TrapCode(
                crate::lower::LokoTrap::MakeClassOutsideBound.code()
            ))
            .code(),
            Some(DiagnosticCode::RuntimeTrap)
        );
        // Codes failed type checks and sends once trapped with are retired.
        assert_eq!(crate::lower::LokoTrap::from_code(1), None);
        assert_eq!(crate::lower::LokoTrap::from_code(3), None);
        let unhandled = |type_error| crate::session::RuntimeResult::UnhandledCondition {
            class: "<type-error>".into(),
            type_error,
            no_applicable_method: None,
            constant_cycle: None,
        };
        assert_eq!(
            unhandled(None).code(),
            Some(DiagnosticCode::RuntimeUnhandledCondition)
        );
        assert_eq!(
            unhandled(Some(alloc::boxed::Box::new(
                crate::session::TypeErrorReport {
                    code: "type-error.overflow".into(),
                    expected: "<integer>".into(),
                    expected_type: interloko_model::type_object::TypeObject::Class(
                        interloko_model::ClassId(1)
                    ),
                    site: "typing:def0000000000000001:s0".into(),
                    value: "#[9223372036854775807, 1]".into(),
                }
            )))
            .code(),
            Some(DiagnosticCode::TypeErrorOverflow)
        );
        let failed_send = crate::session::RuntimeResult::UnhandledCondition {
            class: "<no-applicable-method>".into(),
            type_error: None,
            no_applicable_method: Some(alloc::boxed::Box::new(
                crate::session::NoApplicableMethodReport {
                    code: "dispatch.no-applicable-method".into(),
                    generic: "area".into(),
                    arguments: alloc::vec!["{<square>}".into()],
                    argument_classes: alloc::vec!["<square>".into()],
                },
            )),
            constant_cycle: None,
        };
        assert_eq!(
            failed_send.code(),
            Some(DiagnosticCode::DispatchNoApplicableMethod)
        );
        assert_eq!(
            alloc::string::ToString::to_string(&failed_send),
            "unhandled condition <no-applicable-method>: dispatch.no-applicable-method: \
             no method of area applies to (<square>), called with ({<square>})"
        );
        let cycle = crate::session::RuntimeResult::UnhandledCondition {
            class: "<constant-cycle-error>".into(),
            type_error: None,
            no_applicable_method: None,
            constant_cycle: Some(alloc::boxed::Box::new(
                crate::session::ConstantCycleReport {
                    code: "constant.read-during-initialization".into(),
                    constant: "$x".into(),
                    reader: "twice".into(),
                },
            )),
        };
        assert_eq!(
            cycle.code(),
            Some(DiagnosticCode::ConstantReadDuringInitialization)
        );
        assert_eq!(
            alloc::string::ToString::to_string(&cycle),
            "unhandled condition <constant-cycle-error>: constant.read-during-initialization: \
             `$x` was read while its own initializer was running (via `twice`)"
        );
        assert_eq!(
            trap(execution_tape::vm::Trap::TrapCode(99)).code(),
            Some(DiagnosticCode::RuntimeTrap)
        );
    }
}
