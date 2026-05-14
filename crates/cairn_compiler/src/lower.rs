//! Backend-neutral executable IR lowered from semantic Cairn IR.
//!
//! This module owns the register-shaped executable IR that backends consume. It
//! explicitly does not own syntax parsing, semantic name resolution, runtime
//! object storage, or `execution_tape` bytecode encoding.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use crate::diagnostic::{Diagnostic, DiagnosticCode};
use crate::runtime::RuntimeLayout;
use crate::sem;
use crate::syntax::Span;

/// Durable function identifier within a lowered module.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct FunctionId(
    /// Zero-based lowered function index.
    pub u32,
);

/// Durable virtual register identifier within one lowered function.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Register(
    /// Register number. Register zero is reserved for the tape effect register.
    pub u32,
);

/// Durable label identifier within one lowered function.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct LabelId(
    /// Zero-based label index.
    pub u32,
);

/// Lowered value category.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ValueKind {
    /// Boxed top Cairn value corresponding to source `<object>`.
    Any,
    /// Signed integer value.
    I64,
    /// Unsigned integer value used by runtime metadata.
    U64,
    /// Boolean value used by control-flow conditions.
    Bool,
    /// Runtime object, optionally tied to a statically known nominal class.
    Object(Option<sem::ClassId>),
    /// Class metaobject, optionally tied to the class it represents.
    ClassObject(Option<sem::ClassId>),
    /// Generic-function metaobject, optionally tied to the generic it represents.
    GenericFunctionObject(Option<sem::GenericFunctionId>),
    /// Method metaobject, optionally tied to the method it represents.
    MethodObject(Option<sem::MethodId>),
    /// Slot descriptor metaobject.
    SlotDescriptorObject,
}

/// A lowered module ready for backend emission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Module {
    /// Runtime layout required by object host calls.
    pub runtime: RuntimeLayout,
    /// Lowered functions in [`FunctionId`] order.
    pub functions: Vec<Function>,
    /// Lowered function id for `main`, if present.
    pub main: Option<FunctionId>,
}

impl Module {
    /// Returns a stable, human-readable dump of the lowered module.
    #[must_use]
    pub fn dump(&self) -> String {
        format!("{self}")
    }

    /// Returns a lowered function by id.
    #[must_use]
    pub fn function(&self, id: FunctionId) -> Option<&Function> {
        self.functions.get(id.0 as usize)
    }
}

/// Source role for a lowered function.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FunctionKind {
    /// A source `define function`.
    Function(sem::FunctionId),
    /// A semantic method, either source-authored or compiler-synthesized.
    Method(sem::MethodId),
    /// A generated runtime dispatcher for a generic function.
    Dispatcher(sem::GenericFunctionId),
}

/// Runtime host operation emitted by lowered IR.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum RuntimeOp {
    /// Builds a class metaobject from a class id.
    ClassMetaObject,
    /// Builds a generic-function metaobject from a generic id.
    GenericMetaObject,
    /// Builds a method metaobject from a method id.
    MethodMetaObject,
    /// Builds a slot descriptor metaobject from a class id and slot id.
    SlotDescriptorObject,
    /// Returns a value's runtime class metaobject as `<object>`.
    ObjectClass,
    /// Returns the name of a class metaobject.
    ClassName,
    /// Returns the direct superclass of a class metaobject, or `#f`.
    ClassDirectSuperclass,
    /// Returns a vector of direct slot descriptor metaobjects.
    ClassDirectSlots,
    /// Returns a vector of effective slot descriptor metaobjects.
    ClassEffectiveSlots,
    /// Returns a slot descriptor's source name.
    SlotName,
    /// Returns a slot descriptor's declared type text.
    SlotType,
    /// Returns a slot descriptor's construction init keyword text.
    SlotInitKeyword,
    /// Returns whether a slot descriptor names a writable slot.
    SlotMutable,
    /// Returns a slot descriptor's getter accessor name.
    SlotGetter,
    /// Returns a slot descriptor's setter generic metaobject, or `#f`.
    SlotSetter,
    /// Returns the name of a generic-function metaobject.
    GenericFunctionName,
    /// Returns a vector of method metaobjects.
    GenericFunctionMethods,
    /// Returns the generic-function metaobject implemented by a method.
    MethodGenericFunction,
    /// Returns a vector of class metaobjects for a method's specializers.
    MethodSpecializers,
    /// Returns vector length.
    VectorSize,
    /// Returns a vector element as `<object>`.
    VectorRef,
}

/// A lowered function signature and body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Function {
    /// Lowered function id.
    pub id: FunctionId,
    /// Human-readable function name.
    pub name: String,
    /// Source role for this lowered function.
    pub kind: FunctionKind,
    /// Function parameters in input-register order.
    pub params: Vec<Param>,
    /// Function return value kind.
    pub ret: ValueKind,
    /// Source locals assigned to lowered registers.
    pub locals: Vec<Local>,
    /// Lowered instruction stream.
    pub instructions: Vec<Instr>,
    /// Source span for the function-like item.
    pub span: Span,
}

/// A lowered function parameter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Param {
    /// Source local id when this parameter came from semantic IR.
    pub source: Option<sem::LocalId>,
    /// Parameter name.
    pub name: String,
    /// Parameter value category.
    pub kind: ValueKind,
    /// Input register.
    pub reg: Register,
}

/// A lowered local assignment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Local {
    /// Semantic local id.
    pub source: sem::LocalId,
    /// Source local name.
    pub name: String,
    /// Local value category.
    pub kind: ValueKind,
    /// Register holding the local value.
    pub reg: Register,
}

/// Lowered executable instruction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Instr {
    /// Label marking a branch target.
    Label {
        /// Label id.
        label: LabelId,
    },
    /// Signed integer constant.
    ConstI64 {
        /// Destination register.
        dst: Register,
        /// Constant value.
        value: i64,
        /// Source span.
        span: Span,
    },
    /// Boolean constant.
    ConstBool {
        /// Destination register.
        dst: Register,
        /// Constant value.
        value: bool,
        /// Source span.
        span: Span,
    },
    /// Runtime string literal.
    StringLiteral {
        /// Destination register.
        dst: Register,
        /// Decoded string value.
        value: String,
        /// Built-in string class.
        class: sem::ClassId,
        /// Source span.
        span: Span,
    },
    /// Runtime symbol literal.
    SymbolLiteral {
        /// Destination register.
        dst: Register,
        /// Symbol name.
        name: String,
        /// Built-in symbol class.
        class: sem::ClassId,
        /// Source span.
        span: Span,
    },
    /// Unsigned integer constant.
    ConstU64 {
        /// Destination register.
        dst: Register,
        /// Constant value.
        value: u64,
        /// Source span.
        span: Span,
    },
    /// Class object constant represented by a runtime class id.
    ConstClass {
        /// Destination register.
        dst: Register,
        /// Nominal class represented by this value.
        class: sem::ClassId,
        /// Source span.
        span: Span,
    },
    /// Boxes a concrete value into the top `<object>` runtime value.
    BoxValue {
        /// Destination register.
        dst: Register,
        /// Source register.
        src: Register,
        /// Source value category.
        from: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Signed integer arithmetic.
    I64Binary {
        /// Destination register.
        dst: Register,
        /// Binary operator.
        op: sem::BinaryOp,
        /// Left operand register.
        lhs: Register,
        /// Right operand register.
        rhs: Register,
        /// Source span.
        span: Span,
    },
    /// Signed integer comparison.
    I64Compare {
        /// Destination register.
        dst: Register,
        /// Comparison operator.
        op: sem::CompareOp,
        /// Left operand register.
        lhs: Register,
        /// Right operand register.
        rhs: Register,
        /// Source span.
        span: Span,
    },
    /// Runtime value equality.
    ValueEqual {
        /// Destination register.
        dst: Register,
        /// Left operand register.
        lhs: Register,
        /// Right operand register.
        rhs: Register,
        /// Operand value category.
        kind: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Ordinary direct call to another lowered function.
    Call {
        /// Destination register.
        dst: Register,
        /// Lowered callee function.
        callee: FunctionId,
        /// Argument registers.
        args: Vec<Register>,
        /// Source span.
        span: Span,
    },
    /// Runtime object construction with fields already ordered by the class
    /// initialization plan.
    MakeObject {
        /// Destination register.
        dst: Register,
        /// Nominal class to instantiate.
        class: sem::ClassId,
        /// Field value registers in slot order.
        fields: Vec<Register>,
        /// Source span.
        span: Span,
    },
    /// Runtime immutable vector construction.
    VectorLiteral {
        /// Destination register.
        dst: Register,
        /// Boxed element registers in source order.
        elements: Vec<Register>,
        /// Built-in vector class.
        class: sem::ClassId,
        /// Source span.
        span: Span,
    },
    /// Runtime host call for kernel-provided object/model operations.
    RuntimeCall {
        /// Destination register.
        dst: Register,
        /// Runtime operation.
        op: RuntimeOp,
        /// Argument registers.
        args: Vec<Register>,
        /// Result value category.
        kind: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Runtime slot read.
    ReadSlot {
        /// Destination register.
        dst: Register,
        /// Receiver object register.
        receiver: Register,
        /// Statically known receiver class.
        class: sem::ClassId,
        /// Slot to read.
        slot: sem::SlotId,
        /// Slot value category.
        kind: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Runtime native slot write.
    WriteSlot {
        /// Destination register receiving the assigned value.
        dst: Register,
        /// Receiver object register.
        receiver: Register,
        /// Assigned value register.
        value: Register,
        /// Statically known receiver class.
        class: sem::ClassId,
        /// Slot to write.
        slot: sem::SlotId,
        /// Slot value category.
        kind: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Reads an object's runtime class id.
    ClassOf {
        /// Destination register.
        dst: Register,
        /// Object register.
        object: Register,
        /// Source span.
        span: Span,
    },
    /// Runtime class membership test.
    InstanceOf {
        /// Destination register.
        dst: Register,
        /// Tested value register.
        value: Register,
        /// Tested value kind.
        input: ValueKind,
        /// Target class.
        class: sem::ClassId,
        /// Source span.
        span: Span,
    },
    /// Runtime checked cast from top `<object>` to a nominal object.
    CastObject {
        /// Destination register.
        dst: Register,
        /// Cast value register.
        value: Register,
        /// Cast value kind.
        input: ValueKind,
        /// Target class.
        class: sem::ClassId,
        /// Source span.
        span: Span,
    },
    /// Unsigned integer equality.
    EqU64 {
        /// Destination register.
        dst: Register,
        /// Left operand register.
        lhs: Register,
        /// Right operand register.
        rhs: Register,
        /// Source span.
        span: Span,
    },
    /// Conditional branch.
    Branch {
        /// Boolean condition register.
        cond: Register,
        /// Target when `cond` is true.
        then_label: LabelId,
        /// Target when `cond` is false.
        else_label: LabelId,
        /// Source span.
        span: Span,
    },
    /// Unconditional jump.
    Jump {
        /// Target label.
        label: LabelId,
        /// Source span.
        span: Span,
    },
    /// Register-to-register move.
    Move {
        /// Destination register.
        dst: Register,
        /// Source register.
        src: Register,
        /// Source span.
        span: Span,
    },
    /// Function return.
    Return {
        /// Return value register.
        value: Register,
        /// Source span.
        span: Span,
    },
    /// Runtime trap.
    Trap {
        /// Trap code.
        code: u32,
        /// Source span.
        span: Span,
    },
}

/// Lowering error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LowerError {
    /// Earlier compiler diagnostics prevented lowering.
    Diagnostics {
        /// Diagnostics that must be reported to the caller.
        diagnostics: Vec<Diagnostic>,
    },
    /// Semantic IR contained an invalid placeholder.
    InvalidSemantic {
        /// Error details.
        message: String,
    },
}

impl LowerError {
    /// Stable error code for this lowering failure.
    #[must_use]
    pub const fn code(&self) -> DiagnosticCode {
        match self {
            Self::Diagnostics { .. } => DiagnosticCode::LowerBlockedByDiagnostics,
            Self::InvalidSemantic { .. } => DiagnosticCode::LowerInvalidSemantic,
        }
    }
}

/// Lowers semantic IR to executable backend-neutral IR.
pub fn lower(module: &sem::Module) -> Result<Module, LowerError> {
    if !module.diagnostics.is_empty() {
        return Err(LowerError::Diagnostics {
            diagnostics: module.diagnostics.clone(),
        });
    }

    Lowering::new(module).lower_module()
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct LowerIds {
    functions: Vec<FunctionId>,
    methods: Vec<FunctionId>,
    generics: Vec<FunctionId>,
}

struct Lowering<'module> {
    module: &'module sem::Module,
    ids: LowerIds,
}

impl<'module> Lowering<'module> {
    fn new(module: &'module sem::Module) -> Self {
        let mut ids = LowerIds::default();
        let mut next = 0_u32;

        for _ in &module.functions {
            ids.functions.push(FunctionId(next));
            next = next.saturating_add(1);
        }
        for _ in &module.methods {
            ids.methods.push(FunctionId(next));
            next = next.saturating_add(1);
        }
        for _ in &module.generics {
            ids.generics.push(FunctionId(next));
            next = next.saturating_add(1);
        }

        Self { module, ids }
    }

    fn lower_module(&self) -> Result<Module, LowerError> {
        let mut functions = Vec::with_capacity(
            self.module.functions.len() + self.module.methods.len() + self.module.generics.len(),
        );
        let mut main = None;

        for function in &self.module.functions {
            if function.name == "main" {
                main = Some(self.lower_function_id(function.id)?);
            }
            functions.push(self.lower_function(function)?);
        }
        for method in &self.module.methods {
            functions.push(self.lower_method(method)?);
        }
        for generic in &self.module.generics {
            functions.push(self.lower_dispatcher(generic)?);
        }

        Ok(Module {
            runtime: RuntimeLayout::from_sem(self.module),
            functions,
            main,
        })
    }

    fn lower_function(&self, function: &sem::Function) -> Result<Function, LowerError> {
        let mut body = BodyLowering::for_function(self.module, &self.ids, function)?;
        let ret = value_kind(function.result_kind)?;
        body.lower_block(&function.body, ret)?;
        Ok(Function {
            id: self.lower_function_id(function.id)?,
            name: function.name.clone(),
            kind: FunctionKind::Function(function.id),
            params: body.params,
            ret,
            locals: body.locals,
            instructions: body.instructions,
            span: function.span,
        })
    }

    fn lower_method(&self, method: &sem::Method) -> Result<Function, LowerError> {
        let mut body = BodyLowering::for_method(self.module, &self.ids, method)?;
        let ret = value_kind(method.result_kind)?;
        body.lower_block(&method.body, ret)?;
        let generic = self
            .module
            .generics
            .get(method.generic.0 as usize)
            .ok_or_else(|| invalid_semantic("generic id is out of range"))?;
        Ok(Function {
            id: self.lower_method_id(method.id)?,
            name: format!("{}#method{}", generic.name, method.id.0),
            kind: FunctionKind::Method(method.id),
            params: body.params,
            ret,
            locals: body.locals,
            instructions: body.instructions,
            span: method.span,
        })
    }

    fn lower_dispatcher(&self, generic: &sem::GenericFunction) -> Result<Function, LowerError> {
        if generic.dispatch_param >= generic.arity {
            return Err(invalid_semantic(
                "generic dispatch parameter is out of range",
            ));
        }

        let mut body = BodyLowering::for_dispatcher(self.module, &self.ids, generic);
        body.lower_dispatcher(generic)?;
        Ok(Function {
            id: self.lower_generic_id(generic.id)?,
            name: format!("{}#dispatch", generic.name),
            kind: FunctionKind::Dispatcher(generic.id),
            params: body.params,
            ret: value_kind(generic.result_kind)?,
            locals: body.locals,
            instructions: body.instructions,
            span: generic.span,
        })
    }

    fn lower_function_id(&self, id: sem::FunctionId) -> Result<FunctionId, LowerError> {
        self.ids
            .functions
            .get(id.0 as usize)
            .copied()
            .ok_or_else(|| invalid_semantic("function id is out of range"))
    }

    fn lower_method_id(&self, id: sem::MethodId) -> Result<FunctionId, LowerError> {
        self.ids
            .methods
            .get(id.0 as usize)
            .copied()
            .ok_or_else(|| invalid_semantic("method id is out of range"))
    }

    fn lower_generic_id(&self, id: sem::GenericFunctionId) -> Result<FunctionId, LowerError> {
        self.ids
            .generics
            .get(id.0 as usize)
            .copied()
            .ok_or_else(|| invalid_semantic("generic id is out of range"))
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Value {
    reg: Register,
    kind: ValueKind,
    reachable: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct ActiveRestart {
    specializer: sem::ClassId,
    signal_label: LabelId,
    result: Value,
    condition: Option<Value>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct MatchScratch {
    class_const: Register,
    cond: Register,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct MatchLabels {
    success: LabelId,
    failure: LabelId,
}

struct BodyLowering<'module> {
    module: &'module sem::Module,
    ids: &'module LowerIds,
    source_locals: &'module [sem::Local],
    params: Vec<Param>,
    locals: Vec<Local>,
    instructions: Vec<Instr>,
    local_regs: Vec<Option<Value>>,
    active_restarts: Vec<ActiveRestart>,
    next_reg: u32,
    next_label: u32,
}

impl<'module> BodyLowering<'module> {
    fn for_function(
        module: &'module sem::Module,
        ids: &'module LowerIds,
        function: &'module sem::Function,
    ) -> Result<Self, LowerError> {
        let mut this = Self::new(module, ids, &function.locals);
        for (index, local_id) in function.params.iter().copied().enumerate() {
            let reg = input_register(index);
            this.bind_param(function.locals.get(local_id.0 as usize), local_id, reg)?;
        }
        this.next_reg = next_after_inputs(function.params.len());
        Ok(this)
    }

    fn for_method(
        module: &'module sem::Module,
        ids: &'module LowerIds,
        method: &'module sem::Method,
    ) -> Result<Self, LowerError> {
        let mut this = Self::new(module, ids, &method.locals);
        for (index, local_id) in method.params.iter().copied().enumerate() {
            let reg = input_register(index);
            this.bind_param(method.locals.get(local_id.0 as usize), local_id, reg)?;
        }
        this.next_reg = next_after_inputs(method.params.len());
        Ok(this)
    }

    fn for_dispatcher(
        module: &'module sem::Module,
        ids: &'module LowerIds,
        generic: &'module sem::GenericFunction,
    ) -> Self {
        let params = generic
            .params
            .iter()
            .enumerate()
            .map(|(index, name)| Param {
                source: None,
                name: name.clone(),
                kind: dispatcher_param_kind(generic, index),
                reg: input_register(index),
            })
            .collect();
        Self {
            module,
            ids,
            source_locals: &[],
            params,
            locals: Vec::new(),
            instructions: Vec::new(),
            local_regs: Vec::new(),
            active_restarts: Vec::new(),
            next_reg: next_after_inputs(generic.arity),
            next_label: 0,
        }
    }

    fn new(
        module: &'module sem::Module,
        ids: &'module LowerIds,
        source_locals: &'module [sem::Local],
    ) -> Self {
        Self {
            module,
            ids,
            source_locals,
            params: Vec::new(),
            locals: Vec::new(),
            instructions: Vec::new(),
            local_regs: vec![None; source_locals.len()],
            active_restarts: Vec::new(),
            next_reg: 1,
            next_label: 0,
        }
    }

    fn bind_param(
        &mut self,
        local: Option<&sem::Local>,
        local_id: sem::LocalId,
        reg: Register,
    ) -> Result<(), LowerError> {
        let Some(local) = local else {
            return Err(invalid_semantic("parameter local id is out of range"));
        };
        let kind = value_kind(local.kind)?;
        let value = Value {
            reg,
            kind,
            reachable: true,
        };
        self.set_local(local_id, value)?;
        self.params.push(Param {
            source: Some(local_id),
            name: local.name.clone(),
            kind,
            reg,
        });
        self.locals.push(Local {
            source: local_id,
            name: local.name.clone(),
            kind,
            reg,
        });
        Ok(())
    }

    fn lower_block(&mut self, block: &sem::Block, expected: ValueKind) -> Result<(), LowerError> {
        for let_binding in &block.lets {
            let (local_name, local_kind) = {
                let local = self
                    .current_local(let_binding.local)
                    .ok_or_else(|| invalid_semantic("local id is out of range"))?;
                (local.name.clone(), local.kind)
            };
            if is_compile_time_only(local_kind) {
                continue;
            }
            let value = self.lower_expr(&let_binding.value)?;
            self.set_local(let_binding.local, value)?;
            self.locals.push(Local {
                source: let_binding.local,
                name: local_name,
                kind: value.kind,
                reg: value.reg,
            });
        }

        let result = self.lower_expr(&block.result)?;
        let result = self.coerce_value(result, expected, block.result.span())?;
        self.instructions.push(Instr::Return {
            value: result.reg,
            span: block.result.span(),
        });
        Ok(())
    }

    fn lower_dispatcher(&mut self, generic: &sem::GenericFunction) -> Result<(), LowerError> {
        for method in self.sorted_methods(generic) {
            if method.specializers.len() != generic.arity {
                return Err(invalid_semantic("method specializer arity is invalid"));
            }
            let class_const = self.alloc(ValueKind::U64);
            let cond = self.alloc(ValueKind::Bool);
            let method_result = self.alloc(value_kind(method.result_kind)?);
            let matched = self.alloc_label();
            let unmatched = self.alloc_label();

            for (index, specializer) in method.specializers.iter().copied().enumerate() {
                let Some(specializer) = specializer else {
                    continue;
                };
                let next_check = self.alloc_label();
                self.emit_specializer_match(
                    generic,
                    index,
                    specializer,
                    MatchScratch {
                        class_const: class_const.reg,
                        cond: cond.reg,
                    },
                    MatchLabels {
                        success: next_check,
                        failure: unmatched,
                    },
                    method.span,
                )?;
                self.instructions.push(Instr::Label { label: next_check });
            }
            self.instructions.push(Instr::Jump {
                label: matched,
                span: method.span,
            });
            self.instructions.push(Instr::Label { label: matched });
            let mut call_args = Vec::with_capacity(method.params.len());
            for (index, local_id) in method.params.iter().copied().enumerate() {
                let local = method
                    .locals
                    .get(local_id.0 as usize)
                    .ok_or_else(|| invalid_semantic("method parameter local id is out of range"))?;
                let value = Value {
                    reg: input_register(index),
                    kind: dispatcher_param_kind(generic, index),
                    reachable: true,
                };
                let value = self.coerce_value(value, value_kind(local.kind)?, method.span)?;
                call_args.push(value.reg);
            }
            self.instructions.push(Instr::Call {
                dst: method_result.reg,
                callee: self
                    .ids
                    .methods
                    .get(method.id.0 as usize)
                    .copied()
                    .ok_or_else(|| invalid_semantic("method id is out of range"))?,
                args: call_args,
                span: method.span,
            });
            let result =
                self.coerce_value(method_result, value_kind(generic.result_kind)?, method.span)?;
            self.instructions.push(Instr::Return {
                value: result.reg,
                span: method.span,
            });
            self.instructions.push(Instr::Label { label: unmatched });
        }

        self.instructions.push(Instr::Trap {
            code: 1,
            span: generic.span,
        });
        Ok(())
    }

    fn emit_specializer_match(
        &mut self,
        generic: &sem::GenericFunction,
        index: usize,
        specializer: sem::ClassId,
        scratch: MatchScratch,
        labels: MatchLabels,
        span: Span,
    ) -> Result<(), LowerError> {
        if matches!(
            generic.param_kinds.get(index).copied().flatten(),
            Some(sem::ValueKind::I64)
        ) {
            let label = if is_integer_value_class(self.module, specializer) {
                labels.success
            } else {
                labels.failure
            };
            self.instructions.push(Instr::Jump { label, span });
            return Ok(());
        }

        let applicable = self
            .module
            .classes
            .iter()
            .filter(|class| self.module.is_same_or_subclass(class.id, specializer))
            .map(|class| class.id)
            .collect::<Vec<_>>();
        if applicable.is_empty() {
            self.instructions.push(Instr::Jump {
                label: labels.failure,
                span,
            });
            return Ok(());
        }

        let class_reg = self.alloc(ValueKind::U64);
        self.instructions.push(Instr::ClassOf {
            dst: class_reg.reg,
            object: input_register(index),
            span,
        });
        for class in applicable {
            let next = self.alloc_label();
            self.instructions.push(Instr::ConstU64 {
                dst: scratch.class_const,
                value: u64::from(class.0),
                span,
            });
            self.instructions.push(Instr::EqU64 {
                dst: scratch.cond,
                lhs: class_reg.reg,
                rhs: scratch.class_const,
                span,
            });
            self.instructions.push(Instr::Branch {
                cond: scratch.cond,
                then_label: labels.success,
                else_label: next,
                span,
            });
            self.instructions.push(Instr::Label { label: next });
        }
        self.instructions.push(Instr::Jump {
            label: labels.failure,
            span,
        });
        Ok(())
    }

    fn sorted_methods(&self, generic: &sem::GenericFunction) -> Vec<&'module sem::Method> {
        let mut methods: Vec<_> = self
            .module
            .methods
            .iter()
            .filter(|method| method.generic == generic.id)
            .collect();
        methods.sort_by(|left, right| {
            if self.module.method_more_specific(left, right) {
                core::cmp::Ordering::Less
            } else if self.module.method_more_specific(right, left) {
                core::cmp::Ordering::Greater
            } else {
                left.id.0.cmp(&right.id.0)
            }
        });
        methods
    }

    fn lower_expr(&mut self, expr: &sem::Expr) -> Result<Value, LowerError> {
        match expr {
            sem::Expr::ConstI64 { value, span } => {
                let result = self.alloc(ValueKind::I64);
                self.instructions.push(Instr::ConstI64 {
                    dst: result.reg,
                    value: *value,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::ConstBool { value, span } => {
                let result = self.alloc(ValueKind::Bool);
                self.instructions.push(Instr::ConstBool {
                    dst: result.reg,
                    value: *value,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::StringLiteral { value, class, span } => {
                let result = self.alloc(ValueKind::Object(Some(*class)));
                self.instructions.push(Instr::StringLiteral {
                    dst: result.reg,
                    value: value.clone(),
                    class: *class,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::SymbolLiteral { name, class, span } => {
                let result = self.alloc(ValueKind::Object(Some(*class)));
                self.instructions.push(Instr::SymbolLiteral {
                    dst: result.reg,
                    name: name.clone(),
                    class: *class,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::VectorLiteral {
                elements,
                class,
                span,
            } => {
                let mut element_regs = Vec::with_capacity(elements.len());
                for element in elements {
                    let element_span = element.span();
                    let element = self.lower_expr(element)?;
                    let element = self.box_any(element, element_span)?;
                    element_regs.push(element.reg);
                }
                let result = self.alloc(ValueKind::Object(Some(*class)));
                self.instructions.push(Instr::VectorLiteral {
                    dst: result.reg,
                    elements: element_regs,
                    class: *class,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::Local {
                local,
                result_kind,
                span,
            } => {
                let value = self
                    .local_regs
                    .get(local.0 as usize)
                    .and_then(|value| *value)
                    .ok_or_else(|| invalid_semantic("local was read before assignment"))?;
                self.coerce_value(value, value_kind(*result_kind)?, *span)
            }
            sem::Expr::ClassObject { class, span } => self.lower_class_meta_object(*class, *span),
            sem::Expr::GenericFunctionObject { generic, span } => {
                self.lower_generic_meta_object(*generic, *span)
            }
            sem::Expr::KnownCallable { .. } => Err(invalid_semantic(
                "known callable values have no runtime representation",
            )),
            sem::Expr::IntrinsicCall {
                intrinsic,
                args,
                result_kind,
                span,
            } => self.lower_intrinsic_call(*intrinsic, args, *result_kind, *span),
            sem::Expr::DirectCall {
                function,
                args,
                span,
            } => {
                let function_info = self
                    .module
                    .functions
                    .get(function.0 as usize)
                    .ok_or_else(|| invalid_semantic("function id is out of range"))?;
                let args = self.lower_args_with_expected(args, &function_info.param_kinds)?;
                let result = self.alloc(value_kind(function_info.result_kind)?);
                self.instructions.push(Instr::Call {
                    dst: result.reg,
                    callee: self
                        .ids
                        .functions
                        .get(function.0 as usize)
                        .copied()
                        .ok_or_else(|| invalid_semantic("function id is out of range"))?,
                    args,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::GenericCall {
                generic,
                args,
                span,
            } => {
                let generic_info = self
                    .module
                    .generics
                    .get(generic.0 as usize)
                    .ok_or_else(|| invalid_semantic("generic id is out of range"))?;
                let args = self.lower_generic_args(args, generic_info)?;
                let result = self.alloc(value_kind(generic_info.result_kind)?);
                self.instructions.push(Instr::Call {
                    dst: result.reg,
                    callee: self
                        .ids
                        .generics
                        .get(generic.0 as usize)
                        .copied()
                        .ok_or_else(|| invalid_semantic("generic id is out of range"))?,
                    args,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::Signal {
                condition,
                condition_class,
                span,
                ..
            } => self.lower_signal(condition, *condition_class, *span),
            sem::Expr::ClassOf {
                object,
                class,
                span,
            } => {
                let object = self.lower_expr(object)?;
                let result = self.alloc(ValueKind::ClassObject(Some(*class)));
                if object.kind == ValueKind::I64 {
                    self.emit_class_meta_object(result.reg, *class, *span);
                } else {
                    let class_id = self.alloc(ValueKind::U64);
                    self.instructions.push(Instr::ClassOf {
                        dst: class_id.reg,
                        object: object.reg,
                        span: *span,
                    });
                    self.instructions.push(Instr::RuntimeCall {
                        dst: result.reg,
                        op: RuntimeOp::ClassMetaObject,
                        args: vec![class_id.reg],
                        kind: result.kind,
                        span: *span,
                    });
                }
                Ok(result)
            }
            sem::Expr::InstanceOf { value, class, span } => {
                let value = self.lower_expr(value)?;
                if value.kind == ValueKind::I64 {
                    let result = self.alloc(ValueKind::Bool);
                    self.instructions.push(Instr::ConstBool {
                        dst: result.reg,
                        value: is_integer_value_class(self.module, *class),
                        span: *span,
                    });
                    return Ok(result);
                }
                if let ValueKind::Object(Some(actual)) = value.kind
                    && self.module.is_same_or_subclass(actual, *class)
                {
                    let result = self.alloc(ValueKind::Bool);
                    self.instructions.push(Instr::ConstBool {
                        dst: result.reg,
                        value: true,
                        span: *span,
                    });
                    return Ok(result);
                }
                let result = self.alloc(ValueKind::Bool);
                self.instructions.push(Instr::InstanceOf {
                    dst: result.reg,
                    value: value.reg,
                    input: value.kind,
                    class: *class,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::Cast { value, class, span } => {
                let value = self.lower_expr(value)?;
                if value.kind == ValueKind::I64 && is_integer_value_class(self.module, *class) {
                    return Ok(value);
                }
                if let ValueKind::Object(Some(actual)) = value.kind
                    && self.module.is_same_or_subclass(actual, *class)
                {
                    return Ok(value);
                }
                let result = self.alloc(ValueKind::Object(Some(*class)));
                self.instructions.push(Instr::CastObject {
                    dst: result.reg,
                    value: value.reg,
                    input: value.kind,
                    class: *class,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::MakeObject {
                class,
                class_id,
                fields,
                span,
                ..
            } => {
                let _ = self.lower_expr(class)?;
                let class_info = self
                    .module
                    .classes
                    .get(class_id.0 as usize)
                    .ok_or_else(|| invalid_semantic("class id is out of range"))?;
                let mut field_regs = Vec::with_capacity(class_info.initialization_plan.steps.len());
                for step in &class_info.initialization_plan.steps {
                    let slot = class_info
                        .effective_slots
                        .get(step.slot.0 as usize)
                        .ok_or_else(|| invalid_semantic("initialization slot is out of range"))?;
                    let Some(field) = fields.iter().find(|field| field.slot == step.slot) else {
                        return Err(invalid_semantic("missing object field"));
                    };
                    let value = self.lower_expr(&field.value)?;
                    let value = if slot.kind == sem::ValueKind::Any {
                        self.box_any(value, field.span)?
                    } else {
                        value
                    };
                    field_regs.push(value.reg);
                }
                let result = self.alloc(ValueKind::Object(Some(*class_id)));
                self.instructions.push(Instr::MakeObject {
                    dst: result.reg,
                    class: *class_id,
                    fields: field_regs,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::AccessorCall {
                receiver,
                class,
                slot,
                result_kind,
                span,
                ..
            } => {
                let receiver = self.lower_expr(receiver)?;
                let kind = value_kind(*result_kind)?;
                let result = self.alloc(kind);
                self.instructions.push(Instr::ReadSlot {
                    dst: result.reg,
                    receiver: receiver.reg,
                    class: *class,
                    slot: *slot,
                    kind,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::SlotWrite {
                receiver,
                value,
                class,
                slot,
                result_kind,
                span,
                ..
            } => {
                let receiver = self.lower_expr(receiver)?;
                let value = self.lower_expr(value)?;
                let kind = value_kind(*result_kind)?;
                let value = self.coerce_value(value, kind, *span)?;
                let result = self.alloc(kind);
                self.instructions.push(Instr::WriteSlot {
                    dst: result.reg,
                    receiver: receiver.reg,
                    value: value.reg,
                    class: *class,
                    slot: *slot,
                    kind,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::Binary { op, lhs, rhs, span } => {
                let lhs = self.lower_expr(lhs)?;
                let rhs = self.lower_expr(rhs)?;
                let result = self.alloc(ValueKind::I64);
                self.instructions.push(Instr::I64Binary {
                    dst: result.reg,
                    op: *op,
                    lhs: lhs.reg,
                    rhs: rhs.reg,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::Compare { op, lhs, rhs, span } => {
                let lhs = self.lower_expr(lhs)?;
                let rhs = self.lower_expr(rhs)?;
                let result = self.alloc(ValueKind::Bool);
                self.instructions.push(Instr::I64Compare {
                    dst: result.reg,
                    op: *op,
                    lhs: lhs.reg,
                    rhs: rhs.reg,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::ValueEqual {
                lhs,
                rhs,
                kind,
                span,
            } => {
                let lhs = self.lower_expr(lhs)?;
                let rhs = self.lower_expr(rhs)?;
                let result = self.alloc(ValueKind::Bool);
                self.instructions.push(Instr::ValueEqual {
                    dst: result.reg,
                    lhs: lhs.reg,
                    rhs: rhs.reg,
                    kind: value_kind(*kind)?,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::If {
                condition,
                refinement: _,
                then_branch,
                else_branch,
                span,
            } => self.lower_if(condition, then_branch, else_branch, *span),
            sem::Expr::Handle {
                body,
                restart,
                span,
            } => self.lower_handle(body, restart, *span),
            sem::Expr::Error { .. } => Err(invalid_semantic("semantic expression is invalid")),
        }
    }

    fn lower_class_meta_object(
        &mut self,
        class: sem::ClassId,
        span: Span,
    ) -> Result<Value, LowerError> {
        let result = self.alloc(ValueKind::ClassObject(Some(class)));
        self.emit_class_meta_object(result.reg, class, span);
        Ok(result)
    }

    fn emit_class_meta_object(&mut self, dst: Register, class: sem::ClassId, span: Span) {
        let class_id = self.alloc(ValueKind::U64);
        self.instructions.push(Instr::ConstU64 {
            dst: class_id.reg,
            value: u64::from(class.0),
            span,
        });
        self.instructions.push(Instr::RuntimeCall {
            dst,
            op: RuntimeOp::ClassMetaObject,
            args: vec![class_id.reg],
            kind: ValueKind::ClassObject(Some(class)),
            span,
        });
    }

    fn lower_generic_meta_object(
        &mut self,
        generic: sem::GenericFunctionId,
        span: Span,
    ) -> Result<Value, LowerError> {
        let generic_id = self.alloc(ValueKind::U64);
        let result = self.alloc(ValueKind::GenericFunctionObject(Some(generic)));
        self.instructions.push(Instr::ConstU64 {
            dst: generic_id.reg,
            value: u64::from(generic.0),
            span,
        });
        self.instructions.push(Instr::RuntimeCall {
            dst: result.reg,
            op: RuntimeOp::GenericMetaObject,
            args: vec![generic_id.reg],
            kind: result.kind,
            span,
        });
        Ok(result)
    }

    fn lower_intrinsic_call(
        &mut self,
        intrinsic: sem::Intrinsic,
        args: &[sem::Expr],
        result_kind: sem::ValueKind,
        span: Span,
    ) -> Result<Value, LowerError> {
        let op = runtime_op_for_intrinsic(intrinsic);
        let mut regs = Vec::with_capacity(args.len());
        for (index, arg) in args.iter().enumerate() {
            let value = self.lower_expr(arg)?;
            let value = if runtime_op_arg_is_any(op, index) {
                self.box_any(value, arg.span())?
            } else {
                value
            };
            regs.push(value.reg);
        }
        let result = self.alloc(value_kind(result_kind)?);
        self.instructions.push(Instr::RuntimeCall {
            dst: result.reg,
            op,
            args: regs,
            kind: result.kind,
            span,
        });
        Ok(result)
    }

    fn lower_signal(
        &mut self,
        condition: &sem::Expr,
        condition_class: sem::ClassId,
        span: Span,
    ) -> Result<Value, LowerError> {
        let condition = self.lower_expr(condition)?;
        let ValueKind::Object(Some(lowered_class)) = condition.kind else {
            return Err(invalid_semantic("signal condition is not an object"));
        };
        let Some(active) = self.active_restarts.last().copied() else {
            return Err(invalid_semantic("signal has no active restart"));
        };
        if !self
            .module
            .is_same_or_subclass(lowered_class, condition_class)
            || !self
                .module
                .is_same_or_subclass(condition_class, active.specializer)
        {
            return Err(invalid_semantic(
                "signal condition class does not match restart",
            ));
        }
        if let Some(condition_slot) = active.condition {
            self.instructions.push(Instr::Move {
                dst: condition_slot.reg,
                src: condition.reg,
                span,
            });
        }
        self.instructions.push(Instr::Jump {
            label: active.signal_label,
            span,
        });
        Ok(Value {
            reg: active.result.reg,
            kind: active.result.kind,
            reachable: false,
        })
    }

    fn lower_if(
        &mut self,
        condition: &sem::Expr,
        then_branch: &sem::Expr,
        else_branch: &sem::Expr,
        span: Span,
    ) -> Result<Value, LowerError> {
        let condition = self.lower_expr(condition)?;
        if condition.kind != ValueKind::Bool {
            return Err(invalid_semantic("if condition is not bool"));
        }

        let then_label = self.alloc_label();
        let else_label = self.alloc_label();
        let join_label = self.alloc_label();
        self.instructions.push(Instr::Branch {
            cond: condition.reg,
            then_label,
            else_label,
            span,
        });

        self.instructions.push(Instr::Label { label: then_label });
        let then_value = self.lower_expr(then_branch)?;
        let result = self.alloc(then_value.kind);
        if then_value.reachable {
            self.instructions.push(Instr::Move {
                dst: result.reg,
                src: then_value.reg,
                span: then_branch.span(),
            });
            self.instructions.push(Instr::Jump {
                label: join_label,
                span,
            });
        }

        self.instructions.push(Instr::Label { label: else_label });
        let else_value = self.lower_expr(else_branch)?;
        if else_value.kind != result.kind {
            return Err(invalid_semantic("if branch result kinds differ"));
        }
        if else_value.reachable {
            self.instructions.push(Instr::Move {
                dst: result.reg,
                src: else_value.reg,
                span: else_branch.span(),
            });
            self.instructions.push(Instr::Jump {
                label: join_label,
                span,
            });
        }

        self.instructions.push(Instr::Label { label: join_label });
        Ok(Value {
            reg: result.reg,
            kind: result.kind,
            reachable: then_value.reachable || else_value.reachable,
        })
    }

    fn lower_handle(
        &mut self,
        body: &sem::Expr,
        restart: &sem::Restart,
        span: Span,
    ) -> Result<Value, LowerError> {
        let result = self.alloc(value_kind(restart.result_kind)?);
        let condition = restart
            .condition_local
            .map(|_| self.alloc(ValueKind::Object(Some(restart.specializer))));
        let signal_label = self.alloc_label();
        let join_label = self.alloc_label();
        self.active_restarts.push(ActiveRestart {
            specializer: restart.specializer,
            signal_label,
            result,
            condition,
        });
        let body_value = self.lower_expr(body)?;
        self.active_restarts.pop();
        if body_value.kind != result.kind {
            return Err(invalid_semantic("handle body kind does not match restart"));
        }
        if body_value.reachable {
            self.instructions.push(Instr::Move {
                dst: result.reg,
                src: body_value.reg,
                span: body.span(),
            });
            self.instructions.push(Instr::Jump {
                label: join_label,
                span,
            });
        }

        self.instructions.push(Instr::Label {
            label: signal_label,
        });
        if let Some(local_id) = restart.condition_local {
            let Some(condition) = condition else {
                return Err(invalid_semantic("restart condition local has no register"));
            };
            self.set_local(local_id, condition)?;
            let local = self
                .current_local(local_id)
                .ok_or_else(|| invalid_semantic("restart condition local id is out of range"))?;
            self.locals.push(Local {
                source: local_id,
                name: local.name.clone(),
                kind: condition.kind,
                reg: condition.reg,
            });
        }
        let restart_value = self.lower_expr(&restart.body)?;
        if restart_value.kind != result.kind {
            return Err(invalid_semantic(
                "restart result kind does not match handle",
            ));
        }
        self.instructions.push(Instr::Move {
            dst: result.reg,
            src: restart_value.reg,
            span: restart.body.span(),
        });
        self.instructions.push(Instr::Jump {
            label: join_label,
            span,
        });

        self.instructions.push(Instr::Label { label: join_label });
        Ok(result)
    }

    fn lower_args_with_expected(
        &mut self,
        args: &[sem::Expr],
        expected: &[sem::ValueKind],
    ) -> Result<Vec<Register>, LowerError> {
        let mut lowered = Vec::with_capacity(args.len());
        for (arg, expected) in args.iter().zip(expected) {
            let value = self.lower_expr(arg)?;
            let expected = value_kind(*expected)?;
            let value = self.coerce_value(value, expected, arg.span())?;
            lowered.push(value.reg);
        }
        Ok(lowered)
    }

    fn lower_generic_args(
        &mut self,
        args: &[sem::Expr],
        generic: &sem::GenericFunction,
    ) -> Result<Vec<Register>, LowerError> {
        let mut lowered = Vec::with_capacity(args.len());
        for (index, (arg, expected)) in args.iter().zip(&generic.param_kinds).enumerate() {
            let value = self.lower_expr(arg)?;
            let value = if let Some(expected) = *expected {
                let expected = if expected == sem::ValueKind::Any
                    && generic.dispatch_params.contains(&index)
                {
                    ValueKind::Object(None)
                } else {
                    value_kind(expected)?
                };
                self.coerce_value(value, expected, arg.span())?
            } else if generic.dispatch_params.contains(&index)
                && matches!(
                    value.kind,
                    ValueKind::Object(Some(_)) | ValueKind::Object(None)
                )
            {
                self.coerce_value(value, ValueKind::Object(None), arg.span())?
            } else {
                value
            };
            lowered.push(value.reg);
        }
        Ok(lowered)
    }

    fn coerce_value(
        &mut self,
        value: Value,
        expected: ValueKind,
        span: Span,
    ) -> Result<Value, LowerError> {
        if value.kind == expected {
            return Ok(value);
        }
        if let (ValueKind::Object(Some(actual)), ValueKind::Object(Some(expected_class))) =
            (value.kind, expected)
            && self.module.is_same_or_subclass(actual, expected_class)
        {
            return Ok(Value {
                kind: expected,
                ..value
            });
        }
        if let (ValueKind::Object(Some(actual)), ValueKind::Object(Some(expected_class))) =
            (value.kind, expected)
            && self.module.classes_may_overlap(actual, expected_class)
        {
            let casted = self.alloc(expected);
            self.instructions.push(Instr::CastObject {
                dst: casted.reg,
                value: value.reg,
                input: value.kind,
                class: expected_class,
                span,
            });
            return Ok(casted);
        }
        if matches!(
            (value.kind, expected),
            (ValueKind::Object(Some(_)), ValueKind::Object(None))
        ) {
            return Ok(Value {
                kind: expected,
                ..value
            });
        }
        if let (Some(actual), ValueKind::Object(Some(expected_class))) =
            (lowered_runtime_class(self.module, value.kind), expected)
            && self.module.is_same_or_subclass(actual, expected_class)
        {
            return Ok(Value {
                kind: expected,
                ..value
            });
        }
        if matches!(expected, ValueKind::Object(None))
            && lowered_runtime_class(self.module, value.kind).is_some()
        {
            return Ok(Value {
                kind: expected,
                ..value
            });
        }
        if expected == ValueKind::Any {
            return self.box_any(value, span);
        }
        if matches!(value.kind, ValueKind::Any | ValueKind::Object(None))
            && let ValueKind::Object(Some(class)) = expected
        {
            let casted = self.alloc(expected);
            self.instructions.push(Instr::CastObject {
                dst: casted.reg,
                value: value.reg,
                input: value.kind,
                class,
                span,
            });
            return Ok(casted);
        }
        Err(invalid_semantic(
            "semantic type checking allowed an invalid lowered coercion",
        ))
    }

    fn box_any(&mut self, value: Value, span: Span) -> Result<Value, LowerError> {
        if value.kind == ValueKind::Any {
            return Ok(value);
        }
        if value.kind == ValueKind::U64 {
            return Err(invalid_semantic(
                "runtime metadata cannot be boxed as <object>",
            ));
        }
        let boxed = self.alloc(ValueKind::Any);
        self.instructions.push(Instr::BoxValue {
            dst: boxed.reg,
            src: value.reg,
            from: value.kind,
            span,
        });
        Ok(boxed)
    }

    fn alloc(&mut self, kind: ValueKind) -> Value {
        let value = Value {
            reg: Register(self.next_reg),
            kind,
            reachable: true,
        };
        self.next_reg = self.next_reg.saturating_add(1);
        value
    }

    fn alloc_label(&mut self) -> LabelId {
        let label = LabelId(self.next_label);
        self.next_label = self.next_label.saturating_add(1);
        label
    }

    fn set_local(&mut self, id: sem::LocalId, value: Value) -> Result<(), LowerError> {
        let Some(slot) = self.local_regs.get_mut(id.0 as usize) else {
            return Err(invalid_semantic("local id is out of range"));
        };
        *slot = Some(value);
        Ok(())
    }

    fn current_local(&self, id: sem::LocalId) -> Option<&sem::Local> {
        self.source_locals.get(id.0 as usize)
    }
}

fn value_kind(kind: sem::ValueKind) -> Result<ValueKind, LowerError> {
    match kind {
        sem::ValueKind::Any => Ok(ValueKind::Any),
        sem::ValueKind::I64 => Ok(ValueKind::I64),
        sem::ValueKind::Bool => Ok(ValueKind::Bool),
        sem::ValueKind::String(class)
        | sem::ValueKind::Symbol(class)
        | sem::ValueKind::Vector(class) => Ok(ValueKind::Object(Some(class))),
        sem::ValueKind::Object(class) => Ok(ValueKind::Object(Some(class))),
        sem::ValueKind::ClassObject(class) => Ok(ValueKind::ClassObject(Some(class))),
        sem::ValueKind::GenericFunctionObject(generic) => {
            Ok(ValueKind::GenericFunctionObject(Some(generic)))
        }
        sem::ValueKind::MethodObject(method) => Ok(ValueKind::MethodObject(Some(method))),
        sem::ValueKind::SlotDescriptorObject { .. } => Ok(ValueKind::SlotDescriptorObject),
        sem::ValueKind::KnownCallable(_) | sem::ValueKind::Error => {
            Err(invalid_semantic("semantic value kind is invalid"))
        }
    }
}

fn is_compile_time_only(kind: sem::ValueKind) -> bool {
    matches!(
        kind,
        sem::ValueKind::KnownCallable(sem::KnownCallable::DirectFunction(_))
            | sem::ValueKind::KnownCallable(sem::KnownCallable::SlotAccessor { .. })
    )
}

fn runtime_op_for_intrinsic(intrinsic: sem::Intrinsic) -> RuntimeOp {
    match intrinsic {
        sem::Intrinsic::ObjectClass => RuntimeOp::ObjectClass,
        sem::Intrinsic::ClassName => RuntimeOp::ClassName,
        sem::Intrinsic::ClassDirectSuperclass => RuntimeOp::ClassDirectSuperclass,
        sem::Intrinsic::ClassDirectSlots => RuntimeOp::ClassDirectSlots,
        sem::Intrinsic::ClassEffectiveSlots => RuntimeOp::ClassEffectiveSlots,
        sem::Intrinsic::SlotName => RuntimeOp::SlotName,
        sem::Intrinsic::SlotType => RuntimeOp::SlotType,
        sem::Intrinsic::SlotInitKeyword => RuntimeOp::SlotInitKeyword,
        sem::Intrinsic::SlotMutable => RuntimeOp::SlotMutable,
        sem::Intrinsic::SlotGetter => RuntimeOp::SlotGetter,
        sem::Intrinsic::SlotSetter => RuntimeOp::SlotSetter,
        sem::Intrinsic::GenericFunctionName => RuntimeOp::GenericFunctionName,
        sem::Intrinsic::GenericFunctionMethods => RuntimeOp::GenericFunctionMethods,
        sem::Intrinsic::MethodGenericFunction => RuntimeOp::MethodGenericFunction,
        sem::Intrinsic::MethodSpecializers => RuntimeOp::MethodSpecializers,
        sem::Intrinsic::VectorSize => RuntimeOp::VectorSize,
        sem::Intrinsic::VectorRef => RuntimeOp::VectorRef,
    }
}

fn runtime_op_arg_is_any(op: RuntimeOp, index: usize) -> bool {
    match op {
        RuntimeOp::ObjectClass
        | RuntimeOp::ClassName
        | RuntimeOp::ClassDirectSuperclass
        | RuntimeOp::ClassDirectSlots
        | RuntimeOp::ClassEffectiveSlots
        | RuntimeOp::SlotName
        | RuntimeOp::SlotType
        | RuntimeOp::SlotInitKeyword
        | RuntimeOp::SlotMutable
        | RuntimeOp::SlotGetter
        | RuntimeOp::SlotSetter
        | RuntimeOp::GenericFunctionName
        | RuntimeOp::GenericFunctionMethods
        | RuntimeOp::MethodGenericFunction
        | RuntimeOp::MethodSpecializers
        | RuntimeOp::VectorSize => index == 0,
        RuntimeOp::VectorRef => index == 0,
        RuntimeOp::ClassMetaObject
        | RuntimeOp::GenericMetaObject
        | RuntimeOp::MethodMetaObject
        | RuntimeOp::SlotDescriptorObject => false,
    }
}

fn dispatcher_param_kind(generic: &sem::GenericFunction, index: usize) -> ValueKind {
    match generic.param_kinds.get(index).copied().flatten() {
        Some(sem::ValueKind::Any) if generic.dispatch_params.contains(&index) => {
            ValueKind::Object(None)
        }
        Some(sem::ValueKind::Any) => ValueKind::Any,
        Some(
            sem::ValueKind::Object(class)
            | sem::ValueKind::String(class)
            | sem::ValueKind::Symbol(class)
            | sem::ValueKind::Vector(class),
        ) => ValueKind::Object(Some(class)),
        Some(sem::ValueKind::I64) => ValueKind::I64,
        Some(sem::ValueKind::Bool) => ValueKind::Bool,
        None if generic.dispatch_params.contains(&index) => ValueKind::Object(None),
        None => ValueKind::Any,
        Some(_) => ValueKind::Object(None),
    }
}

fn is_integer_value_class(module: &sem::Module, class: sem::ClassId) -> bool {
    matches!(
        module
            .classes
            .get(class.0 as usize)
            .map(|class| class.origin),
        Some(sem::ClassOrigin::BuiltinValue {
            kind: sem::BuiltinValueClass::Integer,
        })
    )
}

fn lowered_runtime_class(module: &sem::Module, kind: ValueKind) -> Option<sem::ClassId> {
    match kind {
        ValueKind::Object(Some(class)) => Some(class),
        ValueKind::ClassObject(_) => builtin_class(module, sem::BuiltinValueClass::Class),
        ValueKind::GenericFunctionObject(_) => {
            builtin_class(module, sem::BuiltinValueClass::GenericFunction)
        }
        ValueKind::MethodObject(_) => builtin_class(module, sem::BuiltinValueClass::Method),
        ValueKind::SlotDescriptorObject => {
            builtin_class(module, sem::BuiltinValueClass::SlotDescriptor)
        }
        ValueKind::Any
        | ValueKind::I64
        | ValueKind::U64
        | ValueKind::Bool
        | ValueKind::Object(None) => None,
    }
}

fn builtin_class(module: &sem::Module, kind: sem::BuiltinValueClass) -> Option<sem::ClassId> {
    module
        .classes
        .iter()
        .find(|class| class.origin == sem::ClassOrigin::BuiltinValue { kind })
        .map(|class| class.id)
}

fn input_register(index: usize) -> Register {
    Register(u32::try_from(index + 1).unwrap_or(u32::MAX))
}

fn next_after_inputs(input_count: usize) -> u32 {
    u32::try_from(input_count + 1).unwrap_or(u32::MAX)
}

fn invalid_semantic(message: &str) -> LowerError {
    LowerError::InvalidSemantic {
        message: message.into(),
    }
}

fn runtime_op_name(op: RuntimeOp) -> &'static str {
    match op {
        RuntimeOp::ClassMetaObject => "class-object",
        RuntimeOp::GenericMetaObject => "generic-object",
        RuntimeOp::MethodMetaObject => "method-object",
        RuntimeOp::SlotDescriptorObject => "slot-descriptor-object",
        RuntimeOp::ObjectClass => "object-class",
        RuntimeOp::ClassName => "class-name",
        RuntimeOp::ClassDirectSuperclass => "class-direct-superclass",
        RuntimeOp::ClassDirectSlots => "class-direct-slots",
        RuntimeOp::ClassEffectiveSlots => "class-effective-slots",
        RuntimeOp::SlotName => "slot-name",
        RuntimeOp::SlotType => "slot-type",
        RuntimeOp::SlotInitKeyword => "slot-init-keyword",
        RuntimeOp::SlotMutable => "slot-mutable?",
        RuntimeOp::SlotGetter => "slot-getter",
        RuntimeOp::SlotSetter => "slot-setter",
        RuntimeOp::GenericFunctionName => "generic-function-name",
        RuntimeOp::GenericFunctionMethods => "generic-function-methods",
        RuntimeOp::MethodGenericFunction => "method-generic-function",
        RuntimeOp::MethodSpecializers => "method-specializers",
        RuntimeOp::VectorSize => "vector-size",
        RuntimeOp::VectorRef => "vector-ref",
    }
}

impl fmt::Display for Register {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "%r{}", self.0)
    }
}

impl fmt::Display for LabelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bb{}", self.0)
    }
}

impl fmt::Display for Module {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "lowered module")?;
        for (index, function) in self.functions.iter().enumerate() {
            if index != 0 {
                writeln!(f)?;
            }
            self.fmt_function(f, function)?;
        }
        Ok(())
    }
}

impl Module {
    fn fmt_function(&self, f: &mut fmt::Formatter<'_>, function: &Function) -> fmt::Result {
        write!(f, "fn l{} {}(", function.id.0, function.name)?;
        for (index, param) in function.params.iter().enumerate() {
            if index != 0 {
                write!(f, ", ")?;
            }
            write!(f, "{} {}: ", param.reg, param.name)?;
            self.fmt_kind(f, param.kind)?;
        }
        write!(f, ") -> ")?;
        self.fmt_kind(f, function.ret)?;
        self.fmt_kind_comment(f, function.kind)?;
        writeln!(f)?;

        for local in &function.locals {
            write!(
                f,
                "  local l{} {} = {}: ",
                local.source.0, local.name, local.reg
            )?;
            self.fmt_kind(f, local.kind)?;
            writeln!(f)?;
        }
        for instr in &function.instructions {
            self.fmt_instr(f, instr)?;
        }
        Ok(())
    }

    fn fmt_kind_comment(&self, f: &mut fmt::Formatter<'_>, kind: FunctionKind) -> fmt::Result {
        match kind {
            FunctionKind::Function(id) => writeln!(f, " ; source function {}", id.0),
            FunctionKind::Method(id) => writeln!(f, " ; method {}", id.0),
            FunctionKind::Dispatcher(id) => writeln!(f, " ; dispatcher for generic {}", id.0),
        }
    }

    fn fmt_instr(&self, f: &mut fmt::Formatter<'_>, instr: &Instr) -> fmt::Result {
        match instr {
            Instr::Label { label } => writeln!(f, "{label}:"),
            Instr::ConstI64 { dst, value, span } => {
                write!(f, "  {dst} = const.i64 {value}")?;
                fmt_span(f, *span)
            }
            Instr::ConstBool { dst, value, span } => {
                write!(f, "  {dst} = const.bool {value}")?;
                fmt_span(f, *span)
            }
            Instr::StringLiteral {
                dst,
                value,
                class,
                span,
            } => {
                write!(f, "  {dst} = literal.string ")?;
                fmt_quoted(f, value)?;
                write!(f, ": ")?;
                self.fmt_class(f, *class)?;
                fmt_span(f, *span)
            }
            Instr::SymbolLiteral {
                dst,
                name,
                class,
                span,
            } => {
                write!(f, "  {dst} = literal.symbol #{name}: ")?;
                self.fmt_class(f, *class)?;
                fmt_span(f, *span)
            }
            Instr::ConstU64 { dst, value, span } => {
                write!(f, "  {dst} = const.u64 {value}")?;
                fmt_span(f, *span)
            }
            Instr::ConstClass { dst, class, span } => {
                write!(f, "  {dst} = const.class ")?;
                self.fmt_class(f, *class)?;
                fmt_span(f, *span)
            }
            Instr::BoxValue {
                dst,
                src,
                from,
                span,
            } => {
                write!(f, "  {dst} = box.any {src}: ")?;
                self.fmt_kind(f, *from)?;
                fmt_span(f, *span)
            }
            Instr::I64Binary {
                dst,
                op,
                lhs,
                rhs,
                span,
            } => {
                let op = match op {
                    sem::BinaryOp::Add => "i64.add",
                    sem::BinaryOp::Sub => "i64.sub",
                    sem::BinaryOp::Mul => "i64.mul",
                };
                write!(f, "  {dst} = {op} {lhs}, {rhs}")?;
                fmt_span(f, *span)
            }
            Instr::I64Compare {
                dst,
                op,
                lhs,
                rhs,
                span,
            } => {
                let op = match op {
                    sem::CompareOp::Eq => "i64.eq",
                    sem::CompareOp::Lt => "i64.lt",
                    sem::CompareOp::Le => "i64.le",
                    sem::CompareOp::Gt => "i64.gt",
                    sem::CompareOp::Ge => "i64.ge",
                };
                write!(f, "  {dst} = {op} {lhs}, {rhs}")?;
                fmt_span(f, *span)
            }
            Instr::ValueEqual {
                dst,
                lhs,
                rhs,
                kind,
                span,
            } => {
                write!(f, "  {dst} = value.eq {lhs}, {rhs}: ")?;
                self.fmt_kind(f, *kind)?;
                fmt_span(f, *span)
            }
            Instr::Call {
                dst,
                callee,
                args,
                span,
            } => {
                write!(f, "  {dst} = call l{}", callee.0)?;
                self.fmt_arg_list(f, args)?;
                fmt_span(f, *span)
            }
            Instr::MakeObject {
                dst,
                class,
                fields,
                span,
            } => {
                write!(f, "  {dst} = make ")?;
                self.fmt_class(f, *class)?;
                self.fmt_arg_list(f, fields)?;
                fmt_span(f, *span)
            }
            Instr::VectorLiteral {
                dst,
                elements,
                class,
                span,
            } => {
                write!(f, "  {dst} = literal.vector ")?;
                self.fmt_class(f, *class)?;
                self.fmt_arg_list(f, elements)?;
                fmt_span(f, *span)
            }
            Instr::RuntimeCall {
                dst,
                op,
                args,
                kind,
                span,
            } => {
                write!(f, "  {dst} = runtime.{}", runtime_op_name(*op))?;
                self.fmt_arg_list(f, args)?;
                write!(f, ": ")?;
                self.fmt_kind(f, *kind)?;
                fmt_span(f, *span)
            }
            Instr::ReadSlot {
                dst,
                receiver,
                class,
                slot,
                kind,
                span,
            } => {
                write!(f, "  {dst} = slot.read {receiver}, ")?;
                self.fmt_slot(f, *class, *slot)?;
                write!(f, ": ")?;
                self.fmt_kind(f, *kind)?;
                fmt_span(f, *span)
            }
            Instr::WriteSlot {
                dst,
                receiver,
                value,
                class,
                slot,
                kind,
                span,
            } => {
                write!(f, "  {dst} = slot.write {receiver}, ")?;
                self.fmt_slot(f, *class, *slot)?;
                write!(f, ", {value}: ")?;
                self.fmt_kind(f, *kind)?;
                fmt_span(f, *span)
            }
            Instr::ClassOf { dst, object, span } => {
                write!(f, "  {dst} = class_of {object}")?;
                fmt_span(f, *span)
            }
            Instr::InstanceOf {
                dst,
                value,
                input,
                class,
                span,
            } => {
                write!(f, "  {dst} = instance? {value}: ")?;
                self.fmt_kind(f, *input)?;
                write!(f, ", ")?;
                self.fmt_class(f, *class)?;
                fmt_span(f, *span)
            }
            Instr::CastObject {
                dst,
                value,
                input,
                class,
                span,
            } => {
                write!(f, "  {dst} = as ")?;
                self.fmt_class(f, *class)?;
                write!(f, ", {value}: ")?;
                self.fmt_kind(f, *input)?;
                fmt_span(f, *span)
            }
            Instr::EqU64 {
                dst,
                lhs,
                rhs,
                span,
            } => {
                write!(f, "  {dst} = u64.eq {lhs}, {rhs}")?;
                fmt_span(f, *span)
            }
            Instr::Branch {
                cond,
                then_label,
                else_label,
                span,
            } => {
                write!(f, "  br {cond}, {then_label}, {else_label}")?;
                fmt_span(f, *span)
            }
            Instr::Jump { label, span } => {
                write!(f, "  jmp {label}")?;
                fmt_span(f, *span)
            }
            Instr::Move { dst, src, span } => {
                write!(f, "  {dst} = mov {src}")?;
                fmt_span(f, *span)
            }
            Instr::Return { value, span } => {
                write!(f, "  return {value}")?;
                fmt_span(f, *span)
            }
            Instr::Trap { code, span } => {
                write!(f, "  trap {code}")?;
                fmt_span(f, *span)
            }
        }
    }

    fn fmt_kind(&self, f: &mut fmt::Formatter<'_>, kind: ValueKind) -> fmt::Result {
        match kind {
            ValueKind::Any => write!(f, "any"),
            ValueKind::I64 => write!(f, "i64"),
            ValueKind::U64 => write!(f, "u64"),
            ValueKind::Bool => write!(f, "bool"),
            ValueKind::Object(Some(class)) => {
                write!(f, "object(")?;
                self.fmt_class(f, class)?;
                write!(f, ")")
            }
            ValueKind::Object(None) => write!(f, "object"),
            ValueKind::ClassObject(Some(class)) => {
                write!(f, "class(")?;
                self.fmt_class(f, class)?;
                write!(f, ")")
            }
            ValueKind::ClassObject(None) => write!(f, "class"),
            ValueKind::GenericFunctionObject(Some(generic)) => {
                if let Some(generic_info) = self.runtime.generics.get(generic.0 as usize) {
                    write!(f, "generic({})", generic_info.name)
                } else {
                    write!(f, "generic#{}", generic.0)
                }
            }
            ValueKind::GenericFunctionObject(None) => write!(f, "generic"),
            ValueKind::MethodObject(Some(method)) => write!(f, "method#{}", method.0),
            ValueKind::MethodObject(None) => write!(f, "method"),
            ValueKind::SlotDescriptorObject => write!(f, "slot-descriptor"),
        }
    }

    fn fmt_class(&self, f: &mut fmt::Formatter<'_>, class: sem::ClassId) -> fmt::Result {
        if let Some(class_info) = self.runtime.classes.get(class.0 as usize) {
            write!(f, "{}", class_info.name)
        } else {
            write!(f, "class#{}", class.0)
        }
    }

    fn fmt_slot(
        &self,
        f: &mut fmt::Formatter<'_>,
        class: sem::ClassId,
        slot: sem::SlotId,
    ) -> fmt::Result {
        self.fmt_class(f, class)?;
        write!(f, ".")?;
        if let Some(slot_info) = self
            .runtime
            .classes
            .get(class.0 as usize)
            .and_then(|class_info| class_info.slots.get(slot.0 as usize))
        {
            write!(f, "{}", slot_info.name)
        } else {
            write!(f, "slot#{}", slot.0)
        }
    }

    fn fmt_arg_list(&self, f: &mut fmt::Formatter<'_>, args: &[Register]) -> fmt::Result {
        write!(f, "(")?;
        for (index, arg) in args.iter().enumerate() {
            if index != 0 {
                write!(f, ", ")?;
            }
            write!(f, "{arg}")?;
        }
        write!(f, ")")
    }
}

fn fmt_span(f: &mut fmt::Formatter<'_>, span: Span) -> fmt::Result {
    if span.is_empty() {
        writeln!(f)
    } else {
        writeln!(f, " ; span {}..{}", span.start, span.end)
    }
}

fn fmt_quoted(f: &mut fmt::Formatter<'_>, value: &str) -> fmt::Result {
    write!(f, "\"")?;
    for ch in value.chars() {
        match ch {
            '\n' => write!(f, "\\n")?,
            '\t' => write!(f, "\\t")?,
            '"' => write!(f, "\\\"")?,
            '\\' => write!(f, "\\\\")?,
            ch => write!(f, "{ch}")?,
        }
    }
    write!(f, "\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ast, syntax};

    fn lower_source(source: &str) -> Module {
        let tree = syntax::parse(source);
        let ast = ast::lower(&tree);
        let sem = sem::lower(&ast);
        lower(&sem).expect("lowering should succeed")
    }

    #[test]
    fn lowers_direct_calls_arithmetic_and_locals() {
        let module = lower_source(
            "define function add(a, b)\n  let c = a + b;\n  c\nend;\n\
             define function main()\n  add(40, 2)\nend;",
        );
        assert_eq!(module.functions.len(), 2);
        assert_eq!(module.main, Some(FunctionId(1)));
        let dump = module.dump();
        assert!(dump.contains("local l2 c = %r3: i64"));
        assert!(dump.contains("%r3 = i64.add %r1, %r2"));
        assert!(dump.contains("call l0(%r1, %r2)"));
    }

    #[test]
    fn lowers_known_callable_apply_to_existing_calls() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
             define function inc(x)\n  x + 1\nend;\n\
             define function main()\n  let f = inc;\n  let g = area;\n  apply(f, 2) + apply(g, make(<circle>, radius: 3))\nend;",
        );

        let dump = module.dump();
        assert!(dump.contains("fn l0 inc"));
        assert!(dump.contains("fn l1 main"));
        assert!(dump.contains("call l0("));
        assert!(dump.contains("call l3("));
        assert!(!dump.contains("local l0 f"));
        assert!(dump.contains("local l1 g"));
    }

    #[test]
    fn lowers_generic_dispatch_object_construction_and_slots() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
             define function main()\n  area(make(<circle>, radius: 10))\nend;",
        );
        assert_eq!(module.functions.len(), 5);
        assert_eq!(module.main, Some(FunctionId(0)));
        let dump = module.dump();
        assert!(dump.contains("runtime.class-object"));
        assert!(dump.contains("make <circle>(%r"));
        assert!(dump.contains("slot.read %r1, <circle>.radius: i64"));
        assert!(dump.contains("area#dispatch(%r1 x: object) -> i64"));
        assert!(dump.contains("class_of %r1"));
        assert!(dump.contains("br %r"));
    }

    #[test]
    fn lowers_parent_dispatch_as_applicable_to_child_classes() {
        let module = lower_source(
            "define class <shape> (<object>)\n  slot size :: <integer>, init-value: 3;\nend;\n\
             define class <circle> (<shape>)\n  slot radius :: <integer>, init-value: 4;\nend;\n\
             define generic score(x :: <shape>) => <integer>;\n\
             define method score(s :: <shape>) => <integer>\n  s.size\nend;\n\
             define function main()\n  score(make(<circle>))\nend;",
        );

        let dump = module.dump();
        assert!(dump.contains("score#dispatch(%r1 x: object(<shape>)) -> i64"));
        assert!(dump.contains("const.u64 0"));
        assert!(dump.contains("const.u64 1"));
        assert!(dump.contains("call l1(%r1)"));
    }

    #[test]
    fn lowers_object_construction_in_initialization_plan_order() {
        let module = lower_source(
            "define class <shape> (<object>)\n  slot x :: <integer>, init-value: 1;\nend;\n\
             define class <point> (<shape>)\n  slot y :: <integer>, init-value: 2;\nend;\n\
             define function main()\n  make(<point>, x: 7).y\nend;",
        );
        let main = module
            .function(module.main.expect("main should exist"))
            .expect("main should be lowered");
        let make = main
            .instructions
            .iter()
            .find_map(|instr| {
                if let Instr::MakeObject { class, fields, .. } = instr {
                    Some((*class, fields.as_slice()))
                } else {
                    None
                }
            })
            .expect("make should be lowered");

        assert_eq!(make.0, sem::ClassId(1));
        assert_eq!(make.1.len(), 2);
        assert_eq!(const_i64(main, make.1[0]), Some(7));
        assert_eq!(const_i64(main, make.1[1]), Some(2));
    }

    #[test]
    fn lowers_typed_generic_contracts_and_boxes_dispatcher_results() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic keep(x :: <object>) => <object>;\n\
             define method keep(c :: <circle>) => <circle>\n  c\nend;\n\
             define function main()\n  let ignored = keep(make(<circle>, radius: 5));\n  42\nend;",
        );

        let dump = module.dump();
        assert!(dump.contains("fn l1 keep#method0(%r1 c: object(<circle>)) -> object(<circle>)"));
        assert!(dump.contains("keep#dispatch(%r1 x: object) -> any"));
        assert!(dump.contains("box.any"));
    }

    #[test]
    fn lowers_typed_runtime_slot_reads() {
        let module = lower_source(
            "define class <flag> (<object>)\n  slot valid? :: <boolean>;\nend;\n\
             define function main()\n  if make(<flag>, valid?: #t).valid? then 1 else 0 end\nend;",
        );

        let dump = module.dump();
        assert!(dump.contains("make <flag>(%r"));
        assert!(dump.contains("slot.read %r"));
        assert!(dump.contains("<flag>.valid?: bool"));
    }

    #[test]
    fn lowers_object_top_slots_with_boxing() {
        let module = lower_source(
            "define class <box> (<object>)\n  slot value;\nend;\n\
             define function main()\n  make(<box>, value: 7).value\nend;",
        );

        let dump = module.dump();
        assert!(dump.contains("box.any"));
        assert!(dump.contains("<box>.value: any"));
        assert_eq!(module.runtime.classes[0].slots[0].kind, sem::ValueKind::Any);
    }

    #[test]
    fn lowers_known_class_objects_to_metaobjects() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function circle-class()\n  <circle>\nend;\n\
             define function main()\n  let klass = circle-class();\n  make(klass, radius: 9).radius\nend;",
        );

        let dump = module.dump();
        assert!(dump.contains("fn l0 circle-class() -> class(<circle>)"));
        assert!(dump.contains("runtime.class-object"));
        assert!(dump.contains("call l0()"));
        assert!(dump.contains("make <circle>"));
    }

    #[test]
    fn lowers_class_of_to_existing_class_id_operation() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function main()\n  let c = make(<circle>, radius: 1);\n  let klass = class-of(c);\n  make(klass, radius: 10).radius\nend;",
        );

        let dump = module.dump();
        assert!(dump.contains("class_of"));
        assert!(dump.contains("local l1 klass = %r"));
        assert!(dump.contains("class(<circle>)"));
    }

    #[test]
    fn lowers_typed_direct_function_signatures() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function pass(c :: <circle>) => <circle>\n  c\nend;\n\
             define function circle-class() => class <circle>\n  <circle>\nend;\n\
             define function build(klass :: class <circle>, radius :: <integer>) => <circle>\n  make(klass, radius: radius)\nend;\n\
             define function main()\n  build(circle-class(), pass(make(<circle>, radius: 5)).radius).radius\nend;",
        );

        let dump = module.dump();
        assert!(dump.contains("fn l0 pass(%r1 c: object(<circle>)) -> object(<circle>)"));
        assert!(dump.contains("fn l1 circle-class() -> class(<circle>)"));
        assert!(dump.contains("klass: class(<circle>)"));
        assert!(dump.contains("make <circle>"));
    }

    #[test]
    fn lowers_object_top_function_boundaries_with_boxing() {
        let module = lower_source(
            "define function hold(x :: <object>) => <object>\n  x\nend;\n\
             define function number-object() => <object>\n  7\nend;\n\
             define function main()\n  let ignored = hold(7);\n  let also = number-object();\n  42\nend;",
        );

        let dump = module.dump();
        assert!(dump.contains("fn l0 hold(%r1 x: any) -> any"));
        assert!(dump.contains("fn l1 number-object() -> any"));
        assert!(dump.matches("box.any").count() >= 2);
    }

    #[test]
    fn lowers_instance_tests_casts_and_refined_locals() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function radius-or-zero(x :: <object>) => <integer>\n  if instance?(x, <circle>) then x.radius else 0 end\nend;\n\
             define function main()\n  radius-or-zero(make(<circle>, radius: 8))\nend;",
        );

        let dump = module.dump();
        assert!(dump.contains("instance? %r1: any, <circle>"));
        assert!(dump.contains("as <circle>, %r1: any"));
        assert!(dump.contains("slot.read"));
    }

    #[test]
    fn lowers_booleans_comparisons_and_if_join() {
        let module = lower_source(
            "define function check()\n  #t\nend;\n\
             define function main()\n  if 10 < 20 then 1 else 2 end\nend;",
        );

        let dump = module.dump();
        assert!(dump.contains("fn l0 check() -> bool"));
        assert!(dump.contains("const.bool true"));
        assert!(dump.contains("i64.lt"));
        assert!(dump.contains("br "));
        assert!(dump.contains("jmp bb"));
        assert!(dump.contains(" = mov "));
    }

    #[test]
    fn lowers_signal_handle_and_restart_join() {
        let module = lower_source(
            "define class <bad> (<object>)\n  slot fallback :: <integer>;\nend;\n\
             define function main()\n  handle signal(make(<bad>, fallback: 42)) with restart e :: <bad> e.fallback end\nend;",
        );

        let dump = module.dump();
        assert!(dump.contains("make <bad>(%r"));
        assert!(dump.contains("jmp bb0"));
        assert!(dump.contains("local l0 e = %r"));
        assert!(dump.contains("slot.read %r"));
        assert!(dump.contains("<bad>.fallback"));
        assert!(dump.contains(" = mov "));
    }

    fn const_i64(function: &Function, reg: Register) -> Option<i64> {
        function.instructions.iter().find_map(|instr| match instr {
            Instr::ConstI64 { dst, value, .. } if *dst == reg => Some(*value),
            _ => None,
        })
    }
}
