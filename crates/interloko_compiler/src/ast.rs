//! Surface AST lowered from the milestone-0 CST.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::diagnostic::{Diagnostic, DiagnosticArg, DiagnosticCode};
use crate::grammar_frontend::{
    FrontendBinaryOp, FrontendExpression, FrontendForm, FrontendFormKind, StagedParseRegion,
};
use crate::identity::{AstNodeId, DefinitionId, DefinitionKind, SourceFileId};
use crate::syntax::{CstElement, CstKind, CstNode, Span, SyntaxTree, TokenKind};

/// AST module containing function definitions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Module {
    /// Source file identity.
    pub source_file: SourceFileId,
    /// Class definitions in source order.
    pub classes: Vec<Class>,
    /// Generic function declarations in source order.
    pub generics: Vec<Generic>,
    /// Method definitions in source order.
    pub methods: Vec<Method>,
    /// Function definitions in source order.
    pub functions: Vec<Function>,
    /// Immutable top-level constant definitions in source order.
    pub constants: Vec<Constant>,
    /// AST lowering diagnostics.
    pub diagnostics: Vec<Diagnostic>,
}

/// Function definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Function {
    /// Stable-ish AST node identity.
    pub id: AstNodeId,
    /// Stable definition identity.
    pub definition: DefinitionId,
    /// Function name.
    pub name: String,
    /// Parameters in source order.
    pub params: Vec<FunctionParam>,
    /// Optional declared return value types.
    pub return_type: Option<ResultAnnotation>,
    /// Function body.
    pub body: Block,
    /// Source span.
    pub span: Span,
}

/// Immutable top-level constant definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Constant {
    /// Stable-ish AST node identity.
    pub id: AstNodeId,
    /// Stable definition identity.
    pub definition: DefinitionId,
    /// Constant name, conventionally including a leading `$`.
    pub name: String,
    /// Optional value type annotation.
    pub type_annotation: Option<TypeAnnotation>,
    /// Initializer expression.
    pub value: Expr,
    /// Source span.
    pub span: Span,
}

/// Direct function parameter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionParam {
    /// Stable-ish AST node identity.
    pub id: AstNodeId,
    /// Parameter name.
    pub name: String,
    /// Optional declared parameter type.
    pub type_annotation: Option<TypeAnnotation>,
    /// Source span.
    pub span: Span,
}

/// Source type annotation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypeAnnotation {
    /// Type name, including angle brackets.
    pub name: String,
    /// Whether this annotation denotes the class object named by `name`.
    pub class_object: bool,
    /// Callable type signature when this annotation uses `callable(...) => ...`.
    pub callable: Option<CallableTypeAnnotation>,
    /// Source span.
    pub span: Span,
}

/// Source callable type annotation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallableTypeAnnotation {
    /// Callable parameter annotations in call order.
    pub params: Vec<TypeAnnotation>,
    /// Callable result annotations in return order.
    pub results: ResultAnnotation,
}

/// Source result annotation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResultAnnotation {
    /// Result value annotations in return order.
    pub values: Vec<TypeAnnotation>,
    /// Source span.
    pub span: Span,
}

/// Class definition with nominal slots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Class {
    /// Stable-ish AST node identity.
    pub id: AstNodeId,
    /// Stable definition identity.
    pub definition: DefinitionId,
    /// Class name, including Dylan-style angle brackets.
    pub name: String,
    /// Explicit superclass name, including Dylan-style angle brackets.
    ///
    /// `None` means the source omitted a superclass list; semantic lowering
    /// normalizes that to the `<object>` root.
    pub superclass: Option<String>,
    /// Source span for the explicit superclass name, or the class name span
    /// when no superclass was written.
    pub superclass_span: Span,
    /// Slot declarations in source order.
    pub slots: Vec<Slot>,
    /// Source span.
    pub span: Span,
}

/// Slot declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Slot {
    /// Stable-ish AST node identity.
    pub id: AstNodeId,
    /// Stable definition identity.
    pub definition: DefinitionId,
    /// Slot name.
    pub name: String,
    /// Optional declared slot type name.
    ///
    /// Missing types default to Dylan's top `<object>` value category during
    /// semantic lowering.
    pub type_name: Option<String>,
    /// Source span for the optional slot type, or the slot name span when the
    /// declaration is untyped.
    pub type_span: Span,
    /// Optional external construction keyword without the trailing colon.
    ///
    /// Missing keywords default to the slot name.
    pub init_keyword: Option<String>,
    /// Source span for the optional construction keyword, or the slot name span
    /// when the declaration uses the default keyword.
    pub init_keyword_span: Span,
    /// Whether the construction keyword was declared with
    /// `required-init-keyword:`.
    pub required_init_keyword: bool,
    /// Optional expression used when construction omits this slot.
    pub init_value: Option<Expr>,
    /// Slot allocation policy. Only instance slots are supported in this slice.
    pub allocation: SlotAllocation,
    /// Slot mutability metadata.
    pub mutability: SlotMutability,
    /// Source span.
    pub span: Span,
}

/// Slot allocation policy.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SlotAllocation {
    /// One value per constructed object instance.
    Instance,
}

/// Slot mutability metadata.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SlotMutability {
    /// Slot declared with ordinary `slot`.
    Mutable,
    /// Slot declared with `constant slot`.
    Constant,
}

/// Generic function declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Generic {
    /// Stable-ish AST node identity.
    pub id: AstNodeId,
    /// Stable definition identity.
    pub definition: DefinitionId,
    /// Generic function name.
    pub name: String,
    /// Parameters in source order.
    pub params: Vec<GenericParam>,
    /// Optional declared result value types.
    pub return_type: Option<ResultAnnotation>,
    /// Source span.
    pub span: Span,
}

/// Generic function parameter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenericParam {
    /// Stable-ish AST node identity.
    pub id: AstNodeId,
    /// Parameter name.
    pub name: String,
    /// Optional declared parameter type.
    pub type_annotation: Option<TypeAnnotation>,
    /// Source span.
    pub span: Span,
}

/// Method definition specialized on nominal classes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Method {
    /// Stable-ish AST node identity.
    pub id: AstNodeId,
    /// Stable definition identity.
    pub definition: DefinitionId,
    /// Generic function name this method implements.
    pub name: String,
    /// Specialized parameters in call order.
    pub params: Vec<MethodParam>,
    /// Optional declared method result value types.
    pub return_type: Option<ResultAnnotation>,
    /// Method body.
    pub body: Block,
    /// Source span.
    pub span: Span,
}

/// Method parameter with a nominal specializer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MethodParam {
    /// Stable-ish AST node identity.
    pub id: AstNodeId,
    /// Parameter name.
    pub name: String,
    /// Nominal class specializer.
    ///
    /// `<object>` is normalized by semantic lowering because the current
    /// semantic top value is not a nominal class id.
    pub class_name: String,
    /// Source span.
    pub span: Span,
}

/// Function body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Block {
    /// Stable-ish AST node identity.
    pub id: AstNodeId,
    /// Local `let` bindings.
    pub lets: Vec<Let>,
    /// Final result expression.
    pub result: Expr,
    /// Source span.
    pub span: Span,
}

/// Local binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Let {
    /// Stable-ish AST node identity.
    pub id: AstNodeId,
    /// Stable definition identity for the local binding.
    pub definition: DefinitionId,
    /// Local name.
    pub name: String,
    /// Binding pattern introduced by this let.
    pub pattern: LetPattern,
    /// Optional value type annotation.
    pub type_annotation: Option<TypeAnnotation>,
    /// Bound value expression.
    pub value: Expr,
    /// Source span.
    pub span: Span,
}

/// Local binding pattern.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LetPattern {
    /// One local binding.
    Name(LetBinding),
    /// Positional multiple-value destructuring.
    Tuple {
        /// Bound names in result order.
        bindings: Vec<LetBinding>,
        /// Source span.
        span: Span,
    },
}

/// One local introduced by a let binding pattern.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LetBinding {
    /// Stable definition identity for the local binding.
    pub definition: DefinitionId,
    /// Local name.
    pub name: String,
    /// Optional declared local type.
    pub type_annotation: Option<TypeAnnotation>,
    /// Source span of the binding name.
    pub span: Span,
}

/// Binary operator.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BinaryOp {
    /// Addition.
    Add,
    /// Subtraction.
    Sub,
    /// Multiplication.
    Mul,
}

/// Integer comparison operator.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CompareOp {
    /// Equality.
    Eq,
    /// Strictly less than.
    Lt,
    /// Less than or equal.
    Le,
    /// Strictly greater than.
    Gt,
    /// Greater than or equal.
    Ge,
}

/// Surface expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Expr {
    /// Integer literal.
    Number {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Parsed value.
        value: i64,
        /// Source span.
        span: Span,
    },
    /// Boolean literal.
    Bool {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Parsed value.
        value: bool,
        /// Source span.
        span: Span,
    },
    /// String literal.
    String {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Decoded string value.
        value: String,
        /// Source span.
        span: Span,
    },
    /// Symbol literal.
    Symbol {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Symbol name without the leading `#`.
        name: String,
        /// Source span.
        span: Span,
    },
    /// Immutable vector literal.
    Vector {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Ordered element expressions.
        elements: Vec<Expr>,
        /// Source span.
        span: Span,
    },
    /// Anonymous function expression.
    Function {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Function parameters.
        params: Vec<FunctionParam>,
        /// Optional declared return value types.
        return_type: Option<ResultAnnotation>,
        /// Function body.
        body: Box<Block>,
        /// Source span.
        span: Span,
    },
    /// Name reference.
    Name {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Referenced name.
        name: String,
        /// Source span.
        span: Span,
    },
    /// Class name expression.
    ClassName {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Referenced class name.
        class_name: String,
        /// Source span.
        span: Span,
    },
    /// Runtime class membership test.
    InstanceOf {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Tested value.
        value: Box<Expr>,
        /// Tested class name.
        class_name: String,
        /// Source span.
        span: Span,
    },
    /// Checked object cast/narrowing expression.
    Cast {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Target class name.
        class_name: String,
        /// Value to cast.
        value: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// Direct call syntax.
    Call {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Callee name.
        callee: String,
        /// Argument expressions.
        args: Vec<Expr>,
        /// Source span.
        span: Span,
    },
    /// Recoverable condition signal.
    Signal {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Condition object expression.
        condition: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// Object construction through `make(<class>, slot: value)`.
    MakeObject {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Class object expression.
        class: Box<Expr>,
        /// Keyword slot initializers.
        fields: Vec<KeywordArg>,
        /// Source span.
        span: Span,
    },
    /// Object slot read.
    SlotRead {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Receiver expression.
        receiver: Box<Expr>,
        /// Slot name.
        slot: String,
        /// Source span.
        span: Span,
    },
    /// Object slot assignment through `receiver.slot := value`.
    SlotAssign {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Receiver expression.
        receiver: Box<Expr>,
        /// Slot name.
        slot: String,
        /// Assigned value expression.
        value: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// Binary expression.
    Binary {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Operator.
        op: BinaryOp,
        /// Left operand.
        lhs: alloc::boxed::Box<Expr>,
        /// Right operand.
        rhs: alloc::boxed::Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// Prefix integer negation expression.
    UnaryMinus {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Negated expression.
        expr: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// Integer comparison expression.
    Compare {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Comparison operator.
        op: CompareOp,
        /// Left operand.
        lhs: Box<Expr>,
        /// Right operand.
        rhs: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// Conditional expression.
    If {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Boolean condition.
        condition: Box<Expr>,
        /// Expression evaluated when the condition is true.
        then_branch: Box<Expr>,
        /// Expression evaluated when the condition is false.
        else_branch: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// Recoverable condition handler expression.
    Handle {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Handled body expression.
        body: Box<Expr>,
        /// Restart clause.
        restart: Restart,
        /// Source span.
        span: Span,
    },
    /// Placeholder for malformed syntax.
    Error {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Source span.
        span: Span,
    },
}

impl Expr {
    /// Returns this expression's stable-ish AST node id.
    #[must_use]
    pub const fn id(&self) -> AstNodeId {
        match self {
            Self::Number { id, .. }
            | Self::Bool { id, .. }
            | Self::String { id, .. }
            | Self::Symbol { id, .. }
            | Self::Vector { id, .. }
            | Self::Function { id, .. }
            | Self::Name { id, .. }
            | Self::ClassName { id, .. }
            | Self::InstanceOf { id, .. }
            | Self::Cast { id, .. }
            | Self::Call { id, .. }
            | Self::Signal { id, .. }
            | Self::MakeObject { id, .. }
            | Self::SlotRead { id, .. }
            | Self::SlotAssign { id, .. }
            | Self::Binary { id, .. }
            | Self::UnaryMinus { id, .. }
            | Self::Compare { id, .. }
            | Self::If { id, .. }
            | Self::Handle { id, .. }
            | Self::Error { id, .. } => *id,
        }
    }

    /// Returns this expression's source span.
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::Number { span, .. }
            | Self::Bool { span, .. }
            | Self::String { span, .. }
            | Self::Symbol { span, .. }
            | Self::Vector { span, .. }
            | Self::Function { span, .. }
            | Self::Name { span, .. }
            | Self::ClassName { span, .. }
            | Self::InstanceOf { span, .. }
            | Self::Cast { span, .. }
            | Self::Call { span, .. }
            | Self::Signal { span, .. }
            | Self::MakeObject { span, .. }
            | Self::SlotRead { span, .. }
            | Self::SlotAssign { span, .. }
            | Self::Binary { span, .. }
            | Self::UnaryMinus { span, .. }
            | Self::Compare { span, .. }
            | Self::If { span, .. }
            | Self::Handle { span, .. }
            | Self::Error { span, .. } => *span,
        }
    }
}

/// Restart clause for a lexical condition handler.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Restart {
    /// Stable-ish AST node identity.
    pub id: AstNodeId,
    /// Optional handler-local name bound to the signaled condition object.
    pub condition_name: Option<String>,
    /// Source span for the optional condition binding name.
    pub condition_span: Span,
    /// Nominal condition class handled by this restart.
    pub class_name: String,
    /// Handler expression evaluated when a matching condition is signaled.
    pub body: Box<Expr>,
    /// Source span.
    pub span: Span,
}

/// Keyword argument used by object construction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeywordArg {
    /// Stable-ish AST node identity.
    pub id: AstNodeId,
    /// Keyword name.
    pub name: String,
    /// Argument value.
    pub value: Expr,
    /// Source span.
    pub span: Span,
}

/// Lowers a syntax tree to a surface AST.
#[must_use]
pub fn lower(tree: &SyntaxTree) -> Module {
    let mut lowering = Lowering {
        tree,
        diagnostics: tree.diagnostics.clone(),
    };
    lowering.lower_module()
}

/// Lowers syntax plus structured frontend forms emitted by staged grammar adapters.
///
/// The syntax tree remains the product of parsing the source file itself. The
/// staged forms are appended as compiler-generated AST definitions carrying
/// spans from their source regions. Materialized Loko for those forms is only a
/// debug projection and is not reparsed as a definition stream.
#[must_use]
pub fn lower_with_frontend_forms(
    tree: &SyntaxTree,
    staged_regions: &[StagedParseRegion],
) -> Module {
    let mut module = lower(tree);
    if staged_regions.is_empty() {
        return module;
    }

    let mut lowering = FrontendFormLowering {
        source_file: tree.source_file,
        diagnostics: Vec::new(),
    };
    for region in staged_regions {
        for (form_index, form) in region.forms.iter().enumerate() {
            lowering.lower_form(form, &region.object_slug, form_index, &mut module);
        }
    }
    module.diagnostics.extend(lowering.diagnostics);
    module
}

struct Lowering<'tree> {
    tree: &'tree SyntaxTree,
    diagnostics: Vec<Diagnostic>,
}

impl<'tree> Lowering<'tree> {
    fn lower_module(&mut self) -> Module {
        let classes = self
            .tree
            .root
            .child_nodes(CstKind::Class)
            .map(|node| self.lower_class(node))
            .collect();
        let generics = self
            .tree
            .root
            .child_nodes(CstKind::Generic)
            .map(|node| self.lower_generic(node))
            .collect();
        let methods = self
            .tree
            .root
            .child_nodes(CstKind::Method)
            .map(|node| self.lower_method(node))
            .collect();
        let functions = self
            .tree
            .root
            .child_nodes(CstKind::Function)
            .map(|node| self.lower_function(node))
            .collect();
        let constants = self
            .tree
            .root
            .child_nodes(CstKind::Constant)
            .map(|node| self.lower_constant(node))
            .collect();
        Module {
            source_file: self.tree.source_file,
            classes,
            generics,
            methods,
            functions,
            constants,
            diagnostics: self.diagnostics.clone(),
        }
    }

    fn lower_class(&mut self, node: &CstNode) -> Class {
        let type_names = self.token_texts(node, TokenKind::TypeName);
        let name_span = type_names.first().map_or(node.span, |(span, _)| *span);
        let name = type_names
            .first()
            .map_or_else(String::new, |(_, text)| (*text).to_string());
        let superclass = type_names.get(1).map(|(_, text)| (*text).to_string());
        let superclass_span = type_names.get(1).map_or(name_span, |(span, _)| *span);
        let mut slots = Vec::new();
        for slot in node.child_nodes(CstKind::SlotDecl) {
            slots.push(self.lower_slot(&name, slot));
        }
        Class {
            id: self.ast_id(node),
            definition: self.definition(DefinitionKind::Class, &name, ""),
            name,
            superclass,
            superclass_span,
            slots,
            span: node.span,
        }
    }

    fn lower_slot(&mut self, class_name: &str, node: &CstNode) -> Slot {
        let Some((span, text)) = self.first_token_text(node, TokenKind::Ident) else {
            return Slot {
                id: self.ast_id(node),
                definition: self.definition(DefinitionKind::Slot, "", class_name),
                name: String::new(),
                type_name: None,
                type_span: node.span,
                init_keyword: None,
                init_keyword_span: node.span,
                required_init_keyword: false,
                init_value: None,
                allocation: SlotAllocation::Instance,
                mutability: SlotMutability::Mutable,
                span: node.span,
            };
        };
        let name = text.to_string();
        let slot_type = self.first_token_text(node, TokenKind::TypeName);
        let mut init_keyword = None;
        let mut init_keyword_span = span;
        let mut required_init_keyword = false;
        let mut init_value = None;
        for option in node.child_nodes(CstKind::SlotOption) {
            let idents = self.ident_tokens(option);
            let Some((option_span, option_name)) = idents.first() else {
                continue;
            };
            let option_span = *option_span;
            let option_name = (*option_name).to_string();
            let slot_keyword = option
                .child_nodes(CstKind::SlotKeyword)
                .next()
                .and_then(|keyword| self.first_token_text(keyword, TokenKind::Ident));
            match option_name.as_str() {
                "init-keyword" => {
                    let Some((keyword_span, keyword_name)) = slot_keyword else {
                        self.diagnostics.push(
                            Diagnostic::error(
                                option_span,
                                DiagnosticCode::AstSlotOptionRequiresKeyword,
                            )
                            .with_arg(DiagnosticArg::text("option", format!("{option_name}:"))),
                        );
                        continue;
                    };
                    init_keyword = Some((*keyword_name).to_string());
                    init_keyword_span = keyword_span;
                }
                "required-init-keyword" => {
                    let Some((keyword_span, keyword_name)) = slot_keyword else {
                        self.diagnostics.push(
                            Diagnostic::error(
                                option_span,
                                DiagnosticCode::AstSlotOptionRequiresKeyword,
                            )
                            .with_arg(DiagnosticArg::text("option", format!("{option_name}:"))),
                        );
                        continue;
                    };
                    init_keyword = Some((*keyword_name).to_string());
                    init_keyword_span = keyword_span;
                    required_init_keyword = true;
                }
                "init-value" => {
                    let Some(expr) = option.children.iter().find_map(|child| match child {
                        CstElement::Node(expr) if !expr.kind.is_role() => {
                            Some(self.lower_expr(expr))
                        }
                        _ => None,
                    }) else {
                        self.diagnostics.push(
                            Diagnostic::error(
                                option_span,
                                DiagnosticCode::AstSlotInitValueRequiresExpression,
                            )
                            .with_arg(DiagnosticArg::text("option", "init-value:")),
                        );
                        continue;
                    };
                    init_value = Some(expr);
                }
                _ => self.diagnostics.push(
                    Diagnostic::error(option_span, DiagnosticCode::AstUnsupportedSlotOption)
                        .with_arg(DiagnosticArg::text("option", format!("{option_name}:"))),
                ),
            }
        }
        Slot {
            id: self.ast_id(node),
            definition: self.definition(DefinitionKind::Slot, &name, class_name),
            name,
            type_name: slot_type.map(|(_, text)| text.to_string()),
            type_span: slot_type.map_or(span, |(span, _)| span),
            init_keyword,
            init_keyword_span,
            required_init_keyword,
            init_value,
            allocation: SlotAllocation::Instance,
            mutability: if self.has_token(node, TokenKind::Constant) {
                SlotMutability::Constant
            } else {
                SlotMutability::Mutable
            },
            span,
        }
    }

    fn ident_tokens<'a>(&'a self, node: &'a CstNode) -> Vec<(Span, &'a str)> {
        node.children
            .iter()
            .filter_map(|child| match child {
                CstElement::Token(index) if self.tree.tokens[*index].kind == TokenKind::Ident => {
                    let token = self.tree.tokens[*index];
                    Some((token.span, token.text(&self.tree.source)))
                }
                _ => None,
            })
            .collect()
    }

    fn lower_generic(&mut self, node: &CstNode) -> Generic {
        let mut name = String::new();
        let mut saw_generic_keyword = false;
        let mut params = Vec::new();
        let mut return_type = None;

        for child in &node.children {
            match child {
                CstElement::Token(index) if self.tree.tokens[*index].kind == TokenKind::Generic => {
                    saw_generic_keyword = true;
                }
                CstElement::Token(index)
                    if saw_generic_keyword
                        && self.tree.tokens[*index].kind == TokenKind::Ident
                        && name.is_empty() =>
                {
                    name = self.tree.tokens[*index].text(&self.tree.source).to_string();
                }
                CstElement::Token(index) if self.tree.tokens[*index].kind == TokenKind::Arrow => {
                    return_type = self.result_annotation_after(node, *index);
                }
                CstElement::Node(child_node) if child_node.kind == CstKind::ParameterList => {
                    params = self.lower_generic_params(child_node);
                }
                _ => {}
            }
        }

        Generic {
            id: self.ast_id(node),
            definition: self.definition(DefinitionKind::Generic, &name, ""),
            name,
            params,
            return_type,
            span: node.span,
        }
    }

    fn lower_method(&mut self, node: &CstNode) -> Method {
        let mut return_type = None;
        let params = node
            .child_nodes(CstKind::ParameterList)
            .next()
            .map_or_else(Vec::new, |params| self.lower_method_params(params));
        for child in &node.children {
            if let CstElement::Token(index) = child
                && self.tree.tokens[*index].kind == TokenKind::Arrow
            {
                return_type = self.result_annotation_after(node, *index);
            }
        }
        for param in &params {
            if param.class_name.is_empty() {
                self.diagnostics.push(
                    Diagnostic::error(param.span, DiagnosticCode::AstInvalidMethodSpecializer)
                        .with_arg(DiagnosticArg::unsigned("actual_count", 0)),
                );
            }
        }
        let body = node
            .child_nodes(CstKind::Body)
            .next()
            .map(|body| self.lower_block(body))
            .unwrap_or_else(|| Block {
                id: self.ast_id(node),
                lets: Vec::new(),
                result: Expr::Error {
                    id: self.ast_id(node),
                    span: node.span,
                },
                span: node.span,
            });
        let name = self.definition_name_after(node, TokenKind::Method);
        let specializers = params
            .iter()
            .map(|param| param.class_name.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let definition = self.definition(DefinitionKind::Method, &name, &specializers);
        Method {
            id: self.ast_id(node),
            definition,
            name,
            params,
            return_type,
            body,
            span: node.span,
        }
    }

    fn lower_function(&mut self, node: &CstNode) -> Function {
        let mut name = String::new();
        let mut saw_function_keyword = false;
        let mut params = Vec::new();
        let mut return_type = None;
        let mut body = None;

        for child in &node.children {
            match child {
                CstElement::Token(index)
                    if self.tree.tokens[*index].kind == TokenKind::Function =>
                {
                    saw_function_keyword = true;
                }
                CstElement::Token(index)
                    if saw_function_keyword
                        && self.tree.tokens[*index].kind == TokenKind::Ident
                        && name.is_empty() =>
                {
                    name = self.tree.tokens[*index].text(&self.tree.source).to_string();
                }
                CstElement::Token(index) if self.tree.tokens[*index].kind == TokenKind::Arrow => {
                    return_type = self.result_annotation_after(node, *index);
                }
                CstElement::Token(_) => {}
                CstElement::Node(child_node) if child_node.kind == CstKind::ParameterList => {
                    params = self.lower_function_params(child_node);
                }
                CstElement::Node(child_node) if child_node.kind == CstKind::Body => {
                    body = Some(self.lower_block(child_node));
                }
                _ => {}
            }
        }

        let body = body.unwrap_or_else(|| Block {
            id: self.ast_id(node),
            lets: Vec::new(),
            result: Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            },
            span: node.span,
        });

        Function {
            id: self.ast_id(node),
            definition: self.definition(DefinitionKind::Function, &name, ""),
            name,
            params,
            return_type,
            body,
            span: node.span,
        }
    }

    fn lower_constant(&mut self, node: &CstNode) -> Constant {
        let name = self.definition_name_after(node, TokenKind::Constant);
        let type_annotation = node.children.iter().find_map(|child| match child {
            CstElement::Token(index) if self.tree.tokens[*index].kind == TokenKind::DoubleColon => {
                self.type_annotation_after(node, *index)
            }
            _ => None,
        });
        let value = node
            .children
            .iter()
            .find_map(|child| match child {
                CstElement::Node(expr) if !expr.kind.is_role() => Some(self.lower_expr(expr)),
                _ => None,
            })
            .unwrap_or_else(|| Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            });
        Constant {
            id: self.ast_id(node),
            definition: self.definition(DefinitionKind::Constant, &name, ""),
            name,
            type_annotation,
            value,
            span: node.span,
        }
    }

    fn lower_generic_params(&self, node: &CstNode) -> Vec<GenericParam> {
        let tokens = self.node_token_indices(node);
        let mut params = Vec::new();
        let mut index = 0;
        while index < tokens.len() {
            let token_index = tokens[index];
            let token = self.tree.tokens[token_index];
            if token.kind != TokenKind::Ident {
                index += 1;
                continue;
            }
            let name = token.text(&self.tree.source).to_string();
            let type_annotation = if tokens
                .get(index + 1)
                .is_some_and(|next| self.tree.tokens[*next].kind == TokenKind::DoubleColon)
            {
                let parsed = self.type_annotation_from_tokens(&tokens, index + 2);
                if let Some((annotation, next)) = parsed {
                    index = next;
                    Some(annotation)
                } else {
                    index += 1;
                    None
                }
            } else {
                index += 1;
                None
            };
            params.push(GenericParam {
                id: AstNodeId::child(node.id, 1, &name),
                name,
                type_annotation,
                span: token.span,
            });
        }
        params
    }

    fn lower_function_params(&self, node: &CstNode) -> Vec<FunctionParam> {
        let tokens = self.node_token_indices(node);
        let mut params = Vec::new();
        let mut index = 0;
        while index < tokens.len() {
            let token_index = tokens[index];
            let token = self.tree.tokens[token_index];
            if token.kind != TokenKind::Ident {
                index += 1;
                continue;
            }
            let name = token.text(&self.tree.source).to_string();
            let type_annotation = if tokens
                .get(index + 1)
                .is_some_and(|next| self.tree.tokens[*next].kind == TokenKind::DoubleColon)
            {
                let parsed = self.type_annotation_from_tokens(&tokens, index + 2);
                if let Some((annotation, next)) = parsed {
                    index = next;
                    Some(annotation)
                } else {
                    index += 1;
                    None
                }
            } else {
                index += 1;
                None
            };
            params.push(FunctionParam {
                id: AstNodeId::child(node.id, 1, &name),
                name,
                type_annotation,
                span: token.span,
            });
        }
        params
    }

    fn lower_method_params(&self, node: &CstNode) -> Vec<MethodParam> {
        let mut params = Vec::new();
        let mut index = 0;
        while index < node.children.len() {
            let CstElement::Token(name_index) = node.children[index] else {
                index += 1;
                continue;
            };
            if self.tree.tokens[name_index].kind != TokenKind::Ident {
                index += 1;
                continue;
            }
            let name = self.tree.tokens[name_index]
                .text(&self.tree.source)
                .to_string();
            let span = self.tree.tokens[name_index].span;
            let class_name = node.children[index + 1..]
                .iter()
                .take_while(|child| {
                    !matches!(
                        child,
                        CstElement::Token(token)
                            if self.tree.tokens[*token].kind == TokenKind::Comma
                    )
                })
                .find_map(|child| match child {
                    CstElement::Token(type_index)
                        if self.tree.tokens[*type_index].kind == TokenKind::TypeName =>
                    {
                        Some(
                            self.tree.tokens[*type_index]
                                .text(&self.tree.source)
                                .to_string(),
                        )
                    }
                    _ => None,
                })
                .unwrap_or_default();
            params.push(MethodParam {
                id: AstNodeId::child(node.id, 2, &name),
                name,
                class_name,
                span,
            });
            index += 1;
        }
        params
    }

    fn lower_block(&mut self, node: &CstNode) -> Block {
        let mut lets = Vec::new();
        let mut result = None;

        for child in &node.children {
            let CstElement::Node(child_node) = child else {
                continue;
            };
            if child_node.kind == CstKind::LetStmt {
                lets.push(self.lower_let(child_node));
            } else {
                result = Some(self.lower_expr(child_node));
            }
        }

        if result.is_none() {
            self.diagnostics.push(Diagnostic::error(
                node.span,
                DiagnosticCode::SyntaxExpectedExpression,
            ));
        }

        Block {
            id: self.ast_id(node),
            lets,
            result: result.unwrap_or(Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            }),
            span: node.span,
        }
    }

    fn lower_let(&mut self, node: &CstNode) -> Let {
        let pattern = self.lower_let_pattern(node);
        let name = let_pattern_name(&pattern);
        let definition = let_pattern_definition(&pattern);
        let mut value = None;

        for child in &node.children {
            if let CstElement::Node(child_node) = child
                && !child_node.kind.is_role()
            {
                value = Some(self.lower_expr(child_node));
            }
        }

        Let {
            id: self.ast_id(node),
            definition,
            name,
            type_annotation: let_pattern_scalar_annotation(&pattern),
            pattern,
            value: value.unwrap_or(Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            }),
            span: node.span,
        }
    }

    fn lower_let_pattern(&self, node: &CstNode) -> LetPattern {
        let tokens = self.node_token_indices(node);
        let Some(let_position) = tokens
            .iter()
            .position(|index| self.tree.tokens[*index].kind == TokenKind::Let)
        else {
            return LetPattern::Name(LetBinding {
                definition: self.definition(DefinitionKind::Local, "", ""),
                name: String::new(),
                type_annotation: None,
                span: node.span,
            });
        };
        let Some(first_index) = tokens.get(let_position + 1).copied() else {
            return LetPattern::Name(LetBinding {
                definition: self.definition(DefinitionKind::Local, "", ""),
                name: String::new(),
                type_annotation: None,
                span: node.span,
            });
        };
        if self.tree.tokens[first_index].kind != TokenKind::LParen {
            let name = self.tree.tokens[first_index]
                .text(&self.tree.source)
                .to_string();
            let type_annotation = tokens
                .get(let_position + 2)
                .filter(|token| self.tree.tokens[**token].kind == TokenKind::DoubleColon)
                .and_then(|token| self.type_annotation_after(node, *token));
            return LetPattern::Name(LetBinding {
                definition: self.definition(DefinitionKind::Local, &name, ""),
                name,
                type_annotation,
                span: self.tree.tokens[first_index].span,
            });
        }

        let mut bindings = Vec::new();
        let mut index = let_position + 2;
        while index < tokens.len() {
            let token_index = tokens[index];
            let token = self.tree.tokens[token_index];
            match token.kind {
                TokenKind::Ident => {
                    let name = token.text(&self.tree.source).to_string();
                    let type_annotation = if tokens.get(index + 1).is_some_and(|token| {
                        self.tree.tokens[*token].kind == TokenKind::DoubleColon
                    }) {
                        let parsed = self.type_annotation_from_tokens(&tokens, index + 2);
                        if let Some((annotation, next)) = parsed {
                            index = next;
                            Some(annotation)
                        } else {
                            index += 1;
                            None
                        }
                    } else {
                        index += 1;
                        None
                    };
                    let binding_index = bindings.len();
                    bindings.push(LetBinding {
                        definition: self.definition(
                            DefinitionKind::Local,
                            &name,
                            &format!("tuple-let:{}:{binding_index}", node.id.raw()),
                        ),
                        name,
                        type_annotation,
                        span: token.span,
                    });
                    while index < tokens.len()
                        && !matches!(
                            self.tree.tokens[tokens[index]].kind,
                            TokenKind::Comma | TokenKind::RParen | TokenKind::Equal
                        )
                    {
                        index += 1;
                    }
                }
                TokenKind::RParen | TokenKind::Equal | TokenKind::DoubleColon => break,
                _ => {
                    index += 1;
                }
            }
        }

        LetPattern::Tuple {
            bindings,
            span: node.span,
        }
    }

    fn lower_expr(&mut self, node: &CstNode) -> Expr {
        match node.kind {
            CstKind::NumberExpr => self.lower_number(node),
            CstKind::BoolExpr => self.lower_bool(node),
            CstKind::StringExpr => self.lower_string(node),
            CstKind::SymbolExpr => self.lower_symbol(node),
            CstKind::KeywordExpr => self.lower_keyword_value(node),
            CstKind::VectorExpr => self.lower_vector(node),
            CstKind::FunctionExpr => self.lower_function_expr(node),
            CstKind::NameExpr => self.lower_name(node),
            CstKind::TypeExpr => self.lower_class_name(node),
            CstKind::CallExpr => self.lower_call(node),
            CstKind::SlotExpr => self.lower_slot_read(node),
            CstKind::SlotAssignExpr => self.lower_slot_assign(node),
            CstKind::UnaryExpr => self.lower_unary(node),
            CstKind::BinaryExpr => self.lower_binary(node),
            CstKind::IfExpr => self.lower_if(node),
            CstKind::HandleExpr => self.lower_handle(node),
            _ => Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            },
        }
    }

    fn lower_function_expr(&mut self, node: &CstNode) -> Expr {
        let mut params = Vec::new();
        let mut return_type = None;
        let mut body = None;

        for child in &node.children {
            match child {
                CstElement::Token(index) if self.tree.tokens[*index].kind == TokenKind::Arrow => {
                    return_type = self.result_annotation_after(node, *index);
                }
                CstElement::Node(child_node) if child_node.kind == CstKind::ParameterList => {
                    params = self.lower_function_params(child_node);
                }
                CstElement::Node(child_node) if child_node.kind == CstKind::Body => {
                    body = Some(self.lower_block(child_node));
                }
                _ => {}
            }
        }

        let body = body.unwrap_or_else(|| Block {
            id: self.ast_id(node),
            lets: Vec::new(),
            result: Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            },
            span: node.span,
        });

        Expr::Function {
            id: self.ast_id(node),
            params,
            return_type,
            body: Box::new(body),
            span: node.span,
        }
    }

    fn lower_unary(&mut self, node: &CstNode) -> Expr {
        let Some(child) = node.children.iter().find_map(|child| match child {
            CstElement::Node(child) => Some(child),
            CstElement::Token(_) => None,
        }) else {
            return Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            };
        };
        if child.kind == CstKind::NumberExpr {
            return self.lower_negative_number(node, child);
        }
        Expr::UnaryMinus {
            id: self.ast_id(node),
            expr: Box::new(self.lower_expr(child)),
            span: node.span,
        }
    }

    fn lower_negative_number(&mut self, unary: &CstNode, number: &CstNode) -> Expr {
        let Some((_, text)) = self.first_token_text(number, TokenKind::Number) else {
            return Expr::Error {
                id: self.ast_id(unary),
                span: unary.span,
            };
        };
        const I64_MIN_MAGNITUDE: u64 = 1_u64 << 63;
        let value = match text.parse::<u64>() {
            Ok(I64_MIN_MAGNITUDE) => i64::MIN,
            Ok(magnitude) if magnitude <= i64::MAX as u64 => -(magnitude as i64),
            _ => {
                self.diagnostics.push(Diagnostic::error(
                    unary.span,
                    DiagnosticCode::AstIntegerLiteralOutOfRange,
                ));
                return Expr::Error {
                    id: self.ast_id(unary),
                    span: unary.span,
                };
            }
        };
        Expr::Number {
            id: self.ast_id(unary),
            value,
            span: unary.span,
        }
    }

    fn lower_bool(&mut self, node: &CstNode) -> Expr {
        let Some((span, text)) = self.first_token_text(node, TokenKind::Bool) else {
            return Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            };
        };
        Expr::Bool {
            id: self.ast_id(node),
            value: text == "#t",
            span,
        }
    }

    fn lower_number(&mut self, node: &CstNode) -> Expr {
        let Some((span, text)) = self.first_token_text(node, TokenKind::Number) else {
            return Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            };
        };
        match text.parse::<i64>() {
            Ok(value) => Expr::Number {
                id: self.ast_id(node),
                value,
                span,
            },
            Err(_) => {
                self.diagnostics.push(Diagnostic::error(
                    span,
                    DiagnosticCode::AstIntegerLiteralOutOfRange,
                ));
                Expr::Error {
                    id: self.ast_id(node),
                    span,
                }
            }
        }
    }

    fn lower_string(&mut self, node: &CstNode) -> Expr {
        let Some((span, text)) = self.first_token_text(node, TokenKind::String) else {
            return Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            };
        };
        match decode_string_literal(text) {
            Some(value) => Expr::String {
                id: self.ast_id(node),
                value,
                span,
            },
            None => {
                self.diagnostics.push(Diagnostic::error(
                    span,
                    DiagnosticCode::AstMalformedStringLiteral,
                ));
                Expr::Error {
                    id: self.ast_id(node),
                    span,
                }
            }
        }
    }

    fn lower_symbol(&self, node: &CstNode) -> Expr {
        let symbol = self
            .first_token_text(node, TokenKind::Symbol)
            .map(|(span, text)| (span, text[1..].to_string()))
            .or_else(|| {
                self.first_token_text(node, TokenKind::Ident)
                    .map(|(span, text)| (span, text.to_string()))
            });
        let Some((span, name)) = symbol else {
            return Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            };
        };
        Expr::Symbol {
            id: self.ast_id(node),
            name,
            span,
        }
    }

    fn lower_keyword_value(&self, node: &CstNode) -> Expr {
        let symbol = self
            .first_token_text(node, TokenKind::Keyword)
            .map(|(span, text)| (span, text[2..].to_string()))
            .or_else(|| {
                self.first_token_text(node, TokenKind::Ident)
                    .map(|(span, text)| (span, text.to_string()))
            });
        let Some((span, name)) = symbol else {
            return Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            };
        };
        Expr::Symbol {
            id: self.ast_id(node),
            name,
            span,
        }
    }

    fn lower_vector(&mut self, node: &CstNode) -> Expr {
        let elements = node
            .children
            .iter()
            .filter_map(|child| match child {
                CstElement::Node(expr) => Some(self.lower_expr(expr)),
                CstElement::Token(_) => None,
            })
            .collect();
        Expr::Vector {
            id: self.ast_id(node),
            elements,
            span: node.span,
        }
    }

    fn lower_name(&mut self, node: &CstNode) -> Expr {
        let Some((span, text)) = self.first_token_text(node, TokenKind::Ident) else {
            self.diagnostics.push(Diagnostic::error(
                node.span,
                DiagnosticCode::SyntaxExpectedExpression,
            ));
            return Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            };
        };
        Expr::Name {
            id: self.ast_id(node),
            name: text.to_string(),
            span,
        }
    }

    fn lower_class_name(&self, node: &CstNode) -> Expr {
        let Some((span, text)) = self.first_token_text(node, TokenKind::TypeName) else {
            return Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            };
        };
        Expr::ClassName {
            id: self.ast_id(node),
            class_name: text.to_string(),
            span,
        }
    }

    fn lower_call(&mut self, node: &CstNode) -> Expr {
        let Some((name_span, text)) = self.first_token_text(node, TokenKind::Ident) else {
            return Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            };
        };
        if text == "make" {
            return self.lower_make_object(node);
        }
        if text == "signal" {
            return self.lower_signal(node);
        }
        if text == "instance?" {
            return self.lower_instance_of(node);
        }
        if text == "as" {
            return self.lower_cast(node);
        }
        let mut args = Vec::new();
        for arg_list in node.child_nodes(CstKind::ArgList) {
            for child in &arg_list.children {
                if let CstElement::Node(expr) = child {
                    if matches!(expr.kind, CstKind::KeywordArg) {
                        continue;
                    }
                    args.push(self.lower_expr(expr));
                }
            }
        }
        Expr::Call {
            id: self.ast_id(node),
            callee: text.to_string(),
            args,
            span: node.span.cover(name_span),
        }
    }

    fn lower_instance_of(&mut self, node: &CstNode) -> Expr {
        let args = self.lower_regular_args(node);
        if args.len() != 2 {
            self.diagnostics.push(
                Diagnostic::error(node.span, DiagnosticCode::AstInvalidInstanceOf)
                    .with_arg(DiagnosticArg::unsigned("expected_count", 2))
                    .with_arg(DiagnosticArg::unsigned(
                        "actual_count",
                        u64::try_from(args.len()).unwrap_or(u64::MAX),
                    )),
            );
            return Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            };
        }
        let mut args = args.into_iter();
        let value = args.next().expect("arity was checked");
        let class = args.next().expect("arity was checked");
        let Expr::ClassName { class_name, .. } = class else {
            self.diagnostics.push(Diagnostic::error(
                node.span,
                DiagnosticCode::AstInvalidInstanceOf,
            ));
            return Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            };
        };
        Expr::InstanceOf {
            id: self.ast_id(node),
            value: Box::new(value),
            class_name,
            span: node.span,
        }
    }

    fn lower_cast(&mut self, node: &CstNode) -> Expr {
        let args = self.lower_regular_args(node);
        if args.len() != 2 {
            self.diagnostics.push(
                Diagnostic::error(node.span, DiagnosticCode::AstInvalidCast)
                    .with_arg(DiagnosticArg::unsigned("expected_count", 2))
                    .with_arg(DiagnosticArg::unsigned(
                        "actual_count",
                        u64::try_from(args.len()).unwrap_or(u64::MAX),
                    )),
            );
            return Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            };
        }
        let mut args = args.into_iter();
        let class = args.next().expect("arity was checked");
        let value = args.next().expect("arity was checked");
        let Expr::ClassName { class_name, .. } = class else {
            self.diagnostics
                .push(Diagnostic::error(node.span, DiagnosticCode::AstInvalidCast));
            return Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            };
        };
        Expr::Cast {
            id: self.ast_id(node),
            class_name,
            value: Box::new(value),
            span: node.span,
        }
    }

    fn lower_signal(&mut self, node: &CstNode) -> Expr {
        let args = self.lower_regular_args(node);
        if args.len() != 1 {
            self.diagnostics.push(
                Diagnostic::error(node.span, DiagnosticCode::AstInvalidSignal)
                    .with_arg(DiagnosticArg::unsigned("expected_count", 1))
                    .with_arg(DiagnosticArg::unsigned(
                        "actual_count",
                        u64::try_from(args.len()).unwrap_or(u64::MAX),
                    )),
            );
            return Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            };
        }
        Expr::Signal {
            id: self.ast_id(node),
            condition: Box::new(args.into_iter().next().expect("arity was checked")),
            span: node.span,
        }
    }

    fn lower_regular_args(&mut self, node: &CstNode) -> Vec<Expr> {
        let mut args = Vec::new();
        for arg_list in node.child_nodes(CstKind::ArgList) {
            for child in &arg_list.children {
                if let CstElement::Node(expr) = child {
                    if matches!(expr.kind, CstKind::KeywordArg) {
                        continue;
                    }
                    args.push(self.lower_expr(expr));
                }
            }
        }
        args
    }

    fn lower_make_object(&mut self, node: &CstNode) -> Expr {
        let Some(arg_list) = node.child_nodes(CstKind::ArgList).next() else {
            return Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            };
        };
        let class = arg_list
            .children
            .iter()
            .find_map(|child| match child {
                CstElement::Node(expr) if expr.kind != CstKind::KeywordArg => {
                    Some(self.lower_expr(expr))
                }
                _ => None,
            })
            .unwrap_or(Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            });
        let fields = arg_list
            .child_nodes(CstKind::KeywordArg)
            .map(|keyword| self.lower_keyword_arg(keyword))
            .collect();
        Expr::MakeObject {
            id: self.ast_id(node),
            class: Box::new(class),
            fields,
            span: node.span,
        }
    }

    fn lower_keyword_arg(&mut self, node: &CstNode) -> KeywordArg {
        let keyword_node = node
            .child_nodes(CstKind::ArgumentKeyword)
            .next()
            .unwrap_or(node);
        let Some((name_span, name)) = self.first_token_text(keyword_node, TokenKind::Ident) else {
            return KeywordArg {
                id: self.ast_id(node),
                name: String::new(),
                value: Expr::Error {
                    id: self.ast_id(node),
                    span: node.span,
                },
                span: node.span,
            };
        };
        let value = node
            .children
            .iter()
            .find_map(|child| match child {
                CstElement::Node(expr) if !expr.kind.is_role() => Some(self.lower_expr(expr)),
                _ => None,
            })
            .unwrap_or(Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            });
        KeywordArg {
            id: self.ast_id(node),
            name: name.to_string(),
            value,
            span: node.span.cover(name_span),
        }
    }

    fn lower_slot_read(&mut self, node: &CstNode) -> Expr {
        let receiver = node.children.iter().find_map(|child| match child {
            CstElement::Node(expr) if !expr.kind.is_role() => Some(self.lower_expr(expr)),
            _ => None,
        });
        let slot = node.children.iter().rev().find_map(|child| match child {
            CstElement::Token(index) if self.tree.tokens[*index].kind == TokenKind::Ident => {
                Some(self.tree.tokens[*index].text(&self.tree.source).to_string())
            }
            _ => None,
        });
        Expr::SlotRead {
            id: self.ast_id(node),
            receiver: Box::new(receiver.unwrap_or(Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            })),
            slot: slot.unwrap_or_default(),
            span: node.span,
        }
    }

    fn lower_slot_assign(&mut self, node: &CstNode) -> Expr {
        let mut child_nodes = node.children.iter().filter_map(|child| match child {
            CstElement::Node(expr) => Some(expr),
            CstElement::Token(_) => None,
        });
        let target = child_nodes
            .next()
            .map(|expr| self.lower_expr(expr))
            .unwrap_or(Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            });
        let value = child_nodes
            .next()
            .map(|expr| self.lower_expr(expr))
            .unwrap_or(Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            });
        let Expr::SlotRead { receiver, slot, .. } = target else {
            self.diagnostics.push(Diagnostic::error(
                node.span,
                DiagnosticCode::AstInvalidSlotAssignment,
            ));
            return Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            };
        };
        Expr::SlotAssign {
            id: self.ast_id(node),
            receiver,
            slot,
            value: Box::new(value),
            span: node.span,
        }
    }

    fn lower_binary(&mut self, node: &CstNode) -> Expr {
        let mut exprs = Vec::new();
        for child in &node.children {
            if let CstElement::Node(expr) = child {
                exprs.push(self.lower_expr(expr));
            }
        }
        let mut exprs = exprs.into_iter();
        let fallback = || Expr::Error {
            id: self.ast_id(node),
            span: node.span,
        };
        let lhs = exprs.next().unwrap_or_else(fallback);
        let rhs = exprs.next().unwrap_or_else(fallback);
        let op = node.children.iter().find_map(|child| match child {
            CstElement::Token(index) => Some(self.tree.tokens[*index].kind),
            _ => None,
        });

        match op {
            Some(TokenKind::Plus) => Expr::Binary {
                id: self.ast_id(node),
                op: BinaryOp::Add,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span: node.span,
            },
            Some(TokenKind::Minus) => Expr::Binary {
                id: self.ast_id(node),
                op: BinaryOp::Sub,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span: node.span,
            },
            Some(TokenKind::Star) => Expr::Binary {
                id: self.ast_id(node),
                op: BinaryOp::Mul,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span: node.span,
            },
            Some(TokenKind::Equal) => Expr::Compare {
                id: self.ast_id(node),
                op: CompareOp::Eq,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span: node.span,
            },
            Some(TokenKind::Less) => Expr::Compare {
                id: self.ast_id(node),
                op: CompareOp::Lt,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span: node.span,
            },
            Some(TokenKind::LessEqual) => Expr::Compare {
                id: self.ast_id(node),
                op: CompareOp::Le,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span: node.span,
            },
            Some(TokenKind::Greater) => Expr::Compare {
                id: self.ast_id(node),
                op: CompareOp::Gt,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span: node.span,
            },
            Some(TokenKind::GreaterEqual) => Expr::Compare {
                id: self.ast_id(node),
                op: CompareOp::Ge,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span: node.span,
            },
            _ => Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            },
        }
    }

    fn lower_if(&mut self, node: &CstNode) -> Expr {
        let mut exprs = Vec::new();
        for child in &node.children {
            if let CstElement::Node(expr) = child {
                exprs.push(self.lower_expr(expr));
            }
        }
        let mut exprs = exprs.into_iter();
        let condition = exprs.next().unwrap_or_else(|| Expr::Error {
            id: self.ast_id(node),
            span: node.span,
        });
        let then_branch = exprs.next().unwrap_or_else(|| Expr::Error {
            id: self.ast_id(node),
            span: node.span,
        });
        let else_branch = exprs.next().unwrap_or_else(|| Expr::Error {
            id: self.ast_id(node),
            span: node.span,
        });
        Expr::If {
            id: self.ast_id(node),
            condition: Box::new(condition),
            then_branch: Box::new(then_branch),
            else_branch: Box::new(else_branch),
            span: node.span,
        }
    }

    fn lower_handle(&mut self, node: &CstNode) -> Expr {
        let mut body = None;
        let mut restart = None;
        for child in &node.children {
            let CstElement::Node(child_node) = child else {
                continue;
            };
            if child_node.kind == CstKind::RestartClause {
                restart = Some(self.lower_restart(child_node));
            } else {
                body = Some(self.lower_expr(child_node));
            }
        }
        Expr::Handle {
            id: self.ast_id(node),
            body: Box::new(body.unwrap_or(Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            })),
            restart: restart.unwrap_or_else(|| Restart {
                id: self.ast_id(node),
                condition_name: None,
                condition_span: node.span,
                class_name: String::new(),
                body: Box::new(Expr::Error {
                    id: self.ast_id(node),
                    span: node.span,
                }),
                span: node.span,
            }),
            span: node.span,
        }
    }

    fn lower_restart(&mut self, node: &CstNode) -> Restart {
        let condition = self.first_restart_condition_name(node);
        let class_name = self
            .first_token_text(node, TokenKind::TypeName)
            .map_or_else(String::new, |(_, text)| text.to_string());
        let body = node
            .children
            .iter()
            .find_map(|child| match child {
                CstElement::Node(expr) if !expr.kind.is_role() => Some(self.lower_expr(expr)),
                _ => None,
            })
            .unwrap_or(Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            });
        Restart {
            id: self.ast_id(node),
            condition_name: condition.map(|(_, text)| text.to_string()),
            condition_span: condition.map_or(node.span, |(span, _)| span),
            class_name,
            body: Box::new(body),
            span: node.span,
        }
    }

    fn ast_id(&self, node: &CstNode) -> AstNodeId {
        AstNodeId::from_syntax(node.id)
    }

    fn definition(&self, kind: DefinitionKind, name: &str, discriminator: &str) -> DefinitionId {
        DefinitionId::new(self.tree.source_file, kind, name, discriminator)
    }

    fn first_token_text(&self, node: &CstNode, kind: TokenKind) -> Option<(Span, &'tree str)> {
        node.children.iter().find_map(|child| match child {
            CstElement::Token(index) if self.tree.tokens[*index].kind == kind => {
                let token = self.tree.tokens[*index];
                Some((token.span, token.text(&self.tree.source)))
            }
            _ => None,
        })
    }

    fn token_texts(&self, node: &CstNode, kind: TokenKind) -> Vec<(Span, &'tree str)> {
        node.children
            .iter()
            .filter_map(|child| match child {
                CstElement::Token(index) if self.tree.tokens[*index].kind == kind => {
                    let token = self.tree.tokens[*index];
                    Some((token.span, token.text(&self.tree.source)))
                }
                _ => None,
            })
            .collect()
    }

    fn has_token(&self, node: &CstNode, kind: TokenKind) -> bool {
        node.children.iter().any(|child| match child {
            CstElement::Token(index) => self.tree.tokens[*index].kind == kind,
            CstElement::Node(_) => false,
        })
    }

    fn node_token_indices(&self, node: &CstNode) -> Vec<usize> {
        node.children
            .iter()
            .filter_map(|child| match child {
                CstElement::Token(index) => Some(*index),
                CstElement::Node(_) => None,
            })
            .collect()
    }

    fn type_annotation_after(&self, node: &CstNode, marker_index: usize) -> Option<TypeAnnotation> {
        let tokens = self.node_token_indices(node);
        let marker = tokens.iter().position(|index| *index == marker_index)?;
        self.type_annotation_from_tokens(&tokens, marker + 1)
            .map(|(annotation, _)| annotation)
    }

    fn result_annotation_after(
        &self,
        node: &CstNode,
        marker_index: usize,
    ) -> Option<ResultAnnotation> {
        let tokens = self.node_token_indices(node);
        let marker = tokens.iter().position(|index| *index == marker_index)?;
        self.result_annotation_from_tokens(&tokens, marker + 1)
            .map(|(annotation, _)| annotation)
    }

    fn result_annotation_from_tokens(
        &self,
        tokens: &[usize],
        start: usize,
    ) -> Option<(ResultAnnotation, usize)> {
        if tokens
            .get(start)
            .is_some_and(|token| self.tree.tokens[*token].kind == TokenKind::LParen)
        {
            let mut values = Vec::new();
            let mut index = start + 1;
            while index < tokens.len() {
                match self.tree.tokens[tokens[index]].kind {
                    TokenKind::RParen => break,
                    TokenKind::Comma => {
                        index += 1;
                    }
                    _ => {
                        if let Some((annotation, next)) =
                            self.type_annotation_from_tokens(tokens, index)
                        {
                            values.push(annotation);
                            index = next;
                        } else {
                            index += 1;
                        }
                    }
                }
            }
            let end = tokens
                .get(index)
                .copied()
                .unwrap_or_else(|| *tokens.get(start).unwrap_or(&tokens[start]));
            return Some((
                ResultAnnotation {
                    values,
                    span: self.tree.tokens[tokens[start]]
                        .span
                        .cover(self.tree.tokens[end].span),
                },
                index.saturating_add(1),
            ));
        }
        self.type_annotation_from_tokens(tokens, start)
            .map(|(annotation, next)| {
                (
                    ResultAnnotation {
                        span: annotation.span,
                        values: vec![annotation],
                    },
                    next,
                )
            })
    }

    fn type_annotation_from_tokens(
        &self,
        tokens: &[usize],
        mut index: usize,
    ) -> Option<(TypeAnnotation, usize)> {
        let start = index;
        if tokens.get(index).is_some_and(|token| {
            let token = self.tree.tokens[*token];
            token.kind == TokenKind::Ident && token.text(&self.tree.source) == "callable"
        }) {
            let callable_token = self.tree.tokens[tokens[index]];
            if self.tree.tokens[tokens.get(index + 1).copied()?].kind != TokenKind::LParen {
                return None;
            }
            index += 2;
            let mut params = Vec::new();
            while index < tokens.len() {
                match self.tree.tokens[tokens[index]].kind {
                    TokenKind::RParen => break,
                    TokenKind::Comma => {
                        index += 1;
                    }
                    _ => {
                        let (annotation, next) = self.type_annotation_from_tokens(tokens, index)?;
                        params.push(annotation);
                        index = next;
                    }
                }
            }
            let rparen_index = tokens.get(index).copied()?;
            if self.tree.tokens[rparen_index].kind != TokenKind::RParen {
                return None;
            }
            let arrow_index = tokens.get(index + 1).copied()?;
            if self.tree.tokens[arrow_index].kind != TokenKind::Arrow {
                return None;
            }
            let (results, next) = self.result_annotation_from_tokens(tokens, index + 2)?;
            let span = callable_token.span.cover(results.span);
            return Some((
                TypeAnnotation {
                    name: "<callable>".into(),
                    class_object: false,
                    callable: Some(CallableTypeAnnotation { params, results }),
                    span,
                },
                next,
            ));
        }
        let class_object = tokens
            .get(index)
            .is_some_and(|token| self.tree.tokens[*token].kind == TokenKind::Class);
        if class_object {
            index += 1;
        }
        let type_index = tokens.get(index).copied()?;
        let type_token = self.tree.tokens[type_index];
        if type_token.kind != TokenKind::TypeName {
            return None;
        }
        let span = if class_object {
            self.tree.tokens[tokens[start]].span.cover(type_token.span)
        } else {
            type_token.span
        };
        Some(TypeAnnotation {
            name: type_token.text(&self.tree.source).to_string(),
            class_object,
            callable: None,
            span,
        })
        .map(|annotation| (annotation, index + 1))
    }

    fn first_restart_condition_name(&self, node: &CstNode) -> Option<(Span, &'tree str)> {
        let mut tokens = node.children.iter().filter_map(|child| match child {
            CstElement::Token(index) => Some(*index),
            CstElement::Node(_) => None,
        });
        while let Some(index) = tokens.next() {
            let token = self.tree.tokens[index];
            if token.kind != TokenKind::Restart {
                continue;
            }
            let name_index = tokens.next()?;
            let name = self.tree.tokens[name_index];
            if name.kind != TokenKind::Ident {
                return None;
            }
            let double_colon_index = tokens.next()?;
            if self.tree.tokens[double_colon_index].kind != TokenKind::DoubleColon {
                return None;
            }
            return Some((name.span, name.text(&self.tree.source)));
        }
        None
    }

    fn definition_name_after(&self, node: &CstNode, keyword: TokenKind) -> String {
        let mut saw_keyword = false;
        for child in &node.children {
            let CstElement::Token(index) = child else {
                continue;
            };
            let token = self.tree.tokens[*index];
            if token.kind == keyword {
                saw_keyword = true;
            } else if saw_keyword && token.kind == TokenKind::Ident {
                return token.text(&self.tree.source).to_string();
            }
        }
        String::new()
    }
}

fn let_pattern_name(pattern: &LetPattern) -> String {
    match pattern {
        LetPattern::Name(binding) => binding.name.clone(),
        LetPattern::Tuple { bindings, .. } => {
            let names = bindings
                .iter()
                .map(|binding| binding.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            format!("({names})")
        }
    }
}

fn let_pattern_definition(pattern: &LetPattern) -> DefinitionId {
    match pattern {
        LetPattern::Name(binding) => binding.definition,
        LetPattern::Tuple { bindings, .. } => bindings.first().map_or_else(
            || {
                DefinitionId::new(
                    SourceFileId::ANONYMOUS,
                    DefinitionKind::Local,
                    "",
                    "tuple-let",
                )
            },
            |binding| binding.definition,
        ),
    }
}

fn let_pattern_scalar_annotation(pattern: &LetPattern) -> Option<TypeAnnotation> {
    match pattern {
        LetPattern::Name(binding) => binding.type_annotation.clone(),
        LetPattern::Tuple { .. } => None,
    }
}

const GENERATED_FUNCTION_TAG: u64 = 0x1000;
const GENERATED_BLOCK_TAG: u64 = 0x1001;
const GENERATED_CLASS_TAG: u64 = 0x1002;
const GENERATED_SLOT_TAG: u64 = 0x1003;
const GENERATED_EXPR_TAG: u64 = 0x1100;

struct FrontendFormLowering {
    source_file: SourceFileId,
    diagnostics: Vec<Diagnostic>,
}

impl FrontendFormLowering {
    fn lower_form(
        &mut self,
        form: &FrontendForm,
        region_slug: &str,
        form_index: usize,
        module: &mut Module,
    ) {
        match &form.kind {
            FrontendFormKind::Function(function) => module.functions.push(Function {
                id: self.generated_id(
                    GENERATED_FUNCTION_TAG,
                    &format!("{region_slug}:f{form_index}:{}", function.name),
                    form.source_span,
                ),
                definition: self.definition(DefinitionKind::Function, &function.name, ""),
                name: function.name.clone(),
                params: Vec::new(),
                return_type: None,
                body: self.lower_function_body(form, region_slug, form_index),
                span: form.source_span,
            }),
            FrontendFormKind::Class(class) => module.classes.push(Class {
                id: self.generated_id(
                    GENERATED_CLASS_TAG,
                    &format!("{region_slug}:f{form_index}:{}", class.name),
                    form.source_span,
                ),
                definition: self.definition(DefinitionKind::Class, &class.name, ""),
                name: class.name.clone(),
                superclass: Some(class.superclass.clone()),
                superclass_span: form.source_span,
                slots: class
                    .slots
                    .iter()
                    .enumerate()
                    .map(|(slot_index, slot)| {
                        self.lower_slot_form(slot, &class.name, region_slug, form_index, slot_index)
                    })
                    .collect(),
                span: form.source_span,
            }),
        }
    }

    fn lower_function_body(
        &mut self,
        form: &FrontendForm,
        region_slug: &str,
        form_index: usize,
    ) -> Block {
        let FrontendFormKind::Function(function) = &form.kind else {
            return Block {
                id: self.generated_id(
                    GENERATED_BLOCK_TAG,
                    &format!("{region_slug}:f{form_index}:invalid-body"),
                    form.source_span,
                ),
                lets: Vec::new(),
                result: Expr::Error {
                    id: self.generated_id(
                        GENERATED_EXPR_TAG,
                        &format!("{region_slug}:f{form_index}:invalid-expression"),
                        form.source_span,
                    ),
                    span: form.source_span,
                },
                span: form.source_span,
            };
        };
        let result = self.lower_frontend_expression(&function.body);
        Block {
            id: self.generated_id(
                GENERATED_BLOCK_TAG,
                &format!("{region_slug}:f{form_index}:body"),
                form.source_span,
            ),
            lets: Vec::new(),
            result,
            span: form.source_span,
        }
    }

    fn lower_slot_form(
        &self,
        slot: &crate::grammar_frontend::FrontendSlot,
        class_name: &str,
        region_slug: &str,
        form_index: usize,
        slot_index: usize,
    ) -> Slot {
        Slot {
            id: self.generated_id(
                GENERATED_SLOT_TAG,
                &format!("{region_slug}:f{form_index}:s{slot_index}:{}", slot.name),
                slot.source_span,
            ),
            definition: self.definition(DefinitionKind::Slot, &slot.name, class_name),
            name: slot.name.clone(),
            type_name: Some(slot.type_name.clone()),
            type_span: slot.source_span,
            init_keyword: slot.init_keyword.clone(),
            init_keyword_span: slot.source_span,
            required_init_keyword: slot.required_init_keyword,
            init_value: None,
            allocation: SlotAllocation::Instance,
            mutability: if slot.constant {
                SlotMutability::Constant
            } else {
                SlotMutability::Mutable
            },
            span: slot.source_span,
        }
    }

    fn lower_frontend_expression(&mut self, expression: &FrontendExpression) -> Expr {
        match expression {
            FrontendExpression::Integer { value, source_span } => Expr::Number {
                id: self.expr_id("frontend-integer", *source_span),
                value: *value,
                span: *source_span,
            },
            FrontendExpression::Name { name, source_span } => Expr::Name {
                id: self.expr_id("frontend-name", *source_span),
                name: name.clone(),
                span: *source_span,
            },
            FrontendExpression::Call {
                callee,
                args,
                source_span,
            } => Expr::Call {
                id: self.expr_id("frontend-call", *source_span),
                callee: callee.clone(),
                args: args
                    .iter()
                    .map(|arg| self.lower_frontend_expression(arg))
                    .collect(),
                span: *source_span,
            },
            FrontendExpression::Binary {
                op,
                lhs,
                rhs,
                source_span,
            } => Expr::Binary {
                id: self.expr_id("frontend-binary", *source_span),
                op: match op {
                    FrontendBinaryOp::Add => BinaryOp::Add,
                    FrontendBinaryOp::Sub => BinaryOp::Sub,
                    FrontendBinaryOp::Mul => BinaryOp::Mul,
                },
                lhs: Box::new(self.lower_frontend_expression(lhs)),
                rhs: Box::new(self.lower_frontend_expression(rhs)),
                span: *source_span,
            },
        }
    }

    fn generated_id(&self, tag: u64, name: &str, span: Span) -> AstNodeId {
        AstNodeId::generated(self.source_file, tag, name, span.start, span.end)
    }

    fn expr_id(&self, name: &str, span: Span) -> AstNodeId {
        self.generated_id(GENERATED_EXPR_TAG, name, span)
    }

    fn definition(&self, kind: DefinitionKind, name: &str, discriminator: &str) -> DefinitionId {
        DefinitionId::new(self.source_file, kind, name, discriminator)
    }
}

fn decode_string_literal(text: &str) -> Option<String> {
    if !text.starts_with('"') || !text.ends_with('"') || text.len() < 2 {
        return None;
    }
    let body = &text[1..text.len() - 1];
    let mut decoded = String::new();
    let mut chars = body.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            decoded.push(ch);
            continue;
        }
        match chars.next()? {
            'n' => decoded.push('\n'),
            't' => decoded.push('\t'),
            '"' => decoded.push('"'),
            '\\' => decoded.push('\\'),
            _ => return None,
        }
    }
    Some(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar_frontend;
    use crate::identity::SourceFileId;
    use crate::syntax;

    fn parse(source: &str) -> syntax::SyntaxTree {
        grammar_frontend::parse_file(SourceFileId::ANONYMOUS, source).syntax
    }

    fn parse_file(source_file: SourceFileId, source: &str) -> syntax::SyntaxTree {
        grammar_frontend::parse_file(source_file, source).syntax
    }

    #[test]
    fn lowers_function_params_lets_and_binary_result() {
        let tree =
            parse("define function add-one(x)\n  let one :: <integer> = 1;\n  x + one\nend;");
        let module = lower(&tree);
        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(module.functions.len(), 1);
        assert_eq!(module.functions[0].name, "add-one");
        assert_eq!(module.functions[0].params[0].name, "x");
        assert_eq!(module.functions[0].params[0].type_annotation, None);
        assert_eq!(module.functions[0].body.lets[0].name, "one");
        assert_eq!(
            module.functions[0].body.lets[0]
                .type_annotation
                .as_ref()
                .map(|annotation| annotation.name.as_str()),
            Some("<integer>")
        );
        assert!(matches!(
            module.functions[0].body.result,
            Expr::Binary {
                op: BinaryOp::Add,
                ..
            }
        ));
    }

    #[test]
    fn lowers_function_param_and_return_annotations() {
        let tree = parse(
            "define function build(klass :: class <circle>, radius :: <integer>) => <circle>\n  make(klass, radius: radius)\nend;",
        );
        let module = lower(&tree);
        assert_eq!(module.diagnostics, Vec::new());
        let function = &module.functions[0];
        assert_eq!(function.params[0].name, "klass");
        assert_eq!(
            function.params[0]
                .type_annotation
                .as_ref()
                .map(|annotation| (annotation.class_object, annotation.name.as_str())),
            Some((true, "<circle>"))
        );
        assert_eq!(function.params[1].name, "radius");
        assert_eq!(
            function.params[1]
                .type_annotation
                .as_ref()
                .map(|annotation| (annotation.class_object, annotation.name.as_str())),
            Some((false, "<integer>"))
        );
        assert_eq!(
            function
                .return_type
                .as_ref()
                .and_then(|annotation| annotation.values.first())
                .map(|annotation| (annotation.class_object, annotation.name.as_str())),
            Some((false, "<circle>"))
        );
    }

    #[test]
    fn lowers_constant_annotations() {
        let tree = parse("define constant $answer :: <integer> = 42;");
        let module = lower(&tree);
        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(module.constants[0].name, "$answer");
        assert_eq!(
            module.constants[0]
                .type_annotation
                .as_ref()
                .map(|annotation| annotation.name.as_str()),
            Some("<integer>")
        );
    }

    #[test]
    fn lowers_call_arguments() {
        let tree = parse("define function main()\n  add(40, 2)\nend;");
        let module = lower(&tree);
        let Expr::Call { callee, args, .. } = &module.functions[0].body.result else {
            panic!("expected call");
        };
        assert_eq!(callee, "add");
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn lowers_staged_frontend_expressions_without_text_reparse() {
        let source = "// grammar-activate: calculator\n\n\
                      // staged-loko: calculator\n\
                      helper = 40\n\
                      // end-staged-loko\n\n\
                      // staged-loko: calculator\n\
                      main = helper() + 2\n\
                      // end-staged-loko\n";
        let report = grammar_frontend::parse_file(SourceFileId::from_path("staged.loko"), source);
        assert_eq!(report.diagnostics, Vec::new());
        let module = lower_with_frontend_forms(&report.syntax, &report.staged_regions);
        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(module.functions.len(), 2);

        let main = module
            .functions
            .iter()
            .find(|function| function.name == "main")
            .expect("staged main function should be lowered");
        let Expr::Binary {
            op: BinaryOp::Add,
            lhs,
            rhs,
            ..
        } = &main.body.result
        else {
            panic!("expected structural binary expression");
        };
        assert!(matches!(
            lhs.as_ref(),
            Expr::Call { callee, args, .. } if callee == "helper" && args.is_empty()
        ));
        assert!(matches!(rhs.as_ref(), Expr::Number { value: 2, .. }));
    }

    #[test]
    fn lowers_instance_tests_and_casts() {
        let tree = parse(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function main(x :: <object>) => <integer>\n  if instance?(x, <circle>) then as(<circle>, x).radius else 0 end\nend;",
        );
        let module = lower(&tree);
        assert_eq!(module.diagnostics, Vec::new());
        let Expr::If {
            condition,
            then_branch,
            ..
        } = &module.functions[0].body.result
        else {
            panic!("expected if");
        };
        assert!(matches!(
            condition.as_ref(),
            Expr::InstanceOf { class_name, .. } if class_name == "<circle>"
        ));
        assert!(matches!(
            then_branch.as_ref(),
            Expr::SlotRead { receiver, .. }
                if matches!(receiver.as_ref(), Expr::Cast { class_name, .. } if class_name == "<circle>")
        ));
    }

    #[test]
    fn lowers_classes_generics_methods_make_and_slot_reads() {
        let tree = parse(
            "define class <circle> (<object>)\n  slot radius :: <integer>, required-init-keyword: radius:;\n  slot valid? :: <boolean>, init-keyword: enabled?:;\n  constant slot cached :: <integer>, init-value: 5;\nend;\n\
             define generic area(x :: <object>) => <integer>;\n\
             define method area(c :: <circle>) => <integer>\n  c.radius * c.radius\nend;\n\
             define function main()\n  area(make(<circle>, radius: 10, enabled?: #t))\nend;",
        );
        let module = lower(&tree);
        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(module.classes[0].name, "<circle>");
        assert_eq!(module.classes[0].superclass.as_deref(), Some("<object>"));
        assert_eq!(module.classes[0].slots[0].name, "radius");
        assert_eq!(
            module.classes[0].slots[0].type_name.as_deref(),
            Some("<integer>")
        );
        assert_eq!(
            module.classes[0].slots[0].init_keyword.as_deref(),
            Some("radius")
        );
        assert!(module.classes[0].slots[0].required_init_keyword);
        assert_eq!(
            module.classes[0].slots[0].allocation,
            SlotAllocation::Instance
        );
        assert_eq!(
            module.classes[0].slots[0].mutability,
            SlotMutability::Mutable
        );
        assert!(module.classes[0].slots[0].init_value.is_none());
        assert_eq!(module.classes[0].slots[1].name, "valid?");
        assert_eq!(
            module.classes[0].slots[1].type_name.as_deref(),
            Some("<boolean>")
        );
        assert_eq!(
            module.classes[0].slots[1].init_keyword.as_deref(),
            Some("enabled?")
        );
        assert!(!module.classes[0].slots[1].required_init_keyword);
        assert_eq!(module.classes[0].slots[2].name, "cached");
        assert_eq!(
            module.classes[0].slots[2].mutability,
            SlotMutability::Constant
        );
        assert!(matches!(
            module.classes[0].slots[2].init_value,
            Some(Expr::Number { value: 5, .. })
        ));
        assert_eq!(module.generics[0].name, "area");
        assert_eq!(module.generics[0].params[0].name, "x");
        assert_eq!(
            module.generics[0].params[0]
                .type_annotation
                .as_ref()
                .map(|annotation| (annotation.class_object, annotation.name.as_str())),
            Some((false, "<object>"))
        );
        assert_eq!(
            module.generics[0]
                .return_type
                .as_ref()
                .and_then(|annotation| annotation.values.first())
                .map(|annotation| (annotation.class_object, annotation.name.as_str())),
            Some((false, "<integer>"))
        );
        assert_eq!(module.methods[0].name, "area");
        assert_eq!(module.methods[0].params[0].class_name, "<circle>");
        assert_eq!(
            module.methods[0]
                .return_type
                .as_ref()
                .and_then(|annotation| annotation.values.first())
                .map(|annotation| (annotation.class_object, annotation.name.as_str())),
            Some((false, "<integer>"))
        );
        assert!(matches!(
            module.methods[0].body.result,
            Expr::Binary { lhs: _, rhs: _, .. }
        ));
        let Expr::Call { args, .. } = &module.functions[0].body.result else {
            panic!("expected area call");
        };
        assert!(matches!(
            &args[0],
            Expr::MakeObject { class, .. }
                if matches!(class.as_ref(), Expr::ClassName { class_name, .. } if class_name == "<circle>")
        ));
    }

    #[test]
    fn lowers_booleans_comparisons_and_if() {
        let tree = parse("define function main(x)\n  if x < 0 then #f else x >= 10 end\nend;");
        let module = lower(&tree);

        assert_eq!(module.diagnostics, Vec::new());
        let Expr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } = &module.functions[0].body.result
        else {
            panic!("expected if expression");
        };
        assert!(matches!(
            condition.as_ref(),
            Expr::Compare {
                op: CompareOp::Lt,
                ..
            }
        ));
        assert!(matches!(
            then_branch.as_ref(),
            Expr::Bool { value: false, .. }
        ));
        assert!(matches!(
            else_branch.as_ref(),
            Expr::Compare {
                op: CompareOp::Ge,
                ..
            }
        ));
    }

    #[test]
    fn lowers_signal_handle_and_restart() {
        let tree = parse(
            "define function main()\n  handle signal(make(<bad>, fallback: 0)) with restart e :: <bad> e.fallback end\nend;",
        );
        let module = lower(&tree);

        assert_eq!(module.diagnostics, Vec::new());
        let Expr::Handle {
            body,
            restart,
            span: _,
            ..
        } = &module.functions[0].body.result
        else {
            panic!("expected handle expression");
        };
        assert_eq!(restart.class_name, "<bad>");
        assert_eq!(restart.condition_name.as_deref(), Some("e"));
        assert!(matches!(
            restart.body.as_ref(),
            Expr::SlotRead { receiver, slot, .. }
                if slot == "fallback" && matches!(receiver.as_ref(), Expr::Name { name, .. } if name == "e")
        ));
        assert!(matches!(
            body.as_ref(),
            Expr::Signal { condition, .. }
                if matches!(
                    condition.as_ref(),
                    Expr::MakeObject { class, .. }
                        if matches!(class.as_ref(), Expr::ClassName { class_name, .. } if class_name == "<bad>")
                )
        ));
    }

    #[test]
    fn syntax_and_ast_definition_ids_survive_whitespace_edits() {
        let source_file = SourceFileId::from_path("examples/main.loko");
        let before_tree = parse_file(source_file, "define function main()\n  40 + 2\nend;");
        let edited_tree = parse_file(
            source_file,
            "\n// comment before the same definition\n\
             define   function   main(  )\n  40   +   2\nend;",
        );
        let before_syntax = before_tree
            .root
            .child_nodes(syntax::CstKind::Function)
            .next()
            .expect("function syntax should exist")
            .id;
        let edited_syntax = edited_tree
            .root
            .child_nodes(syntax::CstKind::Function)
            .next()
            .expect("edited function syntax should exist")
            .id;
        assert_eq!(before_syntax, edited_syntax);

        let before = lower(&before_tree);
        let edited = lower(&edited_tree);
        assert_eq!(before.source_file, source_file);
        assert_eq!(before.functions[0].id, edited.functions[0].id);
        assert_eq!(
            before.functions[0].definition,
            edited.functions[0].definition
        );
    }
}
