//! Keyword parameters: Dylan's `#key name :: <type> = default`.
//!
//! A function, a generic and a method may take keyword parameters after
//! their required ones. A call passes a keyword as `name: value`, after its
//! positional arguments, in any order.
//!
//! # Convention
//!
//! A keyword parameter is an ordinary trailing parameter of the definition:
//! a function or generic with required parameters `r1 … rn` and keywords
//! `k1 … km` takes `r1 … rn, k1 … km`, keywords in declaration order. A call
//! always passes every keyword, so a typed call with known keywords is a
//! plain direct call or send, with no keyword parsing at run time:
//!
//! 1. The call's positional arguments and the keyword arguments it passes
//!    are evaluated in source order.
//! 2. Then each keyword the call does not pass takes its default, in
//!    declaration order: `$unsupplied` for a keyword declared without one,
//!    otherwise the value of the keyword's *default function*.
//!
//! A keyword's default function is a hidden function named
//! `<owner> #key <keyword>` that takes the owner's required parameters and
//! the keywords declared before this one, and returns the default
//! expression's value. The default expression may therefore read those
//! parameters, and nothing else of the call. It belongs to the owner: a
//! caller that does not pass the keyword calls it, so editing a default
//! changes only that function's body, never the callers' code.
//!
//! # Generics and methods
//!
//! Dylan gives each method its own defaults; Loko simplifies. A generic
//! declares the keywords its methods accept, with their types and defaults,
//! and a send fills defaults from the generic, since it cannot know which
//! method will run. A method may bind any of its generic's keywords under
//! the generic's types, takes them all as parameters whether it binds them
//! or not, and gives no defaults of its own. Dispatch looks only at the
//! required parameters. A method whose behaviour depends on whether a
//! keyword was passed declares it without a default and compares it with
//! `$unsupplied` using `==`.
//!
//! An anonymous function cannot take keyword parameters, and a function or
//! generic that takes them cannot be used as a callable value: calls through
//! callable values pass no keywords yet.
//!
//! [`desugar`] rewrites an AST module into this convention before semantic
//! lowering: keyword parameters become trailing required parameters, and
//! each default becomes a function of the module. It returns each owner's
//! [`KeywordSignature`], which call lowering uses to order a call's
//! arguments.

use alloc::borrow::Cow;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::ast;
use crate::diagnostic::{Diagnostic, DiagnosticArg, DiagnosticCode};
use crate::identity::{AstNodeId, DefinitionId, DefinitionKind, SourceFileId};
use crate::syntax::Span;

use super::CollectionFunction;

/// The name of the kernel constant a keyword declared without a default
/// takes when a call does not pass it.
pub const UNSUPPLIED: &str = "$unsupplied";

/// The symbol [`UNSUPPLIED`] is: `#unsupplied`.
pub const UNSUPPLIED_SYMBOL: &str = interloko_runtime::UNSUPPLIED_SYMBOL;

/// How a call that does not pass a keyword gets its value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KeywordDefault {
    /// `$unsupplied`: the keyword declares no default.
    Unsupplied,
    /// None: the keyword declares no default but cannot hold
    /// `$unsupplied`, which has been reported.
    Missing,
    /// The value of the named default function, called with the owner's
    /// required arguments and the keywords declared before this one.
    Function(String),
}

/// One keyword parameter of a function or generic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeywordSlot {
    /// The keyword, without its colon.
    pub name: String,
    /// What a call that does not pass it passes.
    pub default: KeywordDefault,
}

/// The keyword parameters of a function or generic, which follow its
/// `required` parameters.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct KeywordSignature {
    /// The number of required parameters.
    pub required: usize,
    /// The keyword parameters, in declaration order.
    pub keywords: Vec<KeywordSlot>,
}

impl KeywordSignature {
    /// The position of `keyword` among the keywords.
    #[must_use]
    pub fn position(&self, keyword: &str) -> Option<usize> {
        self.keywords.iter().position(|slot| slot.name == keyword)
    }
}

/// A module rewritten into the keyword convention.
pub struct Desugared<'a> {
    /// The module, with keyword parameters as trailing parameters and the
    /// default functions added after its own functions; the module itself
    /// when it has no keyword parameters.
    pub module: Cow<'a, ast::Module>,
    /// The keyword signature of each function that takes keywords, by name.
    pub functions: Vec<(String, KeywordSignature)>,
    /// The keyword signature of each generic that takes keywords, by name.
    pub generics: Vec<(String, KeywordSignature)>,
}

/// The keywords the kernel's built-in generic `name` takes, with their
/// types, as a method of it declared in `module` at `span` sees them.
fn kernel_keywords(module: &ast::Module, name: &str, span: Span) -> Vec<ast::KeywordParam> {
    let shadowed = module.generics.iter().any(|generic| generic.name == name)
        || module
            .functions
            .iter()
            .any(|function| function.name == name);
    let Some(function) = CollectionFunction::from_name(name).filter(|_| !shadowed) else {
        return Vec::new();
    };
    function
        .keywords()
        .iter()
        .map(|keyword| ast::KeywordParam {
            id: AstNodeId::generated(
                module.source_file,
                KERNEL_KEYWORD_TAG,
                &format!("{name} #key {keyword}"),
                span.start,
                span.end,
            ),
            name: (*keyword).into(),
            type_annotation: Some(object_type(span)),
            default: None,
            span,
        })
        .collect()
}

/// Generated-id tag of a kernel generic's keyword parameter as one of a
/// program's methods takes it.
const KERNEL_KEYWORD_TAG: u64 = 0x6b65_7977_6f72_6473;

/// The keyword signature of the kernel's built-in generic `name`.
#[must_use]
pub fn kernel_signature(function: CollectionFunction) -> KeywordSignature {
    KeywordSignature {
        required: function.arity(),
        keywords: function
            .keywords()
            .iter()
            .map(|keyword| KeywordSlot {
                name: (*keyword).into(),
                default: KeywordDefault::Unsupplied,
            })
            .collect(),
    }
}

fn object_type(span: Span) -> ast::TypeExpr {
    ast::TypeExpr::Named {
        name: "<object>".into(),
        span,
    }
}

/// The type a keyword parameter is declared with: its annotation, or
/// `<object>`.
fn keyword_type(keyword: &ast::KeywordParam) -> ast::TypeExpr {
    keyword
        .type_annotation
        .clone()
        .unwrap_or_else(|| object_type(keyword.span))
}

/// Whether a keyword of type `ty` may go without a default: only an
/// `<object>` can hold `$unsupplied`.
fn holds_unsupplied(ty: &ast::TypeExpr) -> bool {
    matches!(ty, ast::TypeExpr::Named { name, .. } if name == "<object>")
}

/// The name of `owner`'s default function for `keyword`.
fn default_function_name(owner: &str, keyword: &str) -> String {
    format!("{owner} #key {keyword}")
}

/// The name a method gives a keyword of its generic that it does not bind.
fn unbound_name(keyword: &str) -> String {
    format!("#key {keyword}")
}

/// Rewrites `module` into the keyword convention; see the module docs.
/// Problems with keyword declarations are added to the module's
/// diagnostics.
#[must_use]
pub fn desugar(module: &ast::Module) -> Desugared<'_> {
    let takes_keywords = module
        .functions
        .iter()
        .any(|function| !function.keywords.is_empty())
        || module
            .generics
            .iter()
            .any(|generic| !generic.keywords.is_empty())
        || module.methods.iter().any(|method| {
            !method.keywords.is_empty()
                || !kernel_keywords(module, &method.name, method.span).is_empty()
        });
    if !takes_keywords {
        return Desugared {
            module: Cow::Borrowed(module),
            functions: Vec::new(),
            generics: Vec::new(),
        };
    }
    let mut out = module.clone();
    let mut functions = Vec::new();
    let mut generics = Vec::new();
    let mut defaults = Vec::new();

    for function in &mut out.functions {
        if function.keywords.is_empty() {
            continue;
        }
        let required = function.params.clone();
        let mut earlier = Vec::new();
        let mut slots = Vec::new();
        for keyword in core::mem::take(&mut function.keywords) {
            let ty = keyword_type(&keyword);
            let default = keyword_default(
                &mut out.diagnostics,
                &mut defaults,
                module.source_file,
                &function.name,
                &keyword,
                &ty,
                required
                    .iter()
                    .cloned()
                    .chain(earlier.iter().cloned())
                    .collect(),
            );
            let param = ast::FunctionParam {
                id: keyword.id,
                name: keyword.name.clone(),
                type_annotation: Some(ty),
                span: keyword.span,
            };
            earlier.push(param.clone());
            function.params.push(param);
            slots.push(KeywordSlot {
                name: keyword.name,
                default,
            });
        }
        functions.push((
            function.name.clone(),
            KeywordSignature {
                required: required.len(),
                keywords: slots,
            },
        ));
    }

    let mut generic_keywords: Vec<(String, Vec<ast::KeywordParam>)> = Vec::new();
    for generic in &mut out.generics {
        if generic.keywords.is_empty() {
            continue;
        }
        let required = generic
            .params
            .iter()
            .map(|param| ast::FunctionParam {
                id: param.id,
                name: param.name.clone(),
                type_annotation: Some(
                    param
                        .type_annotation
                        .clone()
                        .unwrap_or_else(|| object_type(param.span)),
                ),
                span: param.span,
            })
            .collect::<Vec<_>>();
        let mut earlier = Vec::new();
        let mut slots = Vec::new();
        let keywords = core::mem::take(&mut generic.keywords);
        for keyword in &keywords {
            let ty = keyword_type(keyword);
            let default = keyword_default(
                &mut out.diagnostics,
                &mut defaults,
                module.source_file,
                &generic.name,
                keyword,
                &ty,
                required
                    .iter()
                    .cloned()
                    .chain(earlier.iter().cloned())
                    .collect(),
            );
            earlier.push(ast::FunctionParam {
                id: keyword.id,
                name: keyword.name.clone(),
                type_annotation: Some(ty.clone()),
                span: keyword.span,
            });
            generic.params.push(ast::GenericParam {
                id: keyword.id,
                name: keyword.name.clone(),
                type_annotation: Some(ty),
                span: keyword.span,
            });
            slots.push(KeywordSlot {
                name: keyword.name.clone(),
                default,
            });
        }
        generic_keywords.push((generic.name.clone(), keywords));
        generics.push((
            generic.name.clone(),
            KeywordSignature {
                required: required.len(),
                keywords: slots,
            },
        ));
    }

    for method in &mut out.methods {
        let declared = if let Some((_, keywords)) = generic_keywords
            .iter()
            .find(|(name, _)| *name == method.name)
        {
            keywords.clone()
        } else {
            kernel_keywords(module, &method.name, method.span)
        };
        let bound = core::mem::take(&mut method.keywords);
        for keyword in &bound {
            let Some(generic_keyword) = declared.iter().find(|known| known.name == keyword.name)
            else {
                out.diagnostics.push(
                    Diagnostic::error(keyword.span, DiagnosticCode::MethodKeywordNotInGeneric)
                        .with_arg(DiagnosticArg::text("keyword", keyword.name.clone()))
                        .with_arg(DiagnosticArg::text("generic", method.name.clone())),
                );
                continue;
            };
            if let Some(default) = &keyword.default {
                out.diagnostics.push(
                    Diagnostic::error(default.span(), DiagnosticCode::MethodKeywordDefault)
                        .with_arg(DiagnosticArg::text("keyword", keyword.name.clone()))
                        .with_arg(DiagnosticArg::text("generic", method.name.clone())),
                );
            }
            let expected = keyword_type(generic_keyword);
            if let Some(written) = &keyword.type_annotation
                && written.to_string() != expected.to_string()
            {
                out.diagnostics.push(
                    Diagnostic::error(written.span(), DiagnosticCode::MethodKeywordTypeMismatch)
                        .with_arg(DiagnosticArg::text("keyword", keyword.name.clone()))
                        .with_arg(DiagnosticArg::text("generic", method.name.clone()))
                        .with_arg(DiagnosticArg::text("expected", expected.to_string()))
                        .with_arg(DiagnosticArg::text("actual", written.to_string())),
                );
            }
        }
        // A method takes every keyword of its generic, binding those it
        // declares, under ids of its own.
        for keyword in &declared {
            let name = if bound.iter().any(|bound| bound.name == keyword.name) {
                keyword.name.clone()
            } else {
                unbound_name(&keyword.name)
            };
            method.params.push(ast::MethodParam {
                id: AstNodeId::generated(
                    module.source_file,
                    KERNEL_KEYWORD_TAG,
                    &format!("{} #key {}", method.definition, keyword.name),
                    method.span.start,
                    method.span.end,
                ),
                name,
                specializer: Some(keyword_type(keyword)),
                span: keyword.span,
            });
        }
    }

    out.functions.extend(defaults);
    Desugared {
        module: Cow::Owned(out),
        functions,
        generics,
    }
}

/// Builds `keyword`'s default function of `owner`, taking `params`, when it
/// has a default, and returns how a call fills it in.
fn keyword_default(
    diagnostics: &mut Vec<Diagnostic>,
    defaults: &mut Vec<ast::Function>,
    source_file: SourceFileId,
    owner: &str,
    keyword: &ast::KeywordParam,
    ty: &ast::TypeExpr,
    params: Vec<ast::FunctionParam>,
) -> KeywordDefault {
    let Some(default) = &keyword.default else {
        if !holds_unsupplied(ty) {
            diagnostics.push(
                Diagnostic::error(keyword.span, DiagnosticCode::KeywordParameterNeedsDefault)
                    .with_arg(DiagnosticArg::text("keyword", keyword.name.clone()))
                    .with_arg(DiagnosticArg::text("type", ty.to_string())),
            );
            return KeywordDefault::Missing;
        }
        return KeywordDefault::Unsupplied;
    };
    let name = default_function_name(owner, &keyword.name);
    defaults.push(ast::Function {
        id: keyword.id,
        definition: DefinitionId::new(
            source_file,
            DefinitionKind::Function,
            &name,
            "keyword-default",
        ),
        name: name.clone(),
        params,
        keywords: Vec::new(),
        return_type: Some(ast::ResultAnnotation {
            values: vec![ty.clone()],
            span: ty.span(),
        }),
        body: ast::Block {
            id: keyword.id,
            statements: Vec::new(),
            result: default.clone(),
            span: default.span(),
        },
        strict: false,
        span: keyword.span,
    });
    KeywordDefault::Function(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar_frontend;

    fn parse(source: &str) -> ast::Module {
        let tree = grammar_frontend::parse_file(SourceFileId::ANONYMOUS, source).syntax;
        let module = ast::lower(&tree);
        assert_eq!(module.diagnostics, Vec::new());
        module
    }

    fn param_names(params: &[ast::FunctionParam]) -> Vec<&str> {
        params.iter().map(|param| param.name.as_str()).collect()
    }

    #[test]
    fn a_module_without_keywords_is_borrowed() {
        let module = parse("define function f(x :: <integer>) => <integer>\n  x\nend;");
        let out = desugar(&module);
        assert!(matches!(out.module, Cow::Borrowed(_)));
        assert!(out.functions.is_empty() && out.generics.is_empty());
    }

    #[test]
    fn keywords_become_trailing_parameters_and_defaults_become_functions() {
        let module = parse(
            "define function f(x :: <integer>, #key by :: <integer> = 2, tag) => <integer>\n  \
             x\nend;",
        );
        let out = desugar(&module);
        let f = &out.module.functions[0];
        assert_eq!(param_names(&f.params), ["x", "by", "tag"]);
        assert!(f.keywords.is_empty());
        // The default takes the required parameters and the keywords before
        // its own; `tag` has no default, so it has no function.
        let default = &out.module.functions[1];
        assert_eq!(default.name, "f #key by");
        assert_eq!(param_names(&default.params), ["x"]);
        assert_eq!(out.module.functions.len(), 2);
        assert_eq!(
            out.functions,
            vec![(
                "f".into(),
                KeywordSignature {
                    required: 1,
                    keywords: vec![
                        KeywordSlot {
                            name: "by".into(),
                            default: KeywordDefault::Function("f #key by".into()),
                        },
                        KeywordSlot {
                            name: "tag".into(),
                            default: KeywordDefault::Unsupplied,
                        },
                    ],
                },
            )]
        );
    }

    #[test]
    fn a_method_takes_every_keyword_of_its_generic() {
        let module = parse(
            "define generic g(s, #key a :: <integer> = 1, b) => <integer>;\n\
             define method g(s :: <integer>, #key b) => <integer>\n  s\nend;\n\
             define method element(s :: <string>, k :: <integer>) => <object>\n  s\nend;",
        );
        let out = desugar(&module);
        let method = &out.module.methods[0];
        let names = method
            .params
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>();
        // It binds `b` and takes `a` under a name source cannot write.
        assert_eq!(names, ["s", "#key a", "b"]);
        let element = &out.module.methods[1];
        assert_eq!(element.params.len(), 3);
        assert_eq!(element.params[2].name, "#key default");
        assert!(!crate::syntax::is_name(&element.params[2].name));
        assert!(!crate::syntax::is_name(&default_function_name("g", "a")));
    }
}
