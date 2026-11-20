//! The kernel library: kernel functions and methods written in Loko
//! ([`SOURCE`]) rather than built from syntax trees by hand.
//!
//! The library is parsed the first time a module needs one of its
//! definitions, and each definition joins the module only when needed:
//!
//! - a function when a call or a value names it. A program reaches only
//!   the functions [`EXPORTS`] lists, and only when no binding or
//!   definition of its own takes the name; the rest are the library's own
//!   helpers. Each is a kernel function, as the value of a built-in is.
//! - a method of a built-in collection generic (`size`, `element`,
//!   `element-setter`, `forward-iteration-protocol`) whenever the generic
//!   is, as one of the kernel's methods, over whose class the generic is
//!   sealed as it is over the kernel's other classes.
//!
//! Library code is the kernel's: sem lowers it in the kernel's scope,
//! where no definition of the program is visible, so `size` there is the
//! kernel's whatever the program defines, and a library helper's name
//! never meets a program's. Its syntax trees carry no source positions, as
//! the kernel's other generated code does not: they are not the program's
//! text.

use crate::ast::{self, Block, Expr, ForClause, LetPattern, Statement};
use crate::grammar_frontend;
use crate::identity::SourceFileId;
use crate::syntax::Span;

/// The library's Loko source.
pub const SOURCE: &str = include_str!("library.loko");

/// The library functions a program can call or name, by name.
pub const EXPORTS: &[&str] = &[
    "empty?", "member?", "reduce", "first", "second", "third", "reverse", "reverse!", "map",
];

/// The logical path the library's source identity derives from.
const SOURCE_PATH: &str = "interloko:kernel-library.loko";

/// Whether a program reaches the library function `name`.
#[must_use]
pub fn exports(name: &str) -> bool {
    EXPORTS.contains(&name)
}

/// The parsed library.
#[derive(Clone, Debug)]
pub struct Library {
    module: ast::Module,
}

impl Library {
    /// Parses [`SOURCE`] and clears its positions.
    pub fn parse() -> Self {
        let report = grammar_frontend::parse_file(SourceFileId::from_path(SOURCE_PATH), SOURCE);
        let mut module = ast::lower(&report.syntax);
        debug_assert!(
            report.syntax.diagnostics.is_empty() && module.diagnostics.is_empty(),
            "the kernel library parses cleanly: {:?} {:?}",
            report.syntax.diagnostics,
            module.diagnostics
        );
        for function in &mut module.functions {
            function.span = Span::default();
            clear_block(&mut function.body);
        }
        for method in &mut module.methods {
            method.span = Span::default();
            clear_block(&mut method.body);
        }
        Self { module }
    }

    /// The library function named `name`.
    pub fn function(&self, name: &str) -> Option<&ast::Function> {
        self.module
            .functions
            .iter()
            .find(|function| function.name == name)
    }

    /// The library's methods of the generic named `generic`.
    pub fn methods<'a>(&'a self, generic: &'a str) -> impl Iterator<Item = &'a ast::Method> {
        self.module
            .methods
            .iter()
            .filter(move |method| method.name == generic)
    }

    /// The parsed module, for tests.
    #[cfg(test)]
    pub fn module(&self) -> &ast::Module {
        &self.module
    }
}

fn clear_block(block: &mut Block) {
    block.span = Span::default();
    for statement in &mut block.statements {
        match statement {
            Statement::Let(binding) => {
                binding.span = Span::default();
                match &mut binding.pattern {
                    LetPattern::Name(name) => name.span = Span::default(),
                    LetPattern::Tuple { bindings, span } => {
                        *span = Span::default();
                        for name in bindings {
                            name.span = Span::default();
                        }
                    }
                }
                clear_expr(&mut binding.value);
            }
            Statement::Expr(expr) => clear_expr(expr),
        }
    }
    clear_expr(&mut block.result);
}

fn clear_exprs<'a>(exprs: impl IntoIterator<Item = &'a mut Expr>) {
    exprs.into_iter().for_each(clear_expr);
}

fn clear_keywords(keywords: &mut [ast::KeywordArg]) {
    for keyword in keywords {
        keyword.span = Span::default();
        clear_expr(&mut keyword.value);
    }
}

/// Clears the positions of `expr` and every expression and body in it.
fn clear_expr(expr: &mut Expr) {
    match expr {
        Expr::Number { span, .. }
        | Expr::Float { span, .. }
        | Expr::Number32 { span, .. }
        | Expr::Bool { span, .. }
        | Expr::String { span, .. }
        | Expr::Character { span, .. }
        | Expr::Symbol { span, .. }
        | Expr::Name { span, .. }
        | Expr::ClassName { span, .. }
        | Expr::TypeLiteral { span, .. }
        | Expr::Error { span, .. } => *span = Span::default(),
        Expr::Vector { elements, span, .. } => {
            *span = Span::default();
            clear_exprs(elements);
        }
        Expr::Function {
            params, body, span, ..
        } => {
            *span = Span::default();
            for param in params {
                param.span = Span::default();
            }
            clear_block(body);
        }
        Expr::Cast { value, span, .. }
        | Expr::Signal {
            condition: value,
            span,
            ..
        }
        | Expr::SlotRead {
            receiver: value,
            span,
            ..
        }
        | Expr::UnaryMinus {
            expr: value, span, ..
        } => {
            *span = Span::default();
            clear_expr(value);
        }
        Expr::Assign {
            value,
            name_span,
            span,
            ..
        } => {
            *span = Span::default();
            *name_span = Span::default();
            clear_expr(value);
        }
        Expr::Call {
            args,
            keywords,
            span,
            ..
        } => {
            *span = Span::default();
            clear_exprs(args);
            clear_keywords(keywords);
        }
        Expr::ValueCall {
            callee,
            args,
            keywords,
            span,
            ..
        } => {
            *span = Span::default();
            clear_expr(callee);
            clear_exprs(args);
            clear_keywords(keywords);
        }
        Expr::MakeObject {
            class,
            fields,
            span,
            ..
        } => {
            *span = Span::default();
            clear_expr(class);
            clear_keywords(fields);
        }
        Expr::SlotAssign {
            receiver: lhs,
            value: rhs,
            span,
            ..
        }
        | Expr::Binary { lhs, rhs, span, .. }
        | Expr::Compare { lhs, rhs, span, .. }
        | Expr::Identical { lhs, rhs, span, .. } => {
            *span = Span::default();
            clear_expr(lhs);
            clear_expr(rhs);
        }
        Expr::Loop {
            test, body, span, ..
        } => {
            *span = Span::default();
            clear_expr(test);
            clear_block(body);
        }
        Expr::For {
            clauses,
            body,
            span,
            ..
        } => {
            *span = Span::default();
            for clause in clauses {
                match clause {
                    ForClause::Numeric(clause) => {
                        clause.span = Span::default();
                        clause.binding.span = Span::default();
                        clear_expr(&mut clause.start);
                        if let Some((_, bound)) = &mut clause.bound {
                            clear_expr(bound);
                        }
                        if let Some(step) = &mut clause.step {
                            clear_expr(step);
                        }
                    }
                    ForClause::Collection(clause) => {
                        clause.span = Span::default();
                        clause.binding.span = Span::default();
                        clear_expr(&mut clause.collection);
                    }
                }
            }
            clear_block(body);
        }
        Expr::If {
            arms,
            else_body: default,
            span,
            ..
        }
        | Expr::Case {
            arms,
            otherwise: default,
            span,
            ..
        } => {
            *span = Span::default();
            for arm in arms {
                arm.span = Span::default();
                clear_expr(&mut arm.test);
                clear_block(&mut arm.body);
            }
            if let Some(body) = default {
                clear_block(body);
            }
        }
        Expr::Unless {
            condition,
            body,
            span,
            ..
        } => {
            *span = Span::default();
            clear_expr(condition);
            clear_block(body);
        }
        Expr::Select {
            target,
            test,
            arms,
            otherwise,
            span,
            ..
        } => {
            *span = Span::default();
            clear_expr(target);
            if let Some(test) = test {
                clear_expr(test);
            }
            for arm in arms {
                arm.span = Span::default();
                clear_exprs(&mut arm.keys);
                clear_block(&mut arm.body);
            }
            if let Some(body) = otherwise {
                clear_block(body);
            }
        }
        Expr::Handle {
            body,
            restart,
            span,
            ..
        } => {
            *span = Span::default();
            clear_expr(body);
            restart.span = Span::default();
            restart.condition_span = Span::default();
            if let Some(guard) = &mut restart.guard {
                clear_expr(guard);
            }
            clear_expr(&mut restart.body);
        }
        Expr::Block {
            exit,
            body,
            cleanup,
            span,
            ..
        } => {
            *span = Span::default();
            if let Some(exit) = exit {
                exit.span = Span::default();
            }
            clear_block(body);
            if let Some(cleanup) = cleanup {
                clear_block(cleanup);
            }
        }
    }
}

/// The library's functions and methods, in source order, for tests.
#[cfg(test)]
pub fn definition_names() -> alloc::vec::Vec<alloc::string::String> {
    let library = Library::parse();
    library
        .module()
        .functions
        .iter()
        .map(|function| function.name.clone())
        .chain(
            library
                .module()
                .methods
                .iter()
                .map(|method| method.name.clone()),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{EXPORTS, Library, definition_names};

    #[test]
    fn the_library_parses_and_defines_what_it_exports() {
        let library = Library::parse();
        assert!(library.module().diagnostics.is_empty());
        let names = definition_names();
        for name in EXPORTS {
            assert!(library.function(name).is_some(), "{name} is exported");
        }
        assert!(
            names
                .iter()
                .any(|name| name == "forward-iteration-protocol")
        );
    }
}
