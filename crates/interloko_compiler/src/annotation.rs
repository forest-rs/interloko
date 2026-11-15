//! Typing adjustments: stable annotation targets, the source edits that
//! write an annotation, and the verdict deltas that report what an
//! adjustment changed.
//!
//! An [`AnnotationTarget`] names a place where source can declare a type:
//! a parameter or `let` binding (`binding:<definition>:<name>`, with
//! `#<n>` for the n-th binding of the same name in that definition) or a
//! definition's result (`result:<definition>`). Targets are built from
//! definition ids and source names, never from semantic local numbers, so
//! an edit elsewhere leaves them alone.
//!
//! [`annotation_sites`] finds every target in a module and whether its type
//! is written or inferred; [`AnnotationSite::edit`] produces the
//! [`SourceEdit`] that writes a type there. [`TypingDelta`] compares the
//! typing decisions of two compilations, matching each decision across the
//! edit, and reports which verdicts and codegen facets changed.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::{self, Write as _};

use crate::ast;
use crate::identity::DefinitionId;
use crate::lower::CheckCodegen;
use crate::sem::SiteId;
use crate::syntax::{Span, SyntaxTree, Token, TokenKind, lex};
use crate::typing::{TypingCheck, TypingSeverity, TypingVerdict};

/// A place in source where a type can be declared, named stably.
///
/// The EOP id is the [`fmt::Display`] form: `binding:<definition>:<name>`
/// for the first parameter or `let` binding named `<name>` in the
/// definition, `binding:<definition>:<name>#<n>` for the n-th (counting
/// from zero, in source order) when the name is bound more than once, and
/// `result:<definition>` for a function's or method's result, or a
/// constant's value.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum AnnotationTarget {
    /// A parameter, closure parameter, or `let` binding.
    Binding {
        /// The function, method, or constant whose source binds it.
        definition: DefinitionId,
        /// The bound name.
        name: String,
        /// Which binding of `name` in the definition, in source order.
        occurrence: u32,
    },
    /// A function's or method's result, or a constant's value.
    Result {
        /// The function, method, or constant.
        definition: DefinitionId,
    },
}

impl AnnotationTarget {
    /// Parses an EOP id such as `binding:def…:x`, `binding:def…:x#1`, or
    /// `result:def…`.
    #[must_use]
    pub fn parse(id: &str) -> Option<Self> {
        if let Some(rest) = id.strip_prefix("result:") {
            return Some(Self::Result {
                definition: parse_definition_id(rest)?,
            });
        }
        let rest = id.strip_prefix("binding:")?;
        let (definition, binding) = rest.split_once(':')?;
        let definition = parse_definition_id(definition)?;
        let (name, occurrence) = match binding.rsplit_once('#') {
            Some((name, occurrence)) => (name, occurrence.parse().ok()?),
            None => (binding, 0),
        };
        if name.is_empty() {
            return None;
        }
        Some(Self::Binding {
            definition,
            name: name.to_string(),
            occurrence,
        })
    }

    /// The definition whose source holds the target.
    #[must_use]
    pub const fn definition(&self) -> DefinitionId {
        match self {
            Self::Binding { definition, .. } | Self::Result { definition } => *definition,
        }
    }

    /// This target in another definition, such as the same parameter of a
    /// method whose definition id changed with its specializers.
    #[must_use]
    pub fn in_definition(&self, definition: DefinitionId) -> Self {
        match self {
            Self::Binding {
                name, occurrence, ..
            } => Self::Binding {
                definition,
                name: name.clone(),
                occurrence: *occurrence,
            },
            Self::Result { .. } => Self::Result { definition },
        }
    }
}

impl fmt::Display for AnnotationTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Binding {
                definition,
                name,
                occurrence: 0,
            } => write!(f, "binding:{definition}:{name}"),
            Self::Binding {
                definition,
                name,
                occurrence,
            } => write!(f, "binding:{definition}:{name}#{occurrence}"),
            Self::Result { definition } => write!(f, "result:{definition}"),
        }
    }
}

fn parse_definition_id(text: &str) -> Option<DefinitionId> {
    let hex = text.strip_prefix("def")?;
    if hex.len() != 16 {
        return None;
    }
    u64::from_str_radix(hex, 16).ok().map(DefinitionId)
}

/// What kind of place an [`AnnotationTarget`] names.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AnnotationRole {
    /// A function parameter.
    Param,
    /// A method parameter, whose annotation is also its specializer:
    /// changing it changes which calls the method handles, and the method's
    /// definition id with it.
    MethodParam,
    /// A parameter of a closure inside the definition.
    ClosureParam,
    /// A `let` binding, alone or in a multiple-value pattern.
    Local,
    /// A function's or method's result.
    Result,
    /// A constant's value.
    ConstantValue,
}

impl AnnotationRole {
    /// Returns the stable label used in EOP records.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Param => "param",
            Self::MethodParam => "method-param",
            Self::ClosureParam => "closure-param",
            Self::Local => "local",
            Self::Result => "result",
            Self::ConstantValue => "constant-value",
        }
    }
}

/// Whether a binding's type is written in source or inferred.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DeclarationOrigin {
    /// Source writes the type (`x :: <integer>`, `=> <integer>`).
    Written,
    /// No type is written; sem inferred the binding's category.
    Inferred,
}

impl DeclarationOrigin {
    /// Returns the stable label used in dumps and EOP records.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Written => "written",
            Self::Inferred => "inferred",
        }
    }
}

/// A replacement of one byte range of source text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceEdit {
    /// The replaced range; empty for an insertion.
    pub span: Span,
    /// The replacement text.
    pub text: String,
}

impl SourceEdit {
    /// Returns `source` with the edit applied, or `None` when the span does
    /// not fit `source`.
    #[must_use]
    pub fn apply(&self, source: &str) -> Option<String> {
        let before = source.get(..self.span.start)?;
        let after = source.get(self.span.end..)?;
        let mut edited = String::with_capacity(before.len() + self.text.len() + after.len());
        edited.push_str(before);
        edited.push_str(&self.text);
        edited.push_str(after);
        Some(edited)
    }

    /// Maps an offset in the source before the edit to the edited source.
    /// Offsets inside the replaced range map to its start.
    #[must_use]
    pub const fn map_offset(&self, offset: usize) -> usize {
        if offset <= self.span.start {
            offset
        } else if offset >= self.span.end {
            offset - (self.span.end - self.span.start) + self.text.len()
        } else {
            self.span.start
        }
    }

    /// Maps a span in the source before the edit to the edited source.
    #[must_use]
    pub const fn map_span(&self, span: Span) -> Span {
        Span {
            start: self.map_offset(span.start),
            end: self.map_offset(span.end),
        }
    }
}

/// One annotation target found in source, with what is written there.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnnotationSite {
    /// The target's stable name.
    pub target: AnnotationTarget,
    /// What the target is.
    pub role: AnnotationRole,
    /// Source span of the bound name, or of the definition for a result.
    pub span: Span,
    /// The written annotation's text and span, or `None` when the type is
    /// inferred.
    pub written: Option<(String, Span)>,
    /// Where a new annotation goes when none is written, with the text that
    /// introduces it (` :: ` or ` => `).
    insert: Option<(usize, &'static str)>,
}

impl AnnotationSite {
    /// Whether the type is written or inferred.
    #[must_use]
    pub const fn declaration(&self) -> DeclarationOrigin {
        if self.written.is_some() {
            DeclarationOrigin::Written
        } else {
            DeclarationOrigin::Inferred
        }
    }

    /// The edit that writes `type_source` as this target's annotation,
    /// replacing what is written or inserting it after the name or
    /// parameter list. `None` when the site has no place for one.
    #[must_use]
    pub fn edit(&self, type_source: &str) -> Option<SourceEdit> {
        let type_source = type_source.trim();
        if let Some((_, span)) = &self.written {
            return Some(SourceEdit {
                span: *span,
                text: type_source.to_string(),
            });
        }
        let (at, prefix) = self.insert?;
        Some(SourceEdit {
            span: Span { start: at, end: at },
            text: format!("{prefix}{type_source}"),
        })
    }
}

/// Whether `type_source` lexes as a type annotation: type names, `class`,
/// `callable`, `limited` with its `min:` and `max:` integer bounds,
/// parentheses, commas and `=>`, and nothing else. Whether it names real
/// classes is for the compiler to say.
#[must_use]
pub fn is_type_source(type_source: &str) -> bool {
    let tokens = significant_tokens(type_source);
    !tokens.is_empty()
        && tokens.iter().any(|token| token.kind == TokenKind::TypeName)
        && tokens.iter().all(|token| match token.kind {
            TokenKind::TypeName
            | TokenKind::Class
            | TokenKind::LParen
            | TokenKind::RParen
            | TokenKind::Comma
            | TokenKind::Arrow
            | TokenKind::Colon
            | TokenKind::Minus
            | TokenKind::Number => true,
            TokenKind::Ident => matches!(
                token.text(type_source),
                "callable" | "limited" | "min" | "max"
            ),
            _ => false,
        })
}

/// Whether two annotation texts spell the same tokens, ignoring layout.
#[must_use]
pub fn same_type_source(left: &str, right: &str) -> bool {
    let left_tokens = significant_tokens(left);
    let right_tokens = significant_tokens(right);
    left_tokens.len() == right_tokens.len()
        && left_tokens
            .iter()
            .zip(&right_tokens)
            .all(|(l, r)| l.kind == r.kind && l.text(left) == r.text(right))
}

fn significant_tokens(source: &str) -> Vec<Token> {
    lex(source)
        .into_iter()
        .filter(|token| !token.kind.is_trivia())
        .collect()
}

/// Finds every annotation target in `module`, in definition order and,
/// within a definition, in source order.
///
/// `tree` must be the syntax `module` was lowered from. A binding whose
/// name does not appear at its span in `tree`'s source, such as one a
/// staged grammar emitted, is left out: there is no source to edit.
#[must_use]
pub fn annotation_sites(tree: &SyntaxTree, module: &ast::Module) -> Vec<AnnotationSite> {
    let mut sites = Vec::new();
    let finder = TokenFinder::new(tree);
    for function in &module.functions {
        let mut binders = Vec::new();
        for param in &function.params {
            binders.push(Binder::from_param(param, AnnotationRole::Param));
        }
        collect_block(&function.body, &mut binders);
        push_binders(&finder, function.definition, binders, &mut sites);
        push_result(
            &finder,
            function.definition,
            function.span,
            function.return_type.as_ref(),
            &mut sites,
        );
    }
    for method in &module.methods {
        let mut binders = Vec::new();
        for param in &method.params {
            binders.push(Binder {
                name: param.name.clone(),
                span: param.span,
                role: AnnotationRole::MethodParam,
                written: finder.annotation_after(param.span),
            });
        }
        collect_block(&method.body, &mut binders);
        push_binders(&finder, method.definition, binders, &mut sites);
        push_result(
            &finder,
            method.definition,
            method.span,
            method.return_type.as_ref(),
            &mut sites,
        );
    }
    for constant in &module.constants {
        let mut binders = Vec::new();
        collect_expr(&constant.value, &mut binders);
        push_binders(&finder, constant.definition, binders, &mut sites);
        let written = constant.type_annotation.as_ref().map(|annotation| {
            (
                finder.text(annotation.span()).to_string(),
                annotation.span(),
            )
        });
        let insert = finder
            .constant_name(constant.span, &constant.name)
            .map(|name| (name.end, " :: "));
        if finder.text(constant.span).starts_with("define")
            && (written.is_some() || insert.is_some())
        {
            sites.push(AnnotationSite {
                target: AnnotationTarget::Result {
                    definition: constant.definition,
                },
                role: AnnotationRole::ConstantValue,
                span: constant.span,
                written,
                insert,
            });
        }
    }
    sites
}

/// One binding name found while walking a definition.
struct Binder {
    name: String,
    span: Span,
    role: AnnotationRole,
    /// Span of the written annotation, if any.
    written: Option<Span>,
}

impl Binder {
    fn from_param(param: &ast::FunctionParam, role: AnnotationRole) -> Self {
        Self {
            name: param.name.clone(),
            span: param.span,
            role,
            written: param.type_annotation.as_ref().map(ast::TypeExpr::span),
        }
    }

    fn from_let(binding: &ast::LetBinding) -> Self {
        Self {
            name: binding.name.clone(),
            span: binding.span,
            role: AnnotationRole::Local,
            written: binding.type_annotation.as_ref().map(ast::TypeExpr::span),
        }
    }
}

fn push_binders(
    finder: &TokenFinder<'_>,
    definition: DefinitionId,
    mut binders: Vec<Binder>,
    sites: &mut Vec<AnnotationSite>,
) {
    binders.sort_by_key(|binder| binder.span.start);
    let mut occurrences = BTreeMap::<String, u32>::new();
    for binder in binders {
        if binder.name.is_empty() || finder.text(binder.span) != binder.name {
            continue;
        }
        let occurrence = occurrences.entry(binder.name.clone()).or_insert(0);
        let target = AnnotationTarget::Binding {
            definition,
            name: binder.name,
            occurrence: *occurrence,
        };
        *occurrence += 1;
        let written = binder
            .written
            .map(|span| (finder.text(span).to_string(), span));
        sites.push(AnnotationSite {
            target,
            role: binder.role,
            span: binder.span,
            insert: written.is_none().then_some((binder.span.end, " :: ")),
            written,
        });
    }
}

fn push_result(
    finder: &TokenFinder<'_>,
    definition: DefinitionId,
    span: Span,
    return_type: Option<&ast::ResultAnnotation>,
    sites: &mut Vec<AnnotationSite>,
) {
    if !finder.text(span).starts_with("define") {
        return;
    }
    let written = return_type.map(|result| (finder.text(result.span).to_string(), result.span));
    let insert = if written.is_some() {
        None
    } else {
        finder.parameter_list_end(span).map(|end| (end, " => "))
    };
    if written.is_none() && insert.is_none() {
        return;
    }
    sites.push(AnnotationSite {
        target: AnnotationTarget::Result { definition },
        role: AnnotationRole::Result,
        span,
        written,
        insert,
    });
}

fn collect_block(block: &ast::Block, binders: &mut Vec<Binder>) {
    for binding in &block.lets {
        match &binding.pattern {
            ast::LetPattern::Name(name) => binders.push(Binder::from_let(name)),
            ast::LetPattern::Tuple { bindings, .. } => {
                binders.extend(bindings.iter().map(Binder::from_let));
            }
        }
        collect_expr(&binding.value, binders);
    }
    collect_expr(&block.result, binders);
}

fn collect_expr(expr: &ast::Expr, binders: &mut Vec<Binder>) {
    use ast::Expr;
    match expr {
        Expr::Number { .. }
        | Expr::Float { .. }
        | Expr::Number32 { .. }
        | Expr::Bool { .. }
        | Expr::String { .. }
        | Expr::Symbol { .. }
        | Expr::Name { .. }
        | Expr::ClassName { .. }
        | Expr::Error { .. } => {}
        Expr::Vector { elements, .. } => {
            for element in elements {
                collect_expr(element, binders);
            }
        }
        Expr::Function { params, body, .. } => {
            for param in params {
                binders.push(Binder::from_param(param, AnnotationRole::ClosureParam));
            }
            collect_block(body, binders);
        }
        Expr::InstanceOf { value, .. } | Expr::Cast { value, .. } => collect_expr(value, binders),
        Expr::Call { args, .. } => {
            for arg in args {
                collect_expr(arg, binders);
            }
        }
        Expr::Signal { condition, .. } => collect_expr(condition, binders),
        Expr::MakeObject { class, fields, .. } => {
            collect_expr(class, binders);
            for field in fields {
                collect_expr(&field.value, binders);
            }
        }
        Expr::SlotRead { receiver, .. } => collect_expr(receiver, binders),
        Expr::SlotAssign {
            receiver, value, ..
        } => {
            collect_expr(receiver, binders);
            collect_expr(value, binders);
        }
        Expr::Binary { lhs, rhs, .. } | Expr::Compare { lhs, rhs, .. } => {
            collect_expr(lhs, binders);
            collect_expr(rhs, binders);
        }
        Expr::UnaryMinus { expr, .. } => collect_expr(expr, binders),
        Expr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            collect_expr(condition, binders);
            collect_expr(then_branch, binders);
            collect_expr(else_branch, binders);
        }
        Expr::Handle { body, restart, .. } => {
            collect_expr(body, binders);
            if let Some(guard) = &restart.guard {
                collect_expr(guard, binders);
            }
            collect_expr(&restart.body, binders);
        }
        Expr::Block { body, cleanup, .. } => {
            collect_block(body, binders);
            if let Some(cleanup) = cleanup {
                collect_block(cleanup, binders);
            }
        }
    }
}

/// Token lookups over one syntax tree's significant tokens.
struct TokenFinder<'a> {
    source: &'a str,
    tokens: Vec<Token>,
}

impl<'a> TokenFinder<'a> {
    fn new(tree: &'a SyntaxTree) -> Self {
        Self {
            source: &tree.source,
            tokens: tree
                .tokens
                .iter()
                .copied()
                .filter(|token| !token.kind.is_trivia())
                .collect(),
        }
    }

    fn text(&self, span: Span) -> &'a str {
        self.source.get(span.start..span.end).unwrap_or_default()
    }

    fn index_at(&self, start: usize) -> Option<usize> {
        self.tokens
            .binary_search_by_key(&start, |token| token.span.start)
            .ok()
    }

    /// The annotation after the name at `name`: the tokens after `::` up
    /// to the `,` or `)` that ends the parameter.
    fn annotation_after(&self, name: Span) -> Option<Span> {
        let index = self.index_at(name.start)?;
        if self.tokens.get(index + 1)?.kind != TokenKind::DoubleColon {
            return None;
        }
        let first = *self.tokens.get(index + 2)?;
        let mut last = first;
        let mut depth = 0_usize;
        for token in &self.tokens[index + 2..] {
            match token.kind {
                TokenKind::LParen => depth += 1,
                TokenKind::RParen if depth == 0 => break,
                TokenKind::RParen => depth -= 1,
                TokenKind::Comma if depth == 0 => break,
                _ => {}
            }
            last = *token;
        }
        Some(first.span.cover(last.span))
    }

    /// The end of the parameter list's `)` in the definition at `span`.
    fn parameter_list_end(&self, span: Span) -> Option<usize> {
        let first = self
            .tokens
            .iter()
            .position(|token| token.span.start >= span.start)?;
        let mut depth = 0_usize;
        for token in &self.tokens[first..] {
            if token.span.start >= span.end {
                return None;
            }
            match token.kind {
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(token.span.end);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// The span of a constant's name within its definition at `span`.
    fn constant_name(&self, span: Span, name: &str) -> Option<Span> {
        self.tokens
            .iter()
            .filter(|token| token.span.start >= span.start && token.span.end <= span.end)
            .skip_while(|token| token.kind != TokenKind::Constant)
            .find(|token| token.kind == TokenKind::Ident && self.text(token.span) == name)
            .map(|token| token.span)
    }
}

/// One typing decision as a delta sees it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecisionSnapshot {
    /// The decision's EOP id, `typing:<definition>:s<n>`.
    pub id: String,
    /// Definition whose body holds the check.
    pub owner: DefinitionId,
    /// Check site within `owner`.
    pub site: SiteId,
    /// Kind of runtime check.
    pub check: TypingCheck,
    /// Classification of the check.
    pub verdict: TypingVerdict,
    /// Severity the typing configuration assigns.
    pub severity: TypingSeverity,
    /// What generated code does with the check.
    pub codegen: CheckCodegen,
    /// Source span of the checked expression.
    pub span: Span,
}

/// How one decision differs between two compilations.
///
/// `before` is `None` for a check the change introduced, such as the cast
/// a newly annotated `let` makes; `after` is `None` for one it removed,
/// such as a dispatch that became direct arithmetic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecisionChange {
    /// The decision before the change.
    pub before: Option<DecisionSnapshot>,
    /// The decision after the change.
    pub after: Option<DecisionSnapshot>,
}

impl DecisionChange {
    /// The decision's id after the change, or before it when removed.
    #[must_use]
    pub fn id(&self) -> &str {
        self.after
            .as_ref()
            .or(self.before.as_ref())
            .map_or("", |snapshot| snapshot.id.as_str())
    }
}

/// Counts of verdicts and codegen facets over every decision.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct TypingTally {
    /// Decisions the static types prove.
    pub proved: usize,
    /// Decisions a runtime check decides.
    pub checked: usize,
    /// Decisions that always fail when reached.
    pub refuted: usize,
    /// Checks generated code keeps.
    pub kept: usize,
    /// Checks generated code drops.
    pub elided: usize,
    /// Operations generated code keeps without their check.
    pub specialized: usize,
    /// Numeric dispatches replaced by their method's operation.
    pub inlined: usize,
    /// Dispatches that call their method through a relinkable link.
    pub linked: usize,
    /// Decisions reported as errors, such as a strict definition's
    /// unproved checks.
    pub errors: usize,
}

impl TypingTally {
    fn add(&mut self, snapshot: &DecisionSnapshot) {
        match snapshot.verdict {
            TypingVerdict::Proved => self.proved += 1,
            TypingVerdict::Checked => self.checked += 1,
            TypingVerdict::Refuted => self.refuted += 1,
        }
        match snapshot.codegen {
            CheckCodegen::Kept => self.kept += 1,
            CheckCodegen::Elided => self.elided += 1,
            CheckCodegen::Specialized => self.specialized += 1,
            CheckCodegen::Inlined => self.inlined += 1,
            CheckCodegen::Linked => self.linked += 1,
        }
        if snapshot.severity == TypingSeverity::Error {
            self.errors += 1;
        }
    }
}

/// Which typing decisions changed verdict, severity, or codegen between two
/// compilations: the payoff of an annotation or a policy change.
///
/// Decisions are matched by owner, check kind, and source span mapped
/// across the edit, not by site number, since an annotation can add a
/// check and renumber the sites after it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TypingDelta {
    /// Decisions that changed, appeared, or disappeared, in source order.
    pub changes: Vec<DecisionChange>,
    /// Decisions matched with the same verdict, severity, and codegen.
    pub unchanged: usize,
    /// Counts before the change.
    pub before: TypingTally,
    /// Counts after the change.
    pub after: TypingTally,
}

impl TypingDelta {
    /// Compares `before` with `after`. `map` carries a decision's owner and
    /// span from the old compilation into the new one.
    #[must_use]
    pub fn between(
        before: Vec<DecisionSnapshot>,
        after: Vec<DecisionSnapshot>,
        map: impl Fn(DefinitionId, Span) -> (DefinitionId, Span),
    ) -> Self {
        type Key = (DefinitionId, &'static str, usize, usize);
        let key = |owner: DefinitionId, check: TypingCheck, span: Span| -> Key {
            (owner, check.as_str(), span.start, span.end)
        };
        let mut delta = Self::default();
        for snapshot in &before {
            delta.before.add(snapshot);
        }
        for snapshot in &after {
            delta.after.add(snapshot);
        }
        let mut pending = BTreeMap::<Key, Vec<DecisionSnapshot>>::new();
        for snapshot in before.into_iter().rev() {
            let (owner, span) = map(snapshot.owner, snapshot.span);
            pending
                .entry(key(owner, snapshot.check, span))
                .or_default()
                .push(snapshot);
        }
        for snapshot in after {
            let matched = pending
                .get_mut(&key(snapshot.owner, snapshot.check, snapshot.span))
                .and_then(Vec::pop);
            match matched {
                Some(old)
                    if old.verdict == snapshot.verdict
                        && old.severity == snapshot.severity
                        && old.codegen == snapshot.codegen =>
                {
                    delta.unchanged += 1;
                }
                old => delta.changes.push(DecisionChange {
                    before: old,
                    after: Some(snapshot),
                }),
            }
        }
        for old in pending.into_values().flatten() {
            delta.changes.push(DecisionChange {
                before: Some(old),
                after: None,
            });
        }
        delta.changes.sort_by_key(|change| {
            change.after.as_ref().map_or_else(
                || {
                    change
                        .before
                        .as_ref()
                        .map_or(0, |old| map(old.owner, old.span).1.start)
                },
                |new| new.span.start,
            )
        });
        delta
    }

    /// Whether nothing changed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Renders the delta: a summary line, the verdict, codegen, and error
    /// counts before and after, then one line per change, such as
    /// `typing:def…:s0 instance verdict=checked->proved codegen=kept->elided`.
    #[must_use]
    pub fn render(&self) -> String {
        let count =
            |pick: fn(&DecisionChange) -> bool| self.changes.iter().filter(|c| pick(c)).count();
        let mut out = String::new();
        let _ = writeln!(
            out,
            "typing delta: changed={} added={} removed={} unchanged={}",
            count(|change| change.before.is_some() && change.after.is_some()),
            count(|change| change.before.is_none()),
            count(|change| change.after.is_none()),
            self.unchanged
        );
        let (b, a) = (self.before, self.after);
        let _ = writeln!(
            out,
            "  verdicts proved {}->{} checked {}->{} refuted {}->{}",
            b.proved, a.proved, b.checked, a.checked, b.refuted, a.refuted
        );
        let _ = writeln!(
            out,
            "  codegen kept {}->{} elided {}->{} specialized {}->{} inlined {}->{} linked {}->{}",
            b.kept,
            a.kept,
            b.elided,
            a.elided,
            b.specialized,
            a.specialized,
            b.inlined,
            a.inlined,
            b.linked,
            a.linked
        );
        let _ = writeln!(out, "  errors {}->{}", b.errors, a.errors);
        for change in &self.changes {
            let _ = writeln!(out, "  {}", render_change(change));
        }
        out
    }
}

fn render_change(change: &DecisionChange) -> String {
    fn pair(before: &str, after: &str) -> String {
        if before == after {
            before.to_string()
        } else {
            format!("{before}->{after}")
        }
    }
    match (&change.before, &change.after) {
        (Some(old), Some(new)) => format!(
            "{} {} verdict={} codegen={} severity={}",
            new.id,
            new.check.as_str(),
            pair(old.verdict.as_str(), new.verdict.as_str()),
            pair(old.codegen.as_str(), new.codegen.as_str()),
            pair(old.severity.as_str(), new.severity.as_str()),
        ),
        (None, Some(new)) => format!(
            "{} {} added verdict={} codegen={} severity={}",
            new.id,
            new.check.as_str(),
            new.verdict.as_str(),
            new.codegen.as_str(),
            new.severity.as_str(),
        ),
        (Some(old), None) => format!(
            "{} {} removed verdict={} codegen={} severity={}",
            old.id,
            old.check.as_str(),
            old.verdict.as_str(),
            old.codegen.as_str(),
            old.severity.as_str(),
        ),
        (None, None) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_round_trip_through_their_ids() {
        let definition = DefinitionId(0x0123_4567_89ab_cdef);
        for target in [
            AnnotationTarget::Binding {
                definition,
                name: "x".into(),
                occurrence: 0,
            },
            AnnotationTarget::Binding {
                definition,
                name: "count-down".into(),
                occurrence: 2,
            },
            AnnotationTarget::Result { definition },
        ] {
            let id = target.to_string();
            assert_eq!(AnnotationTarget::parse(&id), Some(target), "{id}");
        }
        assert_eq!(
            AnnotationTarget::parse("binding:def0123456789abcdef:x#0").map(|t| t.to_string()),
            Some("binding:def0123456789abcdef:x".into())
        );
        for bad in [
            "binding:def0123:x",
            "binding:def0123456789abcdef:",
            "binding:def0123456789abcdef:x#y",
            "result:0123456789abcdef",
            "local:def0123456789abcdef:x",
        ] {
            assert_eq!(AnnotationTarget::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn edits_map_offsets_across_the_replaced_range() {
        let edit = SourceEdit {
            span: Span { start: 4, end: 6 },
            text: "abcde".into(),
        };
        assert_eq!(edit.apply("0123456789").as_deref(), Some("0123abcde6789"));
        assert_eq!(edit.map_offset(2), 2);
        assert_eq!(edit.map_offset(4), 4);
        assert_eq!(edit.map_offset(5), 4);
        assert_eq!(edit.map_offset(6), 9);
        assert_eq!(edit.map_offset(8), 11);
        assert_eq!(
            SourceEdit {
                span: Span { start: 20, end: 20 },
                text: String::new()
            }
            .apply("short"),
            None
        );
    }

    #[test]
    fn type_sources_admit_only_type_tokens() {
        for good in [
            "<integer>",
            " class <shape> ",
            "callable(<integer>) => <circle>",
            "(<integer>, <integer>)",
            "limited(<integer>, min: -1, max: 255)",
        ] {
            assert!(is_type_source(good), "{good}");
        }
        for bad in [
            "",
            "x",
            "<integer> = 3",
            "<integer>; define",
            "callable",
            "1",
            "limited(<integer>, min: n)",
        ] {
            assert!(!is_type_source(bad), "{bad}");
        }
        assert!(same_type_source(
            "callable(<integer>)=><a>",
            "callable( <integer> ) => <a>"
        ));
        assert!(!same_type_source("<a>", "<b>"));
    }
}
