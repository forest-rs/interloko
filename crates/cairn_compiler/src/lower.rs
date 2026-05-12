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

use crate::diagnostic::Diagnostic;
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
    /// Signed integer value.
    I64,
    /// Unsigned integer value used by runtime metadata.
    U64,
    /// Boolean value used by control-flow conditions.
    Bool,
    /// Runtime object, optionally tied to a statically known nominal class.
    Object(Option<sem::ClassId>),
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
    /// A source `define method`.
    Method(sem::MethodId),
    /// A generated runtime dispatcher for a generic function.
    Dispatcher(sem::GenericFunctionId),
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
    /// Unsigned integer constant.
    ConstU64 {
        /// Destination register.
        dst: Register,
        /// Constant value.
        value: u64,
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
    /// Runtime object construction with fields already ordered by slot id.
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
    /// Runtime integer slot read.
    ReadSlot {
        /// Destination register.
        dst: Register,
        /// Receiver object register.
        receiver: Register,
        /// Statically known receiver class.
        class: sem::ClassId,
        /// Slot to read.
        slot: sem::SlotId,
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
        body.lower_block(&function.body)?;
        Ok(Function {
            id: self.lower_function_id(function.id)?,
            name: function.name.clone(),
            kind: FunctionKind::Function(function.id),
            params: body.params,
            ret: value_kind(function.result_kind)?,
            locals: body.locals,
            instructions: body.instructions,
            span: function.span,
        })
    }

    fn lower_method(&self, method: &sem::Method) -> Result<Function, LowerError> {
        let mut body = BodyLowering::for_method(self.module, &self.ids, method)?;
        body.lower_block(&method.body)?;
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
            ret: value_kind(method.result_kind)?,
            locals: body.locals,
            instructions: body.instructions,
            span: method.span,
        })
    }

    fn lower_dispatcher(&self, generic: &sem::GenericFunction) -> Result<Function, LowerError> {
        if generic.arity != 1 {
            return Err(invalid_semantic(
                "only one-argument generics can be dispatched",
            ));
        }

        let mut body = BodyLowering::for_dispatcher(self.module, &self.ids);
        body.lower_dispatcher(generic)?;
        Ok(Function {
            id: self.lower_generic_id(generic.id)?,
            name: format!("{}#dispatch", generic.name),
            kind: FunctionKind::Dispatcher(generic.id),
            params: body.params,
            ret: ValueKind::I64,
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
        this.bind_param(
            method.locals.get(method.param.0 as usize),
            method.param,
            Register(1),
        )?;
        this.next_reg = 2;
        Ok(this)
    }

    fn for_dispatcher(module: &'module sem::Module, ids: &'module LowerIds) -> Self {
        Self {
            module,
            ids,
            source_locals: &[],
            params: vec![Param {
                source: None,
                name: "object".into(),
                kind: ValueKind::Object(None),
                reg: Register(1),
            }],
            locals: Vec::new(),
            instructions: Vec::new(),
            local_regs: Vec::new(),
            active_restarts: Vec::new(),
            next_reg: 2,
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

    fn lower_block(&mut self, block: &sem::Block) -> Result<(), LowerError> {
        for let_binding in &block.lets {
            let value = self.lower_expr(&let_binding.value)?;
            self.set_local(let_binding.local, value)?;
            let local = self
                .current_local(let_binding.local)
                .ok_or_else(|| invalid_semantic("local id is out of range"))?;
            self.locals.push(Local {
                source: let_binding.local,
                name: local.name.clone(),
                kind: value.kind,
                reg: value.reg,
            });
        }

        let result = self.lower_expr(&block.result)?;
        self.instructions.push(Instr::Return {
            value: result.reg,
            span: block.result.span(),
        });
        Ok(())
    }

    fn lower_dispatcher(&mut self, generic: &sem::GenericFunction) -> Result<(), LowerError> {
        let object = Register(1);
        let class_reg = self.alloc(ValueKind::U64);
        self.instructions.push(Instr::ClassOf {
            dst: class_reg.reg,
            object,
            span: generic.span,
        });

        for method in self
            .module
            .methods
            .iter()
            .filter(|method| method.generic == generic.id)
        {
            let class_const = self.alloc(ValueKind::U64);
            let cond = self.alloc(ValueKind::Bool);
            let result = self.alloc(ValueKind::I64);
            let matched = self.alloc_label();
            let next = self.alloc_label();
            self.instructions.push(Instr::ConstU64 {
                dst: class_const.reg,
                value: u64::from(method.specializer.0),
                span: method.span,
            });
            self.instructions.push(Instr::EqU64 {
                dst: cond.reg,
                lhs: class_reg.reg,
                rhs: class_const.reg,
                span: method.span,
            });
            self.instructions.push(Instr::Branch {
                cond: cond.reg,
                then_label: matched,
                else_label: next,
                span: method.span,
            });
            self.instructions.push(Instr::Label { label: matched });
            self.instructions.push(Instr::Call {
                dst: result.reg,
                callee: self
                    .ids
                    .methods
                    .get(method.id.0 as usize)
                    .copied()
                    .ok_or_else(|| invalid_semantic("method id is out of range"))?,
                args: vec![object],
                span: method.span,
            });
            self.instructions.push(Instr::Return {
                value: result.reg,
                span: method.span,
            });
            self.instructions.push(Instr::Label { label: next });
        }

        self.instructions.push(Instr::Trap {
            code: 1,
            span: generic.span,
        });
        Ok(())
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
            sem::Expr::Local { local, .. } => self
                .local_regs
                .get(local.0 as usize)
                .and_then(|value| *value)
                .ok_or_else(|| invalid_semantic("local was read before assignment")),
            sem::Expr::DirectCall {
                function,
                args,
                span,
            } => {
                let args = self.lower_args(args)?;
                let function_info = self
                    .module
                    .functions
                    .get(function.0 as usize)
                    .ok_or_else(|| invalid_semantic("function id is out of range"))?;
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
                let args = self.lower_args(args)?;
                let result = self.alloc(ValueKind::I64);
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
            sem::Expr::MakeObject {
                class,
                fields,
                span,
            } => {
                let class_info = self
                    .module
                    .classes
                    .get(class.0 as usize)
                    .ok_or_else(|| invalid_semantic("class id is out of range"))?;
                let mut field_regs = Vec::with_capacity(class_info.slots.len());
                for slot in &class_info.slots {
                    let Some(field) = fields.iter().find(|field| field.slot == slot.id) else {
                        return Err(invalid_semantic("missing object field"));
                    };
                    field_regs.push(self.lower_expr(&field.value)?.reg);
                }
                let result = self.alloc(ValueKind::Object(Some(*class)));
                self.instructions.push(Instr::MakeObject {
                    dst: result.reg,
                    class: *class,
                    fields: field_regs,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::SlotRead {
                receiver,
                class,
                slot,
                span,
            } => {
                let receiver = self.lower_expr(receiver)?;
                let result = self.alloc(ValueKind::I64);
                self.instructions.push(Instr::ReadSlot {
                    dst: result.reg,
                    receiver: receiver.reg,
                    class: *class,
                    slot: *slot,
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
            sem::Expr::If {
                condition,
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
        if lowered_class != condition_class || condition_class != active.specializer {
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

    fn lower_args(&mut self, args: &[sem::Expr]) -> Result<Vec<Register>, LowerError> {
        let mut lowered = Vec::with_capacity(args.len());
        for arg in args {
            lowered.push(self.lower_expr(arg)?.reg);
        }
        Ok(lowered)
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
        sem::ValueKind::I64 => Ok(ValueKind::I64),
        sem::ValueKind::Bool => Ok(ValueKind::Bool),
        sem::ValueKind::Object(class) => Ok(ValueKind::Object(Some(class))),
        sem::ValueKind::Error => Err(invalid_semantic("semantic value kind is invalid")),
    }
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
            FunctionKind::Method(id) => writeln!(f, " ; source method {}", id.0),
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
            Instr::ConstU64 { dst, value, span } => {
                write!(f, "  {dst} = const.u64 {value}")?;
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
            Instr::ReadSlot {
                dst,
                receiver,
                class,
                slot,
                span,
            } => {
                write!(f, "  {dst} = slot.read {receiver}, ")?;
                self.fmt_slot(f, *class, *slot)?;
                fmt_span(f, *span)
            }
            Instr::ClassOf { dst, object, span } => {
                write!(f, "  {dst} = class_of {object}")?;
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
            ValueKind::I64 => write!(f, "i64"),
            ValueKind::U64 => write!(f, "u64"),
            ValueKind::Bool => write!(f, "bool"),
            ValueKind::Object(Some(class)) => {
                write!(f, "object(")?;
                self.fmt_class(f, class)?;
                write!(f, ")")
            }
            ValueKind::Object(None) => write!(f, "object"),
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
            write!(f, "{slot_info}")
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
    fn lowers_generic_dispatch_object_construction_and_slots() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
             define function main()\n  area(make(<circle>, radius: 10))\nend;",
        );
        assert_eq!(module.functions.len(), 3);
        assert_eq!(module.main, Some(FunctionId(0)));
        let dump = module.dump();
        assert!(dump.contains("make <circle>(%r1)"));
        assert!(dump.contains("slot.read %r1, <circle>.radius"));
        assert!(dump.contains("fn l2 area#dispatch(%r1 object: object) -> i64"));
        assert!(dump.contains("class_of %r1"));
        assert!(dump.contains("br %r4, bb0, bb1"));
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
            "define class <bad> (<object>)\n  slot fallback;\nend;\n\
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
}
