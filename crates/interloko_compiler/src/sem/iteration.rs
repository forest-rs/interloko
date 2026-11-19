//! The iteration protocol: Dylan's `forward-iteration-protocol`, which
//! `for (x in c)` uses for a collection whose class typing does not know.
//!
//! The generic answers seven values for a collection: an initial state, a
//! limit, and the callables `next-state(c, s)`, `finished-state?(c, s,
//! limit)`, `current-key(c, s)` and `current-element(c, s)`, then
//! `current-element-setter(v, c, s)` or `#f` for a constant collection. As
//! Dylan's, minus `copy-state`: every state is an immutable value (an
//! index, a table entry position), so holding one copies it.
//!
//! It is a built-in open generic like `size` and `element`: a program adds
//! methods for its own classes, and the kernel's methods serve `<vector>`
//! (so `<stretchy-vector>`), `<string>`, `<table>` and, as a default,
//! `<sequence>`,
//! whose integer states run from 0 below `size(c)` and whose elements are
//! read with `element`. The generic is sealed over `<vector>`, `<table>`
//! and `<sequence>` itself, so a program's `<sequence>` subclass may define
//! its own method for its instances but no program replaces the default.
//!
//! The kernel's methods are written here as Loko syntax trees and lowered
//! like any method body, so their callables are ordinary closures. They
//! call the kernel's operations by names a program cannot write (`%size`),
//! so no definition of the program's can change what they do.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::ast::{
    BinaryOp, Block, CompareOp, CondArm, Expr, FunctionParam, Let, LetBinding, LetPattern,
    ResultAnnotation, Statement, TypeExpr,
};
use crate::identity::{AstNodeId, DefinitionId, DefinitionKind, SourceFileId};
use crate::syntax::Span;

use super::BuiltinValueClass;

/// The name of the iteration protocol generic.
pub const FORWARD_ITERATION_PROTOCOL: &str = "forward-iteration-protocol";

/// The kernel classes with an iteration method of their own, over which the
/// generic is sealed.
pub const SEALED_ITERATION_CLASSES: &[BuiltinValueClass] = &[
    BuiltinValueClass::Vector,
    BuiltinValueClass::String,
    BuiltinValueClass::Table,
];

/// The kernel classes with an iteration method, in method order: the
/// sealed ones, then the `<sequence>` default.
pub const ITERATION_CLASSES: &[BuiltinValueClass] = &[
    BuiltinValueClass::Vector,
    BuiltinValueClass::String,
    BuiltinValueClass::Table,
    BuiltinValueClass::Sequence,
];

/// Builds syntax trees for one kernel method, numbering their nodes so
/// each has its own identity.
pub(super) struct KernelTree {
    source_file: SourceFileId,
    class: &'static str,
    next: usize,
}

impl KernelTree {
    pub(super) const fn new(source_file: SourceFileId, class: &'static str) -> Self {
        Self {
            source_file,
            class,
            next: 0,
        }
    }

    fn id(&mut self) -> AstNodeId {
        self.next += 1;
        AstNodeId::generated(
            self.source_file,
            0x6974_6572,
            &alloc::format!("forward-iteration-protocol:{}", self.class),
            self.next,
            self.next,
        )
    }

    fn span() -> Span {
        Span::default()
    }

    fn name(&mut self, name: &str) -> Expr {
        Expr::Name {
            id: self.id(),
            name: name.into(),
            span: Self::span(),
        }
    }

    fn int(&mut self, value: i64) -> Expr {
        Expr::Number {
            id: self.id(),
            value,
            span: Self::span(),
        }
    }

    fn boolean(&mut self, value: bool) -> Expr {
        Expr::Bool {
            id: self.id(),
            value,
            span: Self::span(),
        }
    }

    fn call(&mut self, callee: &str, args: Vec<Expr>) -> Expr {
        Expr::Call {
            id: self.id(),
            callee: callee.into(),
            args,
            keywords: Vec::new(),
            span: Self::span(),
        }
    }

    fn cast(&mut self, class: &str, value: Expr) -> Expr {
        Expr::Cast {
            id: self.id(),
            target: named(class),
            value: Box::new(value),
            span: Self::span(),
        }
    }

    /// `as(<integer>, name)`.
    fn integer(&mut self, name: &str) -> Expr {
        let value = self.name(name);
        self.cast("<integer>", value)
    }

    fn block(&mut self, statements: Vec<Statement>, result: Expr) -> Block {
        Block {
            id: self.id(),
            statements,
            result,
            span: Self::span(),
        }
    }

    /// `method (params :: <object>) => result body end`.
    fn function(&mut self, params: &[&str], result: &str, body: Expr) -> Expr {
        let params = params
            .iter()
            .map(|name| FunctionParam {
                id: self.id(),
                name: (*name).into(),
                type_annotation: Some(named("<object>")),
                span: Self::span(),
            })
            .collect();
        let body = self.block(Vec::new(), body);
        Expr::Function {
            id: self.id(),
            params,
            return_type: Some(ResultAnnotation {
                values: vec![named(result)],
                span: Self::span(),
            }),
            body: Box::new(body),
            span: Self::span(),
        }
    }

    /// `let name :: type = value;`, local to the method `definition`.
    fn let_statement(
        &mut self,
        definition: DefinitionId,
        name: &str,
        annotation: Option<&str>,
        value: Expr,
    ) -> Statement {
        let binding = LetBinding {
            definition: DefinitionId::new(
                self.source_file,
                DefinitionKind::Local,
                name,
                &definition.raw().to_string(),
            ),
            name: name.into(),
            type_annotation: annotation.map(named),
            span: Self::span(),
        };
        Statement::Let(Box::new(Let {
            id: self.id(),
            definition: binding.definition,
            name: name.into(),
            type_annotation: annotation.map(named),
            pattern: LetPattern::Name(binding),
            value,
            span: Self::span(),
        }))
    }

    /// `as(<integer>, s) + 1`.
    fn next_index(&mut self) -> Expr {
        let state = self.integer("s");
        let one = self.int(1);
        Expr::Binary {
            id: self.id(),
            op: BinaryOp::Add,
            lhs: Box::new(state),
            rhs: Box::new(one),
            span: Self::span(),
        }
    }

    fn compare(&mut self, op: CompareOp, lhs: Expr, rhs: Expr) -> Expr {
        Expr::Compare {
            id: self.id(),
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
            span: Self::span(),
        }
    }

    /// `if (test) then else otherwise end`.
    fn if_else(&mut self, test: Expr, then: Expr, otherwise: Expr) -> Expr {
        let then = self.block(Vec::new(), then);
        let otherwise = self.block(Vec::new(), otherwise);
        Expr::If {
            id: self.id(),
            arms: vec![CondArm {
                test,
                body: then,
                span: Self::span(),
            }],
            else_body: Some(Box::new(otherwise)),
            span: Self::span(),
        }
    }

    /// `values(...)`, the built-in one whatever the program defines.
    fn values(&mut self, values: Vec<Expr>) -> Expr {
        self.call(super::KERNEL_VALUES, values)
    }

    /// The body of the kernel's `forward-iteration-protocol` method for
    /// `class`, whose parameter is `c`, of the method `definition`.
    pub(super) fn protocol_body(
        &mut self,
        class: BuiltinValueClass,
        definition: DefinitionId,
    ) -> Block {
        match class {
            BuiltinValueClass::Vector => self.vector_protocol(definition),
            BuiltinValueClass::String => self.string_protocol(),
            BuiltinValueClass::Table => self.table_protocol(definition),
            _ => self.sequence_protocol(),
        }
    }

    /// States are indices from 0; a stretchy vector's size is read at each
    /// step, so elements added during the loop are visited. A constant
    /// vector has no element setter.
    fn vector_protocol(&mut self, definition: DefinitionId) -> Block {
        let initial = self.int(0);
        let c = self.name("c");
        let limit = self.call("%size", vec![c]);
        let next_body = self.next_index();
        let next = self.function(&["c", "s"], "<object>", next_body);
        let state = self.integer("s");
        let c = self.name("c");
        let vector = self.cast("<vector>", c);
        let size = self.call("%size", vec![vector]);
        let done_body = self.compare(CompareOp::Ge, state, size);
        let done = self.function(&["c", "s", "l"], "<boolean>", done_body);
        let key_body = self.name("s");
        let key = self.function(&["c", "s"], "<object>", key_body);
        let c = self.name("c");
        let vector = self.cast("<vector>", c);
        let state = self.integer("s");
        let element_body = self.call("%vector-element", vec![vector, state]);
        let element = self.function(&["c", "s"], "<object>", element_body);
        let value = self.name("v");
        let c = self.name("c");
        let vector = self.cast("<vector>", c);
        let state = self.integer("s");
        let setter_body = self.call("%vector-element-setter", vec![value, vector, state]);
        let setter = self.function(&["v", "c", "s"], "<object>", setter_body);
        // The setter is an `<object>`, so it joins the `#f` of a constant
        // vector.
        let setter = self.let_statement(definition, "setter", Some("<object>"), setter);
        let setter_name = self.name("setter");
        let body = self.block(vec![setter], setter_name);
        let setter = Expr::Block {
            id: self.id(),
            exit: None,
            body: Box::new(body),
            cleanup: None,
            span: Self::span(),
        };
        let c = self.name("c");
        let constant = self.call("%vector-constant?", vec![c]);
        let no_setter = self.boolean(false);
        let setter = self.if_else(constant, no_setter, setter);
        let result = self.values(vec![initial, limit, next, done, key, element, setter]);
        self.block(Vec::new(), result)
    }

    /// States are `<string-position>`s, from the start to the end, so a
    /// position the loop finds indexes the string, as in Swift. Each
    /// element is the character at its state. A string is immutable, so it
    /// has no element setter.
    fn string_protocol(&mut self) -> Block {
        let c = self.name("c");
        let initial = self.call("%start-position", vec![c]);
        let c = self.name("c");
        let limit = self.call("%end-position", vec![c]);
        let c = self.name("c");
        let string = self.cast("<string>", c);
        let s = self.name("s");
        let state = self.cast("<string-position>", s);
        let next_body = self.call("%next-position", vec![string, state]);
        let next = self.function(&["c", "s"], "<object>", next_body);
        let s = self.name("s");
        let state = self.cast("<string-position>", s);
        let l = self.name("l");
        let limit_value = self.cast("<string-position>", l);
        let done_body = self.compare(CompareOp::Ge, state, limit_value);
        let done = self.function(&["c", "s", "l"], "<boolean>", done_body);
        let key_body = self.name("s");
        let key = self.function(&["c", "s"], "<object>", key_body);
        let c = self.name("c");
        let string = self.cast("<string>", c);
        let s = self.name("s");
        let state = self.cast("<string-position>", s);
        let element_body = self.call("%string-element", vec![string, state]);
        let element = self.function(&["c", "s"], "<object>", element_body);
        let setter = self.boolean(false);
        let result = self.values(vec![initial, limit, next, done, key, element, setter]);
        self.block(Vec::new(), result)
    }

    /// States are entry positions, skipping removed entries, up to the
    /// table's number of positions. The table's version is read once, and
    /// moving to the next entry after the keys changed signals a
    /// `<collection-modified-error>`.
    fn table_protocol(&mut self, definition: DefinitionId) -> Block {
        let c = self.name("c");
        let version_value = self.call("%table-version", vec![c]);
        let version = self.let_statement(definition, "version", None, version_value);
        let c = self.name("c");
        let zero = self.int(0);
        let version_name = self.name("version");
        let initial = self.call("%table-next-position", vec![c, zero, version_name]);
        let c = self.name("c");
        let limit = self.call("%table-positions", vec![c]);
        let c = self.name("c");
        let table = self.cast("<table>", c);
        let after = self.next_index();
        let version_name = self.name("version");
        let next_body = self.call("%table-next-position", vec![table, after, version_name]);
        let next = self.function(&["c", "s"], "<object>", next_body);
        let state = self.integer("s");
        let limit_value = self.integer("l");
        let done_body = self.compare(CompareOp::Ge, state, limit_value);
        let done = self.function(&["c", "s", "l"], "<boolean>", done_body);
        let c = self.name("c");
        let table = self.cast("<table>", c);
        let state = self.integer("s");
        let version_name = self.name("version");
        let key_body = self.call("%table-position-key", vec![table, state, version_name]);
        let key = self.function(&["c", "s"], "<object>", key_body);
        let c = self.name("c");
        let table = self.cast("<table>", c);
        let state = self.integer("s");
        let version_name = self.name("version");
        let element_body = self.call("%table-position-value", vec![table, state, version_name]);
        let element = self.function(&["c", "s"], "<object>", element_body);
        let value = self.name("v");
        let c = self.name("c");
        let table = self.cast("<table>", c);
        let key_table = self.name("c");
        let key_table = self.cast("<table>", key_table);
        let state = self.integer("s");
        let version_name = self.name("version");
        let entry_key = self.call("%table-position-key", vec![key_table, state, version_name]);
        let setter_body = self.call("%table-element-setter", vec![value, table, entry_key]);
        let setter = self.function(&["v", "c", "s"], "<object>", setter_body);
        let result = self.values(vec![initial, limit, next, done, key, element, setter]);
        self.block(vec![version], result)
    }

    /// The `<sequence>` default: integer states from 0 below `size(c)`,
    /// elements read with `element` and stored with `element-setter`, sent
    /// to whatever methods the collection's class has.
    fn sequence_protocol(&mut self) -> Block {
        let initial = self.int(0);
        let c = self.name("c");
        let limit = self.call("size", vec![c]);
        let next_body = self.next_index();
        let next = self.function(&["c", "s"], "<object>", next_body);
        let state = self.integer("s");
        let limit_value = self.integer("l");
        let done_body = self.compare(CompareOp::Ge, state, limit_value);
        let done = self.function(&["c", "s", "l"], "<boolean>", done_body);
        let key_body = self.name("s");
        let key = self.function(&["c", "s"], "<object>", key_body);
        let c = self.name("c");
        let state = self.name("s");
        let element_body = self.call("element", vec![c, state]);
        let element = self.function(&["c", "s"], "<object>", element_body);
        let value = self.name("v");
        let c = self.name("c");
        let state = self.name("s");
        let setter_body = self.call("element-setter", vec![value, c, state]);
        let setter = self.function(&["v", "c", "s"], "<object>", setter_body);
        let result = self.values(vec![initial, limit, next, done, key, element, setter]);
        self.block(Vec::new(), result)
    }
}

fn named(name: &str) -> TypeExpr {
    TypeExpr::Named {
        name: String::from(name),
        span: Span::default(),
    }
}
