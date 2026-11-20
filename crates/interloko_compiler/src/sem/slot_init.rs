//! Slot defaults: Dylan's `slot x = expr`, `init-value:` and
//! `init-function:`.
//!
//! A slot's default gives it a value when a `make` does not pass its
//! keyword. Dylan writes it three ways:
//!
//! - `slot x = expr` evaluates `expr` at each `make` that takes it, so
//!   `slot bindings :: <table> = make(<table>)` gives every instance a
//!   table of its own;
//! - `init-function: f` calls `f` with no arguments at each such `make`;
//! - `init-value: expr` evaluates `expr` once, and every instance that
//!   takes it shares that value.
//!
//! Semantic lowering knows one form, an expression evaluated at each
//! `make` ([`ast::SlotInitKind::Expression`]), which it compiles to code of
//! its own unless it is a literal. [`desugar`] rewrites the other two into
//! it before semantic lowering:
//!
//! - an `init-value:` whose value has no identity apart from what it
//!   writes stays in place: a number, boolean, character, symbol or type
//!   literal, a name (of a constant, function or generic, the only names
//!   a default can read) or an anonymous method, which captures nothing
//!   there and so is one value however often it is evaluated. Any other
//!   becomes a hidden constant named `<class> <slot> init-value`, which
//!   source cannot spell, and the default reads it. A constant is evaluated
//!   once, at its first read in a run, so that is once per run: each run
//!   evaluates its constants afresh;
//! - an `init-function:`'s function is kept the same way, in place or in a
//!   hidden constant `<class> <slot> init-function`, and the default calls
//!   it.

use alloc::borrow::Cow;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::ast::{self, SlotInit, SlotInitKind};
use crate::identity::{AstNodeId, DefinitionId, DefinitionKind};

/// Rewrites every slot default of `module` into the
/// [`SlotInitKind::Expression`] form, adding the hidden constants that
/// keep `init-value:`s and `init-function:`s; see the module docs. A
/// module whose defaults are all expressions is borrowed.
#[must_use]
pub fn desugar(module: &ast::Module) -> Cow<'_, ast::Module> {
    let rewrites = module.classes.iter().any(|class| {
        class.slots.iter().any(|slot| {
            slot.init
                .as_ref()
                .is_some_and(|init| init.kind != SlotInitKind::Expression)
        })
    });
    if !rewrites {
        return Cow::Borrowed(module);
    }
    let mut out = module.clone();
    let mut constants = Vec::new();
    for class in &mut out.classes {
        for slot in &mut class.slots {
            let Some(init) = slot.init.take() else {
                continue;
            };
            let value = match init.kind {
                SlotInitKind::Expression => init.value,
                SlotInitKind::Value => kept(
                    module,
                    &mut constants,
                    &class.name,
                    slot,
                    "init-value",
                    init.value,
                ),
                SlotInitKind::Function => {
                    let function = kept(
                        module,
                        &mut constants,
                        &class.name,
                        slot,
                        "init-function",
                        init.value,
                    );
                    call(slot.id, function)
                }
            };
            slot.init = Some(SlotInit {
                kind: SlotInitKind::Expression,
                value,
                span: init.span,
            });
        }
    }
    out.constants.extend(constants);
    Cow::Owned(out)
}

/// Whether `value`, evaluated at each `make`, is one value: what an
/// `init-value:` evaluated once would give every instance.
fn is_one_value(value: &ast::Expr) -> bool {
    match value {
        ast::Expr::Number { .. }
        | ast::Expr::Float { .. }
        | ast::Expr::Bool { .. }
        | ast::Expr::Character { .. }
        | ast::Expr::Symbol { .. }
        | ast::Expr::Name { .. }
        | ast::Expr::ClassName { .. }
        | ast::Expr::TypeLiteral { .. }
        | ast::Expr::Function { .. } => true,
        ast::Expr::UnaryMinus { expr, .. } => {
            matches!(**expr, ast::Expr::Number { .. } | ast::Expr::Float { .. })
        }
        _ => false,
    }
}

/// `value`, evaluated once: in place when it is one value already, and
/// otherwise a read of a hidden constant of `class`'s `slot`, added to
/// `constants`, that holds it.
fn kept(
    module: &ast::Module,
    constants: &mut Vec<ast::Constant>,
    class: &str,
    slot: &ast::Slot,
    option: &str,
    value: ast::Expr,
) -> ast::Expr {
    if is_one_value(&value) {
        return value;
    }
    let name = hidden_constant_name(class, &slot.name, option);
    let span = value.span();
    constants.push(ast::Constant {
        id: slot.id,
        definition: DefinitionId::new(
            module.source_file,
            DefinitionKind::Constant,
            &name,
            "slot-default",
        ),
        name: name.clone(),
        type_annotation: None,
        value,
        strict: false,
        span,
    });
    ast::Expr::Name {
        id: slot.id,
        name,
        span,
    }
}

/// A call of `function` with no arguments: by name when it is one, so a
/// function is called directly.
fn call(id: AstNodeId, function: ast::Expr) -> ast::Expr {
    let span = function.span();
    match function {
        ast::Expr::Name { name, .. } => ast::Expr::Call {
            id,
            callee: name,
            args: Vec::new(),
            keywords: Vec::new(),
            span,
        },
        function => ast::Expr::ValueCall {
            id,
            callee: Box::new(function),
            args: Vec::new(),
            keywords: Vec::new(),
            span,
        },
    }
}

/// The name of the hidden constant keeping `class`'s `slot`'s `option`,
/// which source cannot spell.
fn hidden_constant_name(class: &str, slot: &str, option: &str) -> String {
    format!("{class} {slot} {option}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar_frontend;
    use crate::identity::SourceFileId;

    fn parse(source: &str) -> ast::Module {
        let tree = grammar_frontend::parse_file(SourceFileId::ANONYMOUS, source).syntax;
        let module = ast::lower(&tree);
        assert_eq!(module.diagnostics, Vec::new());
        module
    }

    fn defaults(module: &ast::Module) -> Vec<(SlotInitKind, &ast::Expr)> {
        module.classes[0]
            .slots
            .iter()
            .filter_map(|slot| slot.init.as_ref())
            .map(|init| (init.kind, &init.value))
            .collect()
    }

    #[test]
    fn a_module_of_expression_defaults_is_borrowed() {
        let module = parse("define class <a> (<object>)\n  slot x = make(<table>);\nend;");
        assert!(matches!(desugar(&module), Cow::Borrowed(_)));
    }

    #[test]
    fn values_are_kept_once_and_functions_called() {
        let module = parse(
            "define class <a> (<object>)\n  \
             slot n = 1;\n  \
             slot k, init-value: -2;\n  \
             slot t, init-value: make(<table>);\n  \
             slot f, init-function: method () make(<table>) end;\n  \
             slot g, init-function: make-it;\n  \
             slot h, init-function: pick(1);\n\
             end;",
        );
        let out = desugar(&module);
        let defaults = defaults(&out);
        assert!(
            defaults
                .iter()
                .all(|(kind, _)| *kind == SlotInitKind::Expression)
        );
        assert!(matches!(defaults[0].1, ast::Expr::Number { value: 1, .. }));
        assert!(matches!(defaults[1].1, ast::Expr::Number { value: -2, .. }));
        assert!(matches!(
            defaults[2].1,
            ast::Expr::Name { name, .. } if name == "<a> t init-value"
        ));
        assert!(matches!(
            defaults[3].1,
            ast::Expr::ValueCall { callee, args, .. }
                if args.is_empty() && matches!(**callee, ast::Expr::Function { .. })
        ));
        assert!(matches!(
            defaults[4].1,
            ast::Expr::Call { callee, args, .. } if callee == "make-it" && args.is_empty()
        ));
        assert!(matches!(
            defaults[5].1,
            ast::Expr::Call { callee, .. } if callee == "<a> h init-function"
        ));
        let hidden = out
            .constants
            .iter()
            .map(|constant| constant.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(hidden, ["<a> t init-value", "<a> h init-function"]);
    }
}
