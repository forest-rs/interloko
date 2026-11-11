//! Self-contained HTML report generation for `interloko_lab`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use interloko_compiler::diagnostic::{Diagnostic, DiagnosticLabelStyle};
use interloko_compiler::grammar_frontend::{
    self, FrontendExpression, FrontendForm, FrontendFormKind, ProjectionOrigin, StagedParseRegion,
};
use interloko_compiler::incremental::{ArtifactReport, QueryArtifact};
use interloko_compiler::sem::{
    self, ClassOrigin, GenericFunctionOrigin, MethodOrigin, SlotMutability, SlotOrigin, ValueKind,
};
use interloko_compiler::session::recording::{HostOutcome, Outcome};
use interloko_compiler::session::{
    DumpStage, EnvironmentKind, EnvironmentObject, EnvironmentOrigin, EnvironmentStorage,
    ExecutionTraceView, RuntimeResult, SessionArtifacts, SourceFormOrigin, SourceFormProvenance,
    TracedLocation, TracedStack,
};
use interloko_compiler::syntax::Span;

use super::CompiledWorkbench;

/// Writes a self-contained HTML report for a compiled workbench.
pub(crate) fn write_report(path: &Path, workbench: &CompiledWorkbench) -> std::io::Result<()> {
    fs::write(path, render_report(workbench))
}

fn render_report(workbench: &CompiledWorkbench) -> String {
    let artifacts = &workbench.artifacts;
    let objects = workbench.environment_objects();
    let related_objects = related_objects_by_id(artifacts);
    let mut out = String::new();

    out.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n");
    out.push_str("  <meta charset=\"utf-8\">\n");
    out.push_str("  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    let _ = writeln!(
        out,
        "  <title>Interloko Report - {}</title>",
        escape_html(&artifacts.source.name)
    );
    out.push_str("  <style>\n");
    out.push_str(REPORT_CSS);
    out.push_str("  </style>\n</head>\n<body>\n");
    out.push_str("  <div class=\"report-shell\">\n");
    out.push_str(&render_hero(artifacts));
    out.push_str("    <main class=\"report-main\">\n");
    out.push_str(&render_source_section(artifacts));
    out.push_str(&render_parse_provenance_section(artifacts));
    out.push_str(&render_grammar_environment_section(artifacts, &objects));
    out.push_str(&render_staged_section(artifacts, &objects));
    out.push_str(&render_model_section(artifacts, &objects, &related_objects));
    out.push_str(&render_diagnostics_section(artifacts));
    out.push_str(&render_artifacts_section(artifacts));
    out.push_str(&render_trace_section(artifacts));
    out.push_str(&render_ir_section(artifacts));
    out.push_str("    </main>\n");
    out.push_str("  </div>\n");
    let _ = writeln!(
        out,
        "  <script type=\"application/json\" id=\"interloko-report-data\">{}</script>",
        render_report_json(artifacts, &objects)
    );
    out.push_str("  <script>\n");
    out.push_str(REPORT_JS);
    out.push_str("  </script>\n</body>\n</html>\n");
    out
}

fn render_hero(artifacts: &SessionArtifacts) -> String {
    let mut out = String::new();
    let runtime = runtime_summary(artifacts.runtime_result.as_ref());
    let _ = writeln!(
        out,
        "    <header class=\"hero\" data-node=\"source:{}\">",
        escape_attr(&artifacts.source.id.to_string())
    );
    out.push_str("      <div class=\"hero__copy\">\n");
    out.push_str("        <p class=\"eyebrow\">Interloko compiler workbench report</p>\n");
    let _ = writeln!(
        out,
        "        <h1>{}</h1>",
        escape_html(short_source_name(&artifacts.source.name))
    );
    let _ = writeln!(
        out,
        "        <p class=\"hero__summary\">{} source bytes moved through syntax, AST, semantic IR, lowered IR, tape emission, grammar MOP, and EOP projection.</p>",
        artifacts.source.source.len()
    );
    out.push_str("      </div>\n");
    out.push_str("      <div class=\"hero__metrics\" aria-label=\"report summary\">\n");
    metric(&mut out, "Runtime result", runtime.as_str(), runtime.class);
    metric(
        &mut out,
        "Diagnostics",
        &artifacts.diagnostics.len().to_string(),
        if artifacts.diagnostics.is_empty() {
            "metric--ok"
        } else {
            "metric--warn"
        },
    );
    metric(
        &mut out,
        "Grammar stack",
        &grammar_stack_summary(artifacts),
        "metric--neutral",
    );
    metric(
        &mut out,
        "Staged regions",
        &artifacts.grammar_parse.staged_regions.len().to_string(),
        if artifacts.grammar_parse.staged_regions.is_empty() {
            "metric--empty"
        } else {
            "metric--accent"
        },
    );
    out.push_str("      </div>\n");
    out.push_str("    </header>\n");
    out
}

fn metric(out: &mut String, label: &str, value: &str, class: &str) {
    let _ = writeln!(
        out,
        "        <div class=\"metric {class}\"><span>{}</span><strong>{}</strong></div>",
        escape_html(label),
        escape_html(value)
    );
}

fn render_pipeline(artifacts: &SessionArtifacts) -> String {
    let reports = artifacts.artifact_reports();
    let stages = [
        (QueryArtifact::Source, "Source"),
        (QueryArtifact::EnvironmentResolution, "Env"),
        (QueryArtifact::GrammarStack, "Grammar"),
        (QueryArtifact::GrammarParse, "Grammar parse"),
        (QueryArtifact::Parse, "Syntax"),
        (QueryArtifact::Ast, "AST"),
        (QueryArtifact::Bind, "Sem"),
        (QueryArtifact::LoweredModule, "Lowered"),
        (QueryArtifact::TapeProgram, "Tape"),
        (QueryArtifact::RuntimeResult, "Runtime"),
    ];
    let mut out = String::new();
    out.push_str("        <ol class=\"pipeline\" aria-label=\"artifact pipeline\">\n");
    for (artifact, label) in stages {
        let key = report_for(&reports, artifact)
            .map(|report| report.key.to_string())
            .unwrap_or_else(|| "not emitted".to_string());
        let metrics = artifacts.metrics_for(artifact);
        let relation = artifact_relation_tokens(artifacts, artifact);
        let _ = writeln!(
            out,
            "          <li class=\"pipeline__node\" data-node=\"artifact:{}\" data-rel=\"{}\"><span>{}</span><code>{}</code><small>{} · {} work</small></li>",
            escape_attr(&key),
            escape_attr(&relation),
            escape_html(label),
            escape_html(&key),
            escape_html(&format_duration_nanos(metrics.elapsed_nanos)),
            metrics.work_units
        );
    }
    out.push_str("        </ol>\n");
    out
}

fn render_section_heading(eyebrow: &str, title: &str, intro: &str) -> String {
    let mut out = String::new();
    out.push_str("        <div class=\"section__heading\">\n");
    out.push_str("          <div>\n");
    let _ = writeln!(
        out,
        "            <p class=\"eyebrow\">{}</p>",
        escape_html(eyebrow)
    );
    let _ = writeln!(out, "            <h2>{}</h2>", escape_html(title));
    out.push_str("          </div>\n");
    let _ = writeln!(
        out,
        "          <p class=\"section__intro\">{}</p>",
        escape_html(intro)
    );
    out.push_str("        </div>\n");
    out
}

fn render_source_section(artifacts: &SessionArtifacts) -> String {
    let mut out = String::new();
    out.push_str("      <section class=\"section section--source\" id=\"source\">\n");
    out.push_str(&render_section_heading(
        "Original source",
        "Source With Provenance",
        "This is the file the compiler saw. Highlights show grammar directives, staged regions, generated frontend forms, and diagnostics; click a highlighted span to follow the related compiler objects.",
    ));
    out.push_str(&render_source_legend(artifacts));
    out.push_str("        <div class=\"source-frame\">\n");
    out.push_str(&render_source_lines(artifacts));
    out.push_str("        </div>\n");
    out.push_str("      </section>\n");
    out
}

fn render_grammar_environment_section(
    artifacts: &SessionArtifacts,
    objects: &[EnvironmentObject],
) -> String {
    let environment = &artifacts.grammar_parse.environment;
    let environment_id = grammar_frontend::language_environment_object_id(environment);
    let mut out = String::new();
    out.push_str("      <section class=\"section\" id=\"grammar-environment\">\n");
    out.push_str(&render_section_heading(
        "Grammar environment",
        "Active Language Environment",
        "The language environment owns the grammar stack that parsed this source. The registry is shown as host/catalog plumbing; the environment and grammars are the objects a user follows.",
    ));
    out.push_str("        <div class=\"grammar-layout\">\n");
    out.push_str("          <article class=\"grammar-panel grammar-panel--environment\">\n");
    let _ = writeln!(
        out,
        "            <div class=\"grammar-panel__title\"><span>{}</span><code><a href=\"#{}\">{}</a></code></div>",
        escape_html(&environment.name),
        escape_attr(&object_anchor(&environment_id)),
        escape_html(&environment_id)
    );
    out.push_str("            <p>Selected environment for this compile. It resolves grammar inheritance, activations, and staged-region provenance before AST lowering begins.</p>\n");
    out.push_str("            <ol class=\"grammar-stack-list\">\n");
    for (index, grammar_id) in environment.grammar_stack.iter().enumerate() {
        if let Some(grammar) = artifacts
            .grammar_parse
            .grammars
            .iter()
            .find(|grammar| grammar.id == *grammar_id)
        {
            let grammar_object_id = grammar_frontend::grammar_object_id(grammar);
            let role = if index + 1 == environment.grammar_stack.len() {
                "active"
            } else {
                "base"
            };
            let source_kind = grammar_source_kind(artifacts, grammar.id);
            let activation_count = artifacts
                .grammar_parse
                .activations
                .iter()
                .filter(|activation| activation.grammar == grammar.id)
                .count();
            let _ = writeln!(
                out,
                "              <li data-node=\"object:{}\"><a href=\"#{}\">{}</a><span>{}</span><span>{}</span><small>{} direct · {} effective · {} activations</small></li>",
                escape_attr(&grammar_object_id),
                escape_attr(&object_anchor(&grammar_object_id)),
                escape_html(&grammar.name),
                role,
                source_kind,
                grammar.direct_rules.len(),
                grammar.effective_rules.len(),
                activation_count
            );
        }
    }
    out.push_str("            </ol>\n");
    out.push_str("          </article>\n");
    out.push_str("          <article class=\"grammar-panel grammar-panel--registry\">\n");
    out.push_str(
        "            <div class=\"grammar-panel__title\"><span>Host grammar catalog</span>",
    );
    if let Some(registry) = object_by_id(objects, "grammar-registry:session") {
        let _ = writeln!(
            out,
            "<code><a href=\"#{}\">{}</a></code></div>",
            escape_attr(&object_anchor(registry.id.as_str())),
            escape_html(registry.id.as_str())
        );
    } else {
        out.push_str("<code>grammar-registry:session</code></div>\n");
    }
    out.push_str("            <p>Registry entries are trusted host-side catalog entries used to choose environments and staged grammar extensions. They are inspectable, but they are not the active parsing context.</p>\n");
    out.push_str("            <div class=\"grammar-registry-links\">\n");
    for object in objects.iter().filter(|object| {
        matches!(
            object.kind,
            EnvironmentKind::RegisteredGrammarEnvironment
                | EnvironmentKind::StagedGrammarAdapter
                | EnvironmentKind::GrammarExtension
        )
    }) {
        let _ = writeln!(
            out,
            "              <a href=\"#{}\"><span>{}</span><code>{}</code></a>",
            escape_attr(&object_anchor(object.id.as_str())),
            escape_html(&object.presentation.title),
            escape_html(object.id.as_str())
        );
    }
    out.push_str("            </div>\n");
    out.push_str("          </article>\n");
    out.push_str("        </div>\n");
    out.push_str("      </section>\n");
    out
}

fn render_source_legend(artifacts: &SessionArtifacts) -> String {
    let has_staged_regions = !artifacts.grammar_parse.staged_regions.is_empty();
    let has_diagnostics = !artifacts.diagnostics.is_empty();
    if !has_staged_regions && !has_diagnostics {
        return String::new();
    }

    let mut out = String::new();
    out.push_str("        <div class=\"source-legend\" aria-label=\"source highlight legend\">\n");
    if has_staged_regions {
        out.push_str("          <span><i class=\"legend-swatch legend-swatch--activation\"></i>activation directive</span>\n");
        out.push_str("          <span><i class=\"legend-swatch legend-swatch--staged\"></i>staged source region</span>\n");
        out.push_str(
            "          <span><i class=\"legend-swatch legend-swatch--form\"></i>frontend form source</span>\n",
        );
    }
    if has_diagnostics {
        out.push_str("          <span><i class=\"legend-swatch legend-swatch--diagnostic\"></i>diagnostic span</span>\n");
    }
    out.push_str("        </div>\n");
    out
}

fn render_source_lines(artifacts: &SessionArtifacts) -> String {
    let bands = source_bands(artifacts);
    let marks = source_marks(artifacts);
    let mut out = String::new();
    out.push_str("          <pre class=\"source-code\"><code>\n");
    if artifacts.source.source.is_empty() {
        out.push_str("<span class=\"source-line source-line--empty\"><span class=\"source-line__num\">1</span><span class=\"source-line__text\">empty source</span></span>\n");
        out.push_str("          </code></pre>\n");
        return out;
    }

    let mut line_start = 0;
    for (line_index, line_with_break) in artifacts.source.source.split_inclusive('\n').enumerate() {
        let content = line_with_break
            .strip_suffix('\n')
            .unwrap_or(line_with_break)
            .strip_suffix('\r')
            .unwrap_or_else(|| {
                line_with_break
                    .strip_suffix('\n')
                    .unwrap_or(line_with_break)
            });
        let line_end = line_start + content.len();
        let relations = intersecting_relations(&bands, line_start, line_end);
        let class = if relations.is_empty() {
            "source-line"
        } else {
            "source-line source-line--staged"
        };
        let _ = write!(
            out,
            "<span class=\"{class}\" id=\"source-line-{}\" data-rel=\"{}\"><span class=\"source-line__num\">{}</span><span class=\"source-line__text\">",
            line_index + 1,
            escape_attr(&relations.join(" ")),
            line_index + 1
        );
        out.push_str(&render_source_segments(
            &artifacts.source.source,
            content,
            line_start,
            line_end,
            &marks,
        ));
        out.push_str("</span></span>\n");
        line_start += line_with_break.len();
    }
    out.push_str("          </code></pre>\n");
    out
}

fn render_parse_provenance_section(artifacts: &SessionArtifacts) -> String {
    let forms = artifacts.source_forms();
    let mut out = String::new();
    out.push_str("      <section class=\"section\" id=\"parse-provenance\">\n");
    out.push_str(&render_section_heading(
        "Parse provenance",
        "Source To Parse Rules",
        "Each top-level form is shown as a source span, token stream, CST path, grammar rule stack, and local diagnostics. This is the workbench's static parse lens: a compact version of the grammar-editor view that explains how text became syntax.",
    ));
    if forms.is_empty() {
        out.push_str(
            "        <div class=\"empty-state\">No top-level source forms were parsed.</div>\n",
        );
        out.push_str("      </section>\n");
        return out;
    }
    out.push_str("        <div class=\"parse-grid\">\n");
    for form in forms {
        out.push_str(&render_parse_form_card(artifacts, &form));
    }
    out.push_str("        </div>\n");
    out.push_str("      </section>\n");
    out
}

fn render_parse_form_card(artifacts: &SessionArtifacts, form: &SourceFormProvenance) -> String {
    let line = source_line_for_byte(&artifacts.source.source, form.span.start);
    let form_anchor = parse_form_anchor(&form.id);
    let origin_class = if form.frontend_form.is_some() {
        " parse-card--staged"
    } else {
        ""
    };
    let mut out = String::new();
    let _ = writeln!(
        out,
        "          <article class=\"parse-card{}\" id=\"{}\" data-node=\"{}\">",
        origin_class,
        escape_attr(&form_anchor),
        escape_attr(&form.id)
    );
    let _ = writeln!(
        out,
        "            <header class=\"parse-card__header\"><div><span>{}</span><strong>{}</strong></div><a href=\"#source-line-{}\">line {}</a><code>{}..{}</code></header>",
        escape_html(&form.id),
        escape_html(&form.title),
        line,
        line,
        form.span.start,
        form.span.end
    );
    out.push_str(&render_doc_comment_block(
        "source documentation",
        &form.doc_comments,
    ));
    out.push_str("            <div class=\"parse-card__body\">\n");
    out.push_str("              <pre class=\"parse-source\"><code>");
    out.push_str(&render_parse_source_excerpt(
        &artifacts.source.source,
        form.span,
        &artifacts.parse_view_for_span(form.span).tokens,
        &form.rule_applications,
    ));
    out.push_str("</code></pre>\n");
    out.push_str("              <div class=\"parse-lens\">\n");
    if let Some(frontend_form) = &form.frontend_form {
        out.push_str("                <div class=\"parse-lens__label\">staged grammar</div>\n");
        out.push_str(&render_source_form_staged_meta(form));
        out.push_str("                <div class=\"parse-lens__label\">frontend form</div>\n");
        out.push_str(&render_staged_frontend_form_summary(frontend_form));
        out.push_str("                <div class=\"parse-lens__label\">materialized Loko</div>\n");
        let materialized =
            grammar_frontend::materialize_frontend_forms(core::slice::from_ref(frontend_form));
        let _ = writeln!(
            out,
            "                <pre class=\"parse-materialized\"><code>{}</code></pre>",
            escape_html(&materialized)
        );
    }
    out.push_str("                <div class=\"parse-lens__label\">rule stack</div>\n");
    out.push_str(&render_rule_ladder(
        artifacts,
        form.span,
        &form.rule_applications,
    ));
    if form.frontend_form.is_none() {
        let view = artifacts.parse_view_for_span(form.span);
        out.push_str("                <div class=\"parse-lens__label\">tokens</div>\n");
        out.push_str(&render_token_strip(&view.tokens));
        out.push_str("                <div class=\"parse-lens__label\">cst path</div>\n");
        out.push_str("                <ol class=\"parse-cst-path\">\n");
        for node in &view.nodes {
            let _ = writeln!(
                out,
                "                  <li style=\"--depth:{}\"><span>{:?}</span><code>{}..{}</code></li>",
                node.depth, node.kind, node.span.start, node.span.end
            );
        }
        out.push_str("                </ol>\n");
        out.push_str("                <div class=\"parse-lens__label\">diagnostics</div>\n");
        out.push_str(&render_parse_diagnostics(&view.diagnostics));
    }
    out.push_str("              </div>\n");
    out.push_str("            </div>\n");
    out.push_str("          </article>\n");
    out
}

fn render_staged_frontend_form_summary(form: &FrontendForm) -> String {
    let mut out = String::new();
    match &form.kind {
        FrontendFormKind::Function(function) => {
            let _ = writeln!(
                out,
                "                <div class=\"parse-frontend-form\"><strong>frontend-function</strong><code>{}</code><small>body {}</small></div>",
                escape_html(&function.name),
                escape_html(&frontend_expression_label(&function.body))
            );
        }
        FrontendFormKind::Class(class) => {
            let _ = writeln!(
                out,
                "                <div class=\"parse-frontend-form\"><strong>frontend-class</strong><code>{} extends {}</code>",
                escape_html(&class.name),
                escape_html(&class.superclass)
            );
            out.push_str("                  <ul class=\"slot-list\">\n");
            for slot in &class.slots {
                let _ = writeln!(
                    out,
                    "                    <li><code>{}</code><span>type {}</span><span>span {}..{}</span></li>",
                    escape_html(&slot.name),
                    escape_html(&slot.type_name),
                    slot.source_span.start,
                    slot.source_span.end
                );
            }
            out.push_str("                  </ul>\n");
            out.push_str("                </div>\n");
        }
    }
    out
}

fn render_doc_comment_block(label: &str, lines: &[String]) -> String {
    if lines.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    let _ = writeln!(
        out,
        "            <div class=\"doc-comment-block\"><span>{}</span>",
        escape_html(label)
    );
    for line in lines {
        let _ = writeln!(out, "              <p>{}</p>", escape_html(line));
    }
    out.push_str("            </div>\n");
    out
}

fn render_source_form_staged_meta(form: &SourceFormProvenance) -> String {
    let activation = form.activation.map_or_else(
        || "activation:<unknown>".to_string(),
        |activation| activation.to_string(),
    );
    let mut out = String::new();
    let grammar_label = form.grammar.map_or_else(
        || "grammar:<unknown>".to_string(),
        |grammar| grammar.to_string(),
    );
    let _ = writeln!(
        out,
        "                <div class=\"parse-staged-meta\"><span>{}</span><code>{}</code><span>{}</span></div>",
        escape_html(&grammar_label),
        escape_html(&activation),
        escape_html(&source_form_origin_label(&form.origin))
    );
    out
}

fn render_rule_ladder(
    artifacts: &SessionArtifacts,
    form_span: Span,
    applications: &[interloko_compiler::grammar_frontend::GrammarRuleApplication],
) -> String {
    let mut out = String::new();
    out.push_str("                <div class=\"parse-rule-ladder\">\n");
    if applications.is_empty() {
        out.push_str(
            "                  <div class=\"parse-empty\">No rule applications recorded.</div>\n",
        );
    } else {
        let mut rules = applications.to_vec();
        rules.sort_by_key(|application| {
            (
                application.span.start,
                core::cmp::Reverse(application.span.end.saturating_sub(application.span.start)),
                application.rule_name.clone(),
            )
        });
        for application in &rules {
            let object_id = effective_rule_object_id_for_application(artifacts, application);
            let href = object_id.as_deref().map(object_anchor);
            let depth = parse_rule_depth(&rules, application.span);
            let relation = parse_rule_relation_key(form_span, application);
            let _ = write!(
                out,
                "                  <div class=\"parse-rule-row\" data-rel=\"{}\" style=\"--rule-depth:{depth}; {}\">",
                escape_attr(&relation),
                parse_rule_bar_style(form_span, application.span)
            );
            if let Some(href) = href {
                let _ = write!(
                    out,
                    "<a href=\"#{}\"><strong>{}</strong><small>{}</small></a>",
                    escape_attr(&href),
                    escape_html(&application.rule_name),
                    escape_html(object_id.as_deref().unwrap_or(""))
                );
            } else {
                let _ = write!(
                    out,
                    "<span><strong>{}</strong></span>",
                    escape_html(&application.rule_name)
                );
            }
            let _ = writeln!(
                out,
                "<code>{}..{}</code></div>",
                application.span.start, application.span.end
            );
        }
    }
    out.push_str("                </div>\n");
    out
}

fn render_token_strip(tokens: &[interloko_compiler::session::SyntaxTokenSummary]) -> String {
    let mut out = String::new();
    out.push_str("                <div class=\"parse-token-strip\">\n");
    if tokens.is_empty() {
        out.push_str("                  <span class=\"parse-empty\">No tokens.</span>\n");
    } else {
        for token in tokens.iter().take(48) {
            let _ = writeln!(
                out,
                "                  <span class=\"parse-token\"><strong>{}</strong><code>{:?}</code></span>",
                escape_html(&token.text),
                token.kind
            );
        }
        if tokens.len() > 48 {
            let _ = writeln!(
                out,
                "                  <span class=\"parse-token parse-token--more\">{} more</span>",
                tokens.len() - 48
            );
        }
    }
    out.push_str("                </div>\n");
    out
}

fn render_parse_source_excerpt(
    source: &str,
    form_span: Span,
    tokens: &[interloko_compiler::session::SyntaxTokenSummary],
    applications: &[interloko_compiler::grammar_frontend::GrammarRuleApplication],
) -> String {
    if tokens.is_empty() {
        let relation = applications
            .iter()
            .map(|application| parse_rule_relation_key(form_span, application))
            .collect::<Vec<_>>()
            .join(" ");
        return format!(
            "<mark class=\"parse-source-token\" data-rel=\"{}\" title=\"{}\">{}</mark>",
            escape_attr(&relation),
            escape_attr(&format!(
                "source span {}..{}",
                form_span.start, form_span.end
            )),
            escape_html(source.get(form_span.start..form_span.end).unwrap_or(""))
        );
    }

    let mut out = String::new();
    let mut cursor = form_span.start;
    for token in tokens {
        if token.span.end <= form_span.start || form_span.end <= token.span.start {
            continue;
        }
        let start = token.span.start.max(form_span.start);
        let end = token.span.end.min(form_span.end);
        if start > cursor {
            out.push_str(&escape_html(source.get(cursor..start).unwrap_or("")));
        }
        let relations = applications
            .iter()
            .filter(|application| {
                application.span.start <= token.span.start && token.span.end <= application.span.end
            })
            .map(|application| parse_rule_relation_key(form_span, application))
            .collect::<Vec<_>>();
        let relation = relations.join(" ");
        let _ = write!(
            out,
            "<mark class=\"parse-source-token\" data-rel=\"{}\" title=\"{}\">{}</mark>",
            escape_attr(&relation),
            escape_attr(&format!(
                "{:?} {}..{}",
                token.kind, token.span.start, token.span.end
            )),
            escape_html(source.get(start..end).unwrap_or(""))
        );
        cursor = end;
    }
    if cursor < form_span.end {
        out.push_str(&escape_html(
            source.get(cursor..form_span.end).unwrap_or(""),
        ));
    }
    out
}

fn render_parse_diagnostics(
    diagnostics: &[interloko_compiler::grammar_frontend::GrammarDiagnosticFact],
) -> String {
    let mut out = String::new();
    out.push_str("                <div class=\"parse-diagnostics\">\n");
    if diagnostics.is_empty() {
        out.push_str("                  <span class=\"parse-empty\">No parser diagnostics touched this form.</span>\n");
    } else {
        for diagnostic in diagnostics {
            let unexpected = diagnostic.unexpected_fragment.as_deref().unwrap_or("none");
            let expected = diagnostic.expected_fragment.as_deref().unwrap_or("unknown");
            let _ = writeln!(
                out,
                "                  <div class=\"parse-diagnostic\"><strong>{}</strong><span>{}..{}</span><small>unexpected {} · expected {}</small></div>",
                diagnostic.code,
                diagnostic.span.start,
                diagnostic.span.end,
                escape_html(unexpected),
                escape_html(expected)
            );
        }
    }
    out.push_str("                </div>\n");
    out
}

fn render_source_segments(
    source: &str,
    content: &str,
    line_start: usize,
    line_end: usize,
    marks: &[SourceMark],
) -> String {
    let mut line_marks = marks
        .iter()
        .filter_map(|mark| mark.clipped(line_start, line_end))
        .collect::<Vec<_>>();
    line_marks.sort_by_key(|mark| (mark.start, mark.end));
    let mut out = String::new();
    let mut cursor = line_start;
    for mark in line_marks {
        if mark.end <= cursor {
            continue;
        }
        let start = mark.start.max(cursor);
        if start > cursor {
            out.push_str(&escape_html(source.get(cursor..start).unwrap_or("")));
        }
        let end = mark.end.max(start).min(line_end);
        let _ = write!(
            out,
            "<mark class=\"source-mark {}\" data-rel=\"{}\" title=\"{}\">{}</mark>",
            mark.class,
            escape_attr(&mark.relation),
            escape_attr(mark.label),
            escape_html(source.get(start..end).unwrap_or(""))
        );
        cursor = end;
    }
    if cursor < line_end {
        out.push_str(&escape_html(source.get(cursor..line_end).unwrap_or("")));
    }
    if line_start == line_end {
        out.push_str(&escape_html(content));
    }
    out
}

fn render_staged_section(artifacts: &SessionArtifacts, objects: &[EnvironmentObject]) -> String {
    let mut out = String::new();
    out.push_str("      <section class=\"section\" id=\"staged\">\n");
    out.push_str(&render_section_heading(
        "Staged grammar",
        "Expansion Chain",
        "A staged grammar extension turns selected source regions into structured frontend forms. This section shows that path from active grammar to forms, debug Loko text, generated objects, and executable artifacts.",
    ));
    if artifacts.grammar_parse.staged_regions.is_empty() {
        out.push_str("        <div class=\"empty-state\">No staged regions were activated in this source. The report still shows the normal source-to-tape pipeline.</div>\n");
    } else {
        out.push_str("        <div class=\"region-grid\">\n");
        for region in &artifacts.grammar_parse.staged_regions {
            out.push_str(&render_staged_region(artifacts, objects, region));
        }
        out.push_str("        </div>\n");
    }
    out.push_str("      </section>\n");
    out
}

fn render_staged_region(
    artifacts: &SessionArtifacts,
    objects: &[EnvironmentObject],
    region: &StagedParseRegion,
) -> String {
    let region_object_id = grammar_frontend::staged_parse_region_object_id(region);
    let region_rel = relation_for_region(region);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "          <article class=\"region-card\" data-node=\"{}\" data-rel=\"{}\">",
        escape_attr(&region_rel),
        escape_attr(&format!("{region_rel} grammar:{}", region.grammar_slug))
    );
    out.push_str("            <div class=\"region-card__head\">\n");
    let _ = writeln!(
        out,
        "              <h3>{}</h3><code>{}</code>",
        escape_html(&region.adapter),
        escape_html(&region_object_id)
    );
    let _ = writeln!(
        out,
        "              <p>driver {} · oracle {} · directive {}..{} · region {}..{} · activation {}</p>",
        escape_html(region.driver.as_str()),
        escape_html(region.oracle.as_str()),
        region.activation_directive.start,
        region.activation_directive.end,
        region.source_span.start,
        region.source_span.end,
        escape_html(&region.activation.to_string())
    );
    out.push_str("            </div>\n");
    out.push_str("            <div class=\"expansion-chain\">\n");
    out.push_str("              <div class=\"chain-step chain-step--adapter\">\n");
    out.push_str("                <span class=\"step-label\">Adapter</span>\n");
    let _ = writeln!(
        out,
        "                <strong>{}</strong><small>{} · {}</small>",
        escape_html(&region.adapter),
        escape_html(grammar_name_for_region(artifacts, region)),
        escape_html(&region.grammar.to_string())
    );
    out.push_str("              </div>\n");
    out.push_str(&render_region_grammar_flow(artifacts, objects, region));
    out.push_str("              <div class=\"chain-step chain-step--forms\">\n");
    out.push_str("                <span class=\"step-label\">Frontend forms</span>\n");
    for (index, form) in region.forms.iter().enumerate() {
        out.push_str(&render_frontend_form(artifacts, region, index, form));
    }
    out.push_str("              </div>\n");
    out.push_str("              <div class=\"chain-step chain-step--source\">\n");
    out.push_str("                <span class=\"step-label\">Materialized Loko</span>\n");
    let _ = writeln!(
        out,
        "                <pre><code>{}</code></pre>",
        escape_html(&region.materialized_source)
    );
    out.push_str("              </div>\n");
    out.push_str("              <div class=\"chain-step chain-step--objects\">\n");
    out.push_str("                <span class=\"step-label\">AST / semantic objects</span>\n");
    let ast_entries = generated_ast_entries_for_region(artifacts, region);
    let generated = generated_objects_for_region(artifacts, region, objects);
    if ast_entries.is_empty() && generated.is_empty() {
        out.push_str("                <div class=\"empty-state empty-state--small\">No generated semantic objects could be matched to this region.</div>\n");
    } else {
        out.push_str("                <ul class=\"object-links\">\n");
        for entry in ast_entries {
            let _ = writeln!(
                out,
                "                  <li data-node=\"{}\" data-rel=\"{}\"><code>{}</code><span>{}</span></li>",
                escape_attr(&entry.id),
                escape_attr(&entry.relation),
                escape_html(&entry.id),
                escape_html(&entry.title)
            );
        }
        for object in generated {
            let relations = relations_for_object_in_region(artifacts, region, object.id.as_str());
            let _ = writeln!(
                out,
                "                  <li data-node=\"object:{}\" data-rel=\"{}\"><code><a href=\"#{}\">{}</a></code><span>{}</span></li>",
                escape_attr(object.id.as_str()),
                escape_attr(&relations),
                escape_attr(&object_anchor(object.id.as_str())),
                escape_html(object.id.as_str()),
                escape_html(&object.presentation.title)
            );
        }
        out.push_str("                </ul>\n");
    }
    out.push_str("              </div>\n");
    out.push_str("              <div class=\"chain-step chain-step--artifacts\">\n");
    out.push_str("                <span class=\"step-label\">Executable artifacts</span>\n");
    out.push_str(&render_region_artifact_links(artifacts, region));
    out.push_str("              </div>\n");
    out.push_str("            </div>\n");
    out.push_str("          </article>\n");
    out
}

fn render_region_grammar_flow(
    artifacts: &SessionArtifacts,
    objects: &[EnvironmentObject],
    region: &StagedParseRegion,
) -> String {
    let region_rel = relation_for_region(region);
    let flow_relations = flow_relations_for_region(region);
    let package_id = format!("grammar-package:{}", region.grammar_slug);
    let resource_prefix = format!("grammar-resource:{}/", region.grammar_slug);
    let resources = objects
        .iter()
        .filter(|object| {
            object.kind == EnvironmentKind::GrammarResource
                && object.id.as_str().starts_with(&resource_prefix)
        })
        .collect::<Vec<_>>();
    let rule_names = artifacts
        .grammar_parse
        .extension_provenance
        .iter()
        .filter(|provenance| {
            provenance.activation == region.activation
                && provenance.object_slug == region.object_slug
        })
        .map(|provenance| provenance.rule_name.as_str())
        .collect::<Vec<_>>();

    let mut out = String::new();
    let _ = writeln!(
        out,
        "              <div class=\"chain-step chain-step--grammar\" data-rel=\"{} {}\">",
        escape_attr(&region_rel),
        escape_attr(&flow_relations)
    );
    out.push_str(
        "                <span class=\"step-label\">Grammar package, rules, and actions</span>\n",
    );
    out.push_str("                <ul class=\"object-links\">\n");
    if object_by_id(objects, &package_id).is_some() {
        let _ = writeln!(
            out,
            "                  <li data-node=\"object:{}\" data-rel=\"{}\"><code><a href=\"#{}\">{}</a></code><span>source-authored grammar package</span></li>",
            escape_attr(&package_id),
            escape_attr(&format!("{region_rel} {flow_relations}")),
            escape_attr(&object_anchor(&package_id)),
            escape_html(&package_id)
        );
    }
    for resource in resources {
        let _ = writeln!(
            out,
            "                  <li data-node=\"object:{}\" data-rel=\"{}\"><code><a href=\"#{}\">{}</a></code><span>checked-in grammar resource</span></li>",
            escape_attr(resource.id.as_str()),
            escape_attr(&format!("{region_rel} {flow_relations}")),
            escape_attr(&object_anchor(resource.id.as_str())),
            escape_html(resource.id.as_str())
        );
    }
    for rule_name in rule_names {
        let rule_id = format!("grammar-rule:{}/{}", region.grammar_slug, rule_name);
        let effective_rule_id = format!(
            "effective-grammar-rule:{}/{}",
            region.grammar_slug, rule_name
        );
        let action_id = format!("grammar-action:{}/{}", region.grammar_slug, rule_name);
        for (id, label) in [
            (rule_id, "direct rule"),
            (effective_rule_id, "effective rule"),
            (action_id, "structural action"),
        ] {
            if object_by_id(objects, &id).is_some() {
                let _ = writeln!(
                    out,
                    "                  <li data-node=\"object:{}\" data-rel=\"{}\"><code><a href=\"#{}\">{}</a></code><span>{}</span></li>",
                    escape_attr(&id),
                    escape_attr(&format!("{region_rel} {flow_relations}")),
                    escape_attr(&object_anchor(&id)),
                    escape_html(&id),
                    escape_html(label)
                );
            }
        }
    }
    out.push_str("                </ul>\n");
    out.push_str("              </div>\n");
    out
}

fn relations_for_object_in_region(
    artifacts: &SessionArtifacts,
    region: &StagedParseRegion,
    object_id: &str,
) -> String {
    let mut relations = vec![relation_for_region(region), format!("object:{object_id}")];
    for (index, form) in region.forms.iter().enumerate() {
        if object_ids_for_form(artifacts, form)
            .iter()
            .any(|id| id == object_id)
        {
            relations.push(relation_for_form(region, index));
            relations.push(flow_relation_for_form(region, index));
        }
    }
    relations.sort();
    relations.dedup();
    relations.join(" ")
}

struct GeneratedAstEntry {
    id: String,
    title: String,
    relation: String,
}

fn generated_ast_entries_for_region(
    artifacts: &SessionArtifacts,
    region: &StagedParseRegion,
) -> Vec<GeneratedAstEntry> {
    let mut entries = Vec::new();
    for (index, form) in region.forms.iter().enumerate() {
        let form_rel = relation_for_form(region, index);
        let flow_rel = flow_relation_for_form(region, index);
        let region_rel = relation_for_region(region);
        match &form.kind {
            FrontendFormKind::Function(function) => {
                for ast_function in &artifacts.ast.functions {
                    if ast_function.name == function.name {
                        let id = format!("ast-function:{}", ast_function.definition);
                        entries.push(GeneratedAstEntry {
                            relation: format!("{region_rel} {form_rel} {flow_rel} {id}"),
                            id,
                            title: format!("function {}", ast_function.name),
                        });
                    }
                }
            }
            FrontendFormKind::Class(class) => {
                for ast_class in &artifacts.ast.classes {
                    if ast_class.name == class.name {
                        let id = format!("ast-class:{}", ast_class.definition);
                        entries.push(GeneratedAstEntry {
                            relation: format!("{region_rel} {form_rel} {flow_rel} {id}"),
                            id,
                            title: format!("class {}", ast_class.name),
                        });
                    }
                }
            }
        }
    }
    entries.sort_by(|left, right| left.id.cmp(&right.id));
    entries.dedup_by(|left, right| left.id == right.id);
    entries
}

fn render_region_artifact_links(
    artifacts: &SessionArtifacts,
    region: &StagedParseRegion,
) -> String {
    let reports = artifacts.artifact_reports();
    let flow_relations = flow_relations_for_region(region);
    let stages = [
        (QueryArtifact::Ast, "AST"),
        (QueryArtifact::Bind, "Semantic IR"),
        (QueryArtifact::LoweredModule, "Lowered IR"),
        (QueryArtifact::TapeProgram, "Tape"),
        (QueryArtifact::RuntimeResult, "Runtime"),
    ];
    let mut out = String::new();
    out.push_str("                <ul class=\"stage-links\">\n");
    for (artifact, label) in stages {
        let key = report_for(&reports, artifact)
            .map(|report| report.key.to_string())
            .unwrap_or_else(|| "not emitted".to_string());
        let relation = format!(
            "{} {} {}",
            flow_relations,
            artifact_stage_relation(artifact),
            artifact_key_relation(&key)
        );
        let _ = writeln!(
            out,
            "                  <li data-node=\"artifact:{}\" data-rel=\"{}\"><span>{}</span><code><a href=\"#{}\">{}</a></code></li>",
            escape_attr(&key),
            escape_attr(&relation),
            escape_html(label),
            escape_attr(&artifact_anchor(&key)),
            escape_html(&key)
        );
    }
    out.push_str("                </ul>\n");
    out
}

fn render_frontend_form(
    artifacts: &SessionArtifacts,
    region: &StagedParseRegion,
    index: usize,
    form: &FrontendForm,
) -> String {
    let form_rel = relation_for_form(region, index);
    let flow_rel = flow_relation_for_form(region, index);
    let region_rel = relation_for_region(region);
    let object_relations = object_ids_for_form(artifacts, form)
        .into_iter()
        .map(|id| relation_for_generated_id(&id))
        .collect::<Vec<_>>()
        .join(" ");
    let mut out = String::new();
    let _ = writeln!(
        out,
        "                <div class=\"form-row\" data-node=\"{}\" data-rel=\"{} {} {}\">",
        escape_attr(&form_rel),
        escape_attr(&region_rel),
        escape_attr(&flow_rel),
        escape_attr(&object_relations)
    );
    match &form.kind {
        FrontendFormKind::Function(function) => {
            let _ = writeln!(
                out,
                "                  <div><strong>frontend-function</strong><code>{}</code></div>",
                escape_html(&function.name)
            );
            let _ = writeln!(
                out,
                "                  <p>body: {}</p>",
                escape_html(&frontend_expression_label(&function.body))
            );
        }
        FrontendFormKind::Class(class) => {
            let _ = writeln!(
                out,
                "                  <div><strong>frontend-class</strong><code>{} extends {}</code></div>",
                escape_html(&class.name),
                escape_html(&class.superclass)
            );
            out.push_str("                  <ul class=\"slot-list\">\n");
            for slot in &class.slots {
                let keyword = slot.init_keyword.as_deref().unwrap_or(slot.name.as_str());
                let required = if slot.required_init_keyword {
                    "required"
                } else {
                    "optional"
                };
                let constant = if slot.constant { "constant" } else { "mutable" };
                let _ = writeln!(
                    out,
                    "                    <li><code>{}</code><span title=\"Declared slot type\">type {}</span><span title=\"Construction keyword\">init {}:</span><span title=\"Initialization and write policy\">{} {}</span></li>",
                    escape_html(&slot.name),
                    escape_html(&slot.type_name),
                    escape_html(keyword),
                    escape_html(required),
                    escape_html(constant)
                );
            }
            out.push_str("                  </ul>\n");
        }
    }
    let _ = writeln!(
        out,
        "                  <small>source span {}..{}</small>",
        form.source_span.start, form.source_span.end
    );
    let emission_id = format!(
        "grammar-extension-emission:{}/{}/f{}",
        region.adapter, region.object_slug, index
    );
    let _ = writeln!(
        out,
        "                  <small>action output <a href=\"#{}\">{}</a></small>",
        escape_attr(&object_anchor(&emission_id)),
        escape_html(&emission_id)
    );
    out.push_str("                </div>\n");
    out
}

fn render_model_section(
    artifacts: &SessionArtifacts,
    objects: &[EnvironmentObject],
    related_objects: &BTreeMap<String, String>,
) -> String {
    let mut out = String::new();
    out.push_str("      <section class=\"section\" id=\"model\">\n");
    out.push_str(&render_section_heading(
        "MOP / EOP",
        "Generated And Inspectable Objects",
        "These cards are the environment's inspectable objects: classes, slots, accessors, generics, methods, functions, and grammar records. They are the object-model view of what the compiler built.",
    ));
    for (kind, title) in [
        (EnvironmentKind::Class, "Classes"),
        (EnvironmentKind::Slot, "Slots"),
        (EnvironmentKind::Accessor, "Accessors"),
        (EnvironmentKind::Generic, "Generics"),
        (EnvironmentKind::Method, "Methods"),
        (EnvironmentKind::Function, "Functions"),
        (EnvironmentKind::Constant, "Constants"),
        (EnvironmentKind::GrammarRegistry, "Grammar Registry"),
        (
            EnvironmentKind::RegisteredGrammarEnvironment,
            "Registered Grammar Environments",
        ),
        (EnvironmentKind::GrammarPackage, "Grammar Packages"),
        (EnvironmentKind::GrammarResource, "Grammar Resources"),
        (EnvironmentKind::GrammarAction, "Grammar Actions"),
        (
            EnvironmentKind::StagedGrammarAdapter,
            "Staged Grammar Adapters",
        ),
        (EnvironmentKind::GrammarExtension, "Grammar Extensions"),
        (
            EnvironmentKind::GrammarExtensionActivation,
            "Grammar Extension Activations",
        ),
        (
            EnvironmentKind::GrammarExtensionEmission,
            "Grammar Extension Emissions",
        ),
        (
            EnvironmentKind::GrammarExtensionProvenance,
            "Grammar Extension Provenance",
        ),
    ] {
        let group = objects
            .iter()
            .filter(|object| object.kind == kind)
            .collect::<Vec<_>>();
        if group.is_empty() {
            continue;
        }
        let _ = writeln!(out, "        <h3 class=\"group-heading\">{}</h3>", title);
        out.push_str("        <div class=\"object-grid\">\n");
        for object in group {
            out.push_str(&render_object_card(artifacts, object, related_objects));
        }
        out.push_str("        </div>\n");
    }
    out.push_str("      </section>\n");
    out
}

fn render_object_card(
    artifacts: &SessionArtifacts,
    object: &EnvironmentObject,
    related_objects: &BTreeMap<String, String>,
) -> String {
    let relation = related_objects
        .get(object.id.as_str())
        .map_or("", String::as_str);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "          <article class=\"object-card\" id=\"{}\" data-node=\"object:{}\" data-rel=\"{}\">",
        escape_attr(&object_anchor(object.id.as_str())),
        escape_attr(object.id.as_str()),
        escape_attr(relation)
    );
    let _ = writeln!(
        out,
        "            <div class=\"object-card__title\"><span>{}</span><code>{}</code></div>",
        escape_html(&object.presentation.title),
        escape_html(object.id.as_str())
    );
    out.push_str(&render_object_meta_chips(object));
    out.push_str(&render_doc_comment_block(
        "documentation",
        &object.documentation.lines,
    ));
    out.push_str(&render_object_links(artifacts, object));
    out.push_str(&render_object_facts(artifacts, object));
    let _ = writeln!(
        out,
        "            <details class=\"raw-inspection\"><summary>Raw inspection facts</summary><code>{}</code></details>",
        escape_html(&object.inspection.detail)
    );
    out.push_str("            <div class=\"affordance-row\">\n");
    for affordance in &object.affordances.names {
        let _ = writeln!(
            out,
            "              <span>{}</span>",
            escape_html(affordance.as_str())
        );
    }
    out.push_str("            </div>\n");
    out.push_str("          </article>\n");
    out
}

fn render_object_links(artifacts: &SessionArtifacts, object: &EnvironmentObject) -> String {
    if object.source.span.is_none() && object.artifacts.keys.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    out.push_str("            <div class=\"object-link-row\">\n");
    if let Some(span) = object.source.span {
        let line = source_line_for_byte(&artifacts.source.source, span.start);
        let _ = writeln!(
            out,
            "              <a href=\"#source-line-{}\" title=\"Source span {}..{}\">source line {}</a>",
            line, span.start, span.end, line
        );
    }
    for key in &object.artifacts.keys {
        let key = key.to_string();
        let _ = writeln!(
            out,
            "              <a href=\"#{}\" title=\"Compiler artifact key\"><code>{}</code></a>",
            escape_attr(&artifact_anchor(&key)),
            escape_html(&key)
        );
    }
    out.push_str("            </div>\n");
    out
}

fn render_object_meta_chips(object: &EnvironmentObject) -> String {
    let mut out = String::new();
    out.push_str("            <div class=\"meta-chip-row\">\n");
    meta_chip(
        &mut out,
        object.kind.as_str(),
        environment_kind_title(object.kind),
    );
    meta_chip(
        &mut out,
        object.origin.as_str(),
        environment_origin_title(object.origin),
    );
    meta_chip(
        &mut out,
        object.storage.as_str(),
        environment_storage_title(object.storage),
    );
    out.push_str("            </div>\n");
    out
}

fn meta_chip(out: &mut String, label: &str, title: &str) {
    let _ = writeln!(
        out,
        "              <span class=\"meta-chip\" title=\"{}\">{}</span>",
        escape_attr(title),
        escape_html(label)
    );
}

fn render_object_facts(artifacts: &SessionArtifacts, object: &EnvironmentObject) -> String {
    let facts = object_facts(artifacts, object);
    if facts.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    out.push_str("            <dl class=\"object-facts\">\n");
    for fact in facts {
        let _ = writeln!(
            out,
            "              <div title=\"{}\"><dt>{}</dt><dd>{}</dd></div>",
            escape_attr(fact.title),
            escape_html(fact.label),
            escape_html(&fact.value)
        );
    }
    out.push_str("            </dl>\n");
    out
}

struct ObjectFact {
    label: &'static str,
    value: String,
    title: &'static str,
}

impl ObjectFact {
    fn new(label: &'static str, value: impl Into<String>, title: &'static str) -> Self {
        Self {
            label,
            value: value.into(),
            title,
        }
    }
}

fn object_facts(artifacts: &SessionArtifacts, object: &EnvironmentObject) -> Vec<ObjectFact> {
    match object.kind {
        EnvironmentKind::Class => class_object_facts(artifacts, object.id.as_str()),
        EnvironmentKind::Slot => slot_object_facts(artifacts, object.id.as_str()),
        EnvironmentKind::Accessor => accessor_object_facts(artifacts, object.id.as_str()),
        EnvironmentKind::Generic => generic_object_facts(artifacts, object.id.as_str()),
        EnvironmentKind::Method => method_object_facts(artifacts, object.id.as_str()),
        EnvironmentKind::Function => function_object_facts(artifacts, object.id.as_str()),
        EnvironmentKind::Constant => constant_object_facts(artifacts, object.id.as_str()),
        _ => Vec::new(),
    }
}

fn class_object_facts(artifacts: &SessionArtifacts, object_id: &str) -> Vec<ObjectFact> {
    let Some(index) = parse_prefixed_index(object_id, "class:c") else {
        return Vec::new();
    };
    let Some(class) = artifacts.sem.classes.get(index) else {
        return Vec::new();
    };
    let superclass = class
        .superclass
        .and_then(|id| class_name(&artifacts.sem, id))
        .unwrap_or("<none>");
    let init_keywords = class
        .initialization_plan
        .steps
        .iter()
        .map(|step| format!("{}:", step.init_keyword))
        .collect::<Vec<_>>();
    Vec::from([
        ObjectFact::new(
            "superclass",
            superclass,
            "Direct superclass for this class.",
        ),
        ObjectFact::new(
            "slots",
            format!(
                "{} direct / {} effective",
                class.direct_slots.len(),
                class.effective_slots.len()
            ),
            "Direct slots are declared here; effective slots include inherited layout slots.",
        ),
        ObjectFact::new(
            "init keys",
            if init_keywords.is_empty() {
                "none".into()
            } else {
                init_keywords.join(", ")
            },
            "Construction keywords accepted by this class initialization plan.",
        ),
        ObjectFact::new(
            "runtime",
            class_origin_label(class.origin),
            "Runtime representation selected for this class.",
        ),
    ])
}

fn slot_object_facts(artifacts: &SessionArtifacts, object_id: &str) -> Vec<ObjectFact> {
    let Some((class_index, slot_index)) = parse_class_slot_object_id(object_id, "slot:") else {
        return Vec::new();
    };
    let Some(class) = artifacts.sem.classes.get(class_index) else {
        return Vec::new();
    };
    let Some(slot) = class.effective_slots.get(slot_index) else {
        return Vec::new();
    };
    Vec::from([
        ObjectFact::new("type", slot.type_name.clone(), "Declared slot value type."),
        ObjectFact::new(
            "init key",
            format!("{}:", slot.init_keyword),
            "Construction keyword that initializes this slot.",
        ),
        ObjectFact::new(
            "mutability",
            slot_mutability_label(slot.mutability),
            "Whether the slot has a synthesized setter generic.",
        ),
        ObjectFact::new(
            "source",
            if slot.source_class == class.id {
                "direct"
            } else {
                "inherited"
            },
            "Whether this effective slot was declared on this class or inherited.",
        ),
        ObjectFact::new(
            "storage",
            slot_origin_label(slot.origin),
            "Storage bridge used for this slot.",
        ),
    ])
}

fn accessor_object_facts(artifacts: &SessionArtifacts, object_id: &str) -> Vec<ObjectFact> {
    let Some((class_index, slot_index)) = parse_class_slot_object_id(object_id, "accessor:") else {
        return Vec::new();
    };
    let Some(class) = artifacts.sem.classes.get(class_index) else {
        return Vec::new();
    };
    let Some(slot) = class.effective_slots.get(slot_index) else {
        return Vec::new();
    };
    Vec::from([
        ObjectFact::new(
            "reads",
            format!("{}.{}", class.name, slot.name),
            "Slot read by this accessor.",
        ),
        ObjectFact::new(
            "result",
            value_kind_label(&artifacts.sem, slot.accessor.result_kind),
            "Value category returned by the accessor.",
        ),
    ])
}

fn generic_object_facts(artifacts: &SessionArtifacts, object_id: &str) -> Vec<ObjectFact> {
    let Some(index) = parse_prefixed_index(object_id, "generic:g") else {
        return Vec::new();
    };
    let Some(generic) = artifacts.sem.generics.get(index) else {
        return Vec::new();
    };
    let method_count = artifacts
        .sem
        .methods
        .iter()
        .filter(|method| method.generic == generic.id)
        .count();
    Vec::from([
        ObjectFact::new(
            "params",
            generic.params.join(", "),
            "Generic function parameters in call order.",
        ),
        ObjectFact::new(
            "dispatch",
            generic
                .dispatch_params
                .iter()
                .map(|index| format!("#{index}"))
                .collect::<Vec<_>>()
                .join(", "),
            "Parameter indexes that participate in method dispatch.",
        ),
        ObjectFact::new(
            "methods",
            method_count.to_string(),
            "Methods currently attached to this generic.",
        ),
        ObjectFact::new(
            "result",
            value_kind_list_label(&artifacts.sem, &generic.result_kinds),
            "Resolved generic result category.",
        ),
    ])
}

fn method_object_facts(artifacts: &SessionArtifacts, object_id: &str) -> Vec<ObjectFact> {
    let Some(index) = parse_prefixed_index(object_id, "method:m") else {
        return Vec::new();
    };
    let Some(method) = artifacts.sem.methods.get(index) else {
        return Vec::new();
    };
    let generic = artifacts
        .sem
        .generics
        .get(method.generic.0 as usize)
        .map_or("<unknown>", |generic| generic.name.as_str());
    let specializers = method
        .specializers
        .iter()
        .map(|specializer| {
            specializer
                .and_then(|id| class_name(&artifacts.sem, id))
                .unwrap_or("<object>")
                .to_string()
        })
        .collect::<Vec<_>>()
        .join(", ");
    Vec::from([
        ObjectFact::new(
            "generic",
            generic,
            "Generic function implemented by this method.",
        ),
        ObjectFact::new(
            "specializers",
            specializers,
            "Nominal classes this method specializes on.",
        ),
        ObjectFact::new(
            "origin",
            method_origin_label(method.origin),
            "Whether this method came from source or compiler synthesis.",
        ),
        ObjectFact::new(
            "result",
            value_kind_list_label(&artifacts.sem, &method.result_kinds),
            "Resolved method result category.",
        ),
    ])
}

fn function_object_facts(artifacts: &SessionArtifacts, object_id: &str) -> Vec<ObjectFact> {
    let Some(index) = parse_prefixed_index(object_id, "function:f") else {
        return Vec::new();
    };
    let Some(function) = artifacts.sem.functions.get(index) else {
        return Vec::new();
    };
    Vec::from([
        ObjectFact::new(
            "params",
            function.params.len().to_string(),
            "Number of direct function parameters.",
        ),
        ObjectFact::new(
            "locals",
            function.locals.len().to_string(),
            "Total locals, including parameters and let bindings.",
        ),
        ObjectFact::new(
            "result",
            value_kind_list_label(&artifacts.sem, &function.result_kinds),
            "Resolved function result category.",
        ),
    ])
}

fn constant_object_facts(artifacts: &SessionArtifacts, object_id: &str) -> Vec<ObjectFact> {
    let Some(index) = parse_prefixed_index(object_id, "constant:k") else {
        return Vec::new();
    };
    let Some(constant) = artifacts.sem.constants.get(index) else {
        return Vec::new();
    };
    Vec::from([
        ObjectFact::new(
            "binding",
            "immutable",
            "Top-level constants are immutable source bindings.",
        ),
        ObjectFact::new(
            "result",
            value_kind_label(&artifacts.sem, constant.result_kind),
            "Resolved constant value category.",
        ),
    ])
}

fn environment_kind_title(kind: EnvironmentKind) -> &'static str {
    match kind {
        EnvironmentKind::SourceFile => "Named source file known to this session.",
        EnvironmentKind::GrammarRegistry => {
            "Host catalog of available language environments and staged grammar extensions."
        }
        EnvironmentKind::RegisteredGrammarEnvironment => {
            "Language environment template available through the host grammar catalog."
        }
        EnvironmentKind::StagedGrammarAdapter => {
            "Trusted host adapter that implements one staged grammar extension."
        }
        EnvironmentKind::GrammarExtension => {
            "Structured descriptor for a registered staged grammar extension."
        }
        EnvironmentKind::GrammarExtensionActivation => {
            "Concrete activation of a staged grammar extension over a source region."
        }
        EnvironmentKind::GrammarExtensionEmission => {
            "Frontend form emitted by a staged grammar extension."
        }
        EnvironmentKind::GrammarExtensionProvenance => {
            "Grammar rule provenance charged to a staged grammar extension."
        }
        EnvironmentKind::LanguageEnvironment => {
            "Active language environment selected for parsing this source."
        }
        EnvironmentKind::Grammar => {
            "Grammar metaobject in the active environment's resolved grammar stack."
        }
        EnvironmentKind::GrammarPackage => {
            "Source-authored grammar package bundled into the frontend."
        }
        EnvironmentKind::GrammarResource => {
            "Source-authored grammar resource file loaded for a package."
        }
        EnvironmentKind::GrammarRule => "Direct grammar rule projected from a grammar.",
        EnvironmentKind::GrammarAction => {
            "Deterministic structural action attached to a grammar rule."
        }
        EnvironmentKind::EffectiveGrammarRule => {
            "Grammar rule after stack resolution and inheritance."
        }
        EnvironmentKind::GrammarActivation => "Source region parsed by a selected grammar.",
        EnvironmentKind::StagedRegion => "Materialized source region emitted by a grammar adapter.",
        EnvironmentKind::Class => "Class metaobject in the semantic object model.",
        EnvironmentKind::Slot => "Effective slot descriptor on a finalized class.",
        EnvironmentKind::Accessor => "Getter function synthesized for a slot read.",
        EnvironmentKind::Generic => "Generic function participating in method dispatch.",
        EnvironmentKind::Method => "Method implementation attached to a generic function.",
        EnvironmentKind::Function => "Direct function definition.",
        EnvironmentKind::Constant => "Immutable top-level constant definition.",
        EnvironmentKind::DispatchSite => "Resolved generic dispatch site.",
        EnvironmentKind::Diagnostic => "Structured compiler diagnostic.",
        EnvironmentKind::RuntimeLayoutRow => "Runtime storage layout fact.",
        EnvironmentKind::Artifact => "Incremental compiler artifact.",
        EnvironmentKind::HostFunction => {
            "Scalar capability declared by the embedding host registry."
        }
        EnvironmentKind::HostObject => "Object supplied by a host projection adapter.",
        EnvironmentKind::Mutation => "Entry in the session's in-memory mutation log.",
        EnvironmentKind::TypingConfig => "Soft typing policy applied to this session's compiles.",
        EnvironmentKind::TypingDecision => {
            "Advisory classification of one runtime check by the soft typing pass."
        }
    }
}

fn environment_origin_title(origin: EnvironmentOrigin) -> &'static str {
    match origin {
        EnvironmentOrigin::SourceAuthored => "Created from Loko source.",
        EnvironmentOrigin::HostBacked => "Declared by explicit host-backed metadata.",
        EnvironmentOrigin::CompilerGenerated => "Generated by compiler or session work.",
        EnvironmentOrigin::HostProvided => "Projected by an explicit host adapter.",
        EnvironmentOrigin::Session => "Introduced by an in-memory session mutation.",
    }
}

fn environment_storage_title(storage: EnvironmentStorage) -> &'static str {
    match storage {
        EnvironmentStorage::SourceDefined => "Represented by source-level definitions.",
        EnvironmentStorage::HostRegistry => "Represented by host registry metadata.",
        EnvironmentStorage::HostAdapter => "Represented through an adapter-owned host bridge.",
        EnvironmentStorage::FrontendRegistry => {
            "Represented by the session grammar frontend registry."
        }
        EnvironmentStorage::CachedArtifact => "Represented by cached compiler/session artifacts.",
        EnvironmentStorage::HostOwned => "Represented by host-owned state outside Interloko.",
        EnvironmentStorage::TransientInMemory => {
            "Lives only in the live session log; not persisted or serialized."
        }
    }
}

fn parse_prefixed_index(object_id: &str, prefix: &str) -> Option<usize> {
    object_id.strip_prefix(prefix)?.parse().ok()
}

fn parse_class_slot_object_id(object_id: &str, prefix: &str) -> Option<(usize, usize)> {
    let rest = object_id.strip_prefix(prefix)?;
    let (class_text, slot_text) = rest.split_once(".s")?;
    let class = class_text.strip_prefix('c')?.parse().ok()?;
    let slot = slot_text.parse().ok()?;
    Some((class, slot))
}

fn class_name(module: &sem::Module, id: sem::ClassId) -> Option<&str> {
    module
        .classes
        .get(id.0 as usize)
        .map(|class| class.name.as_str())
}

fn class_origin_label(origin: ClassOrigin) -> String {
    match origin {
        ClassOrigin::ObjectRoot => "object root".into(),
        ClassOrigin::Native => "native object".into(),
        ClassOrigin::BuiltinValue { kind } => format!("built-in {}", kind.name()),
        ClassOrigin::HostBacked { host_class } => format!("host-backed class h{}", host_class.0),
    }
}

fn slot_mutability_label(mutability: SlotMutability) -> &'static str {
    match mutability {
        SlotMutability::Mutable => "writable",
        SlotMutability::Constant => "constant",
    }
}

fn slot_origin_label(origin: SlotOrigin) -> String {
    match origin {
        SlotOrigin::Native => "native slot storage".into(),
        SlotOrigin::HostBacked {
            host_class,
            host_slot,
        } => format!("host slot h{}.s{}", host_class.0, host_slot.0),
    }
}

fn method_origin_label(origin: MethodOrigin) -> String {
    match origin {
        MethodOrigin::Source => "source method".into(),
        MethodOrigin::SlotSetter { class, slot } => {
            format!("synthesized setter for c{}.s{}", class.0, slot.0)
        }
        MethodOrigin::Session { sequence } => {
            format!("session-inserted method (mutation #{sequence})")
        }
        MethodOrigin::Numeric { operator } => {
            format!("built-in numeric method for `{}`", operator.name())
        }
    }
}

fn value_kind_label(module: &sem::Module, kind: ValueKind) -> String {
    match kind {
        ValueKind::Any => "<object>".into(),
        ValueKind::I64 => "<integer>".into(),
        ValueKind::F64 => "<float>".into(),
        ValueKind::Bool => "<boolean>".into(),
        ValueKind::String(id) | ValueKind::Symbol(id) | ValueKind::Vector(id) => {
            class_name(module, id)
                .unwrap_or("<unknown value class>")
                .into()
        }
        ValueKind::Object(id) => class_name(module, id).unwrap_or("<unknown class>").into(),
        ValueKind::ClassObject(id) => {
            format!(
                "class {}",
                class_name(module, id).unwrap_or("<unknown class>")
            )
        }
        ValueKind::GenericFunctionObject(id) => format!("generic:g{}", id.0),
        ValueKind::MethodObject(id) => format!("method:m{}", id.0),
        ValueKind::SlotDescriptorObject { class, slot } => {
            format!("slot:c{}.s{}", class.0, slot.0)
        }
        ValueKind::Callable => "<callable>".into(),
        ValueKind::KnownCallable(callable) => format!("known callable {callable:?}"),
        ValueKind::Error => "error".into(),
    }
}

fn value_kind_list_label(module: &sem::Module, kinds: &[ValueKind]) -> String {
    match kinds {
        [] => "()".into(),
        [kind] => value_kind_label(module, *kind),
        values => {
            let values = values
                .iter()
                .map(|kind| value_kind_label(module, *kind))
                .collect::<Vec<_>>()
                .join(", ");
            format!("({values})")
        }
    }
}

fn render_diagnostics_section(artifacts: &SessionArtifacts) -> String {
    let mut out = String::new();
    out.push_str("      <section class=\"section\" id=\"diagnostics\">\n");
    out.push_str(&render_section_heading(
        "Diagnostics",
        "Structured Diagnostic Facts",
        "Diagnostics are stored as stable codes, spans, labels, and arguments. Presentation tools can render those facts into human text without changing the compiler artifact itself.",
    ));
    if artifacts.diagnostics.is_empty() {
        out.push_str("        <div class=\"empty-state\">No diagnostics were emitted.</div>\n");
    } else {
        out.push_str("        <div class=\"diagnostic-list\">\n");
        for (index, diagnostic) in artifacts.diagnostics.iter().enumerate() {
            out.push_str(&render_diagnostic(index, diagnostic));
        }
        out.push_str("        </div>\n");
    }
    out.push_str("      </section>\n");
    out
}

fn render_diagnostic(index: usize, diagnostic: &Diagnostic) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "          <article class=\"diagnostic-card\" data-node=\"diagnostic:{}\" data-rel=\"diagnostic:{}\">",
        index, index
    );
    let _ = writeln!(
        out,
        "            <div><strong>{:?}</strong><code>{}</code><span>{}..{}</span></div>",
        diagnostic.severity,
        escape_html(&diagnostic.code.to_string()),
        diagnostic.span.start,
        diagnostic.span.end
    );
    if !diagnostic.args.is_empty() {
        out.push_str("            <dl class=\"arg-list\">\n");
        for arg in &diagnostic.args {
            let _ = writeln!(
                out,
                "              <div><dt>{}</dt><dd>{}</dd></div>",
                escape_html(&arg.name),
                escape_html(&arg.value.render())
            );
        }
        out.push_str("            </dl>\n");
    }
    if !diagnostic.labels.is_empty() {
        out.push_str("            <ul class=\"label-list\">\n");
        for label in &diagnostic.labels {
            let style = match label.style {
                DiagnosticLabelStyle::Primary => "primary",
                DiagnosticLabelStyle::Secondary => "secondary",
            };
            let message = label.message.as_deref().unwrap_or("");
            let _ = writeln!(
                out,
                "              <li>{} {}..{} {}</li>",
                style,
                label.span.start,
                label.span.end,
                escape_html(message)
            );
        }
        out.push_str("            </ul>\n");
    }
    out.push_str("          </article>\n");
    out
}

fn render_artifacts_section(artifacts: &SessionArtifacts) -> String {
    let mut out = String::new();
    out.push_str("      <section class=\"section\" id=\"artifacts\">\n");
    out.push_str(&render_section_heading(
        "Incremental engine",
        "Artifact Keys And Dependency Graph",
        "Every compiler stage produces a named artifact with a stable key, work count, and observed duration. The graph shows which artifacts depend on which earlier results.",
    ));
    out.push_str("        <div class=\"pipeline-panel\" id=\"artifact-pipeline\">\n");
    out.push_str("          <div class=\"pipeline-panel__copy\">\n");
    out.push_str("            <h3>Compiler Artifact Path</h3>\n");
    out.push_str("            <p>Stable query artifacts produced for this file. Keys identify artifacts; timing and work show the observed cost of this run.</p>\n");
    out.push_str("          </div>\n");
    out.push_str(&render_pipeline(artifacts));
    out.push_str("        </div>\n");
    out.push_str("        <div class=\"artifact-grid\">\n");
    for report in artifacts.artifact_metrics() {
        let label = artifact_label(report.artifact);
        let relation = artifact_relation_tokens(artifacts, report.artifact);
        let _ = writeln!(
            out,
            "          <article class=\"artifact-card\" id=\"{}\" data-node=\"artifact:{}\" data-rel=\"{}\"><span>{}</span><code>{}</code><dl><div><dt>time</dt><dd>{}</dd></div><div><dt>work</dt><dd>{}</dd></div></dl></article>",
            escape_attr(&artifact_anchor(&report.key.to_string())),
            escape_attr(&report.key.to_string()),
            escape_attr(&relation),
            escape_html(&label),
            escape_html(&report.key.to_string()),
            escape_html(&format_duration_nanos(report.metrics.elapsed_nanos)),
            report.metrics.work_units
        );
    }
    out.push_str("        </div>\n");
    out.push_str(&render_collapsible_dump(
        "Artifact Graph",
        "artifact-graph",
        &artifacts.dump_artifact_graph(),
        false,
    ));
    out.push_str("      </section>\n");
    out
}

fn render_trace_section(artifacts: &SessionArtifacts) -> String {
    let mut out = String::new();
    out.push_str("      <section class=\"section\" id=\"trace\">\n");
    out.push_str(&render_section_heading(
        "Runtime trace",
        "Execution Timeline",
        "This is the recorded run of main through execution_tape. It shows every runtime host call with its Interloko call stack and the final outcome, so generated behavior can be inspected after compilation.",
    ));
    let Some(trace) = &artifacts.runtime_trace else {
        out.push_str("        <div class=\"empty-state\">Runtime tracing was not requested for this report.</div>\n");
        out.push_str("      </section>\n");
        return out;
    };
    let view = artifacts.execution_trace_view(trace);
    let rows = trace_rows(&view);
    out.push_str("        <div class=\"trace-summary\">\n");
    trace_metric(&mut out, "activations", view.activation_count());
    trace_metric(&mut out, "host calls", view.host_calls().len());
    trace_metric(&mut out, "instructions", view.instructions().len());
    trace_metric(&mut out, "events", rows.len());
    out.push_str("        </div>\n");
    out.push_str("        <ol class=\"trace-timeline\">\n");
    for (index, row) in rows.iter().take(80).enumerate() {
        let _ = writeln!(
            out,
            "          <li class=\"trace-event {}\"><span>{index:04}</span><strong>{}</strong><small>{}</small></li>",
            row.class,
            escape_html(&row.summary),
            escape_html(&row.detail)
        );
    }
    if rows.len() > 80 {
        let _ = writeln!(
            out,
            "          <li class=\"trace-event trace-event--more\"><span>...</span><strong>{} more events</strong><small>Open the raw trace for the complete record.</small></li>",
            rows.len() - 80
        );
    }
    out.push_str("        </ol>\n");
    out.push_str(&render_collapsible_dump(
        "Raw Execution Trace",
        "execution-trace",
        &artifacts.dump_execution_trace(),
        false,
    ));
    out.push_str("      </section>\n");
    out
}

fn trace_metric(out: &mut String, label: &str, value: usize) {
    let _ = writeln!(
        out,
        "          <div><span>{}</span><strong>{}</strong></div>",
        escape_html(label),
        value
    );
}

/// One report timeline row derived from a recorded trace.
struct TraceRow {
    class: &'static str,
    summary: String,
    detail: String,
}

fn trace_rows(view: &ExecutionTraceView<'_>) -> Vec<TraceRow> {
    let mut rows = Vec::new();
    if let Some(entry) = view.entry() {
        rows.push(TraceRow {
            class: "trace-event--run",
            summary: format!("run starts at {entry}"),
            detail: format!("args={}", view.arguments().len()),
        });
    }
    for call in view.host_calls() {
        let status = match call.outcome {
            HostOutcome::Returned { .. } => "returned".to_string(),
            HostOutcome::Failed(error) => format!("failed: {error}"),
            HostOutcome::Threw { tag, .. } => format!("threw tag#{}", tag.0),
        };
        rows.push(TraceRow {
            class: "trace-event--host",
            summary: format!("host {} · {status}", call.symbol.unwrap_or("<unnamed>")),
            detail: trace_stack_detail(call.location, &call.stack),
        });
    }
    for step in view.instructions() {
        rows.push(TraceRow {
            class: "trace-event--instr",
            summary: format!("instruction {}", step.mnemonic()),
            detail: match step.next {
                Some(next) => format!("{} -> {next}", step.location),
                None => step.location.to_string(),
            },
        });
    }
    if let Some(outcome) = view.outcome() {
        let summary = match outcome.result {
            Outcome::Returned(_) => "run returns".to_string(),
            Outcome::Trapped(trap) => format!("run traps: {}", trap.trap),
        };
        rows.push(TraceRow {
            class: "trace-event--run",
            summary,
            detail: outcome.location.map_or_else(String::new, |location| {
                trace_stack_detail(location, &outcome.stack)
            }),
        });
    }
    rows
}

fn trace_stack_detail(location: TracedLocation<'_>, stack: &TracedStack<'_>) -> String {
    let mut detail = format!("at {location}");
    if let Some(depth) = stack.depth() {
        let _ = write!(detail, " · depth={depth}");
    }
    if let Some(caller) = stack.locations().nth(1) {
        let _ = write!(detail, " · from {caller}");
    }
    detail
}

fn format_duration_nanos(nanos: u64) -> String {
    if nanos == 0 {
        "0 ns".into()
    } else if nanos < 1_000 {
        format!("{nanos} ns")
    } else if nanos < 1_000_000 {
        format!("{:.1} us", nanos as f64 / 1_000.0)
    } else if nanos < 1_000_000_000 {
        format!("{:.1} ms", nanos as f64 / 1_000_000.0)
    } else {
        format!("{:.2} s", nanos as f64 / 1_000_000_000.0)
    }
}

fn render_ir_section(artifacts: &SessionArtifacts) -> String {
    let mut out = String::new();
    out.push_str("      <section class=\"section\" id=\"ir\">\n");
    out.push_str(&render_section_heading(
        "Executable path",
        "Lowered IR And Tape",
        "Lowered IR is Interloko's executable plan. Tape disassembly is the concrete execution_tape program emitted from that plan and run by the runtime.",
    ));
    out.push_str(&render_collapsible_dump_with_rel(
        "Lowered IR",
        "lowered-ir",
        &artifacts.dump_stage(DumpStage::Lowered),
        true,
        &artifact_relation_tokens(artifacts, QueryArtifact::LoweredModule),
    ));
    out.push_str(&render_collapsible_dump_with_rel(
        "Tape Disassembly",
        "tape-disassembly",
        &artifacts.dump_stage(DumpStage::Tape),
        false,
        &artifact_relation_tokens(artifacts, QueryArtifact::TapeProgram),
    ));
    out.push_str("      </section>\n");
    out
}

fn render_collapsible_dump(title: &str, id: &str, dump: &str, open: bool) -> String {
    render_collapsible_dump_with_rel(title, id, dump, open, "")
}

fn render_collapsible_dump_with_rel(
    title: &str,
    id: &str,
    dump: &str,
    open: bool,
    relation: &str,
) -> String {
    let mut out = String::new();
    let open_attr = if open { " open" } else { "" };
    let _ = writeln!(
        out,
        "        <details class=\"dump\" id=\"{}\" data-rel=\"{}\"{}>",
        escape_attr(id),
        escape_attr(relation),
        open_attr
    );
    let _ = writeln!(out, "          <summary>{}</summary>", escape_html(title));
    let _ = writeln!(
        out,
        "          <pre><code>{}</code></pre>",
        escape_html(dump)
    );
    out.push_str("        </details>\n");
    out
}

fn render_report_json(artifacts: &SessionArtifacts, objects: &[EnvironmentObject]) -> String {
    let mut out = String::new();
    out.push('{');
    let _ = write!(
        out,
        "\"source\":{{\"id\":{},\"name\":{},\"bytes\":{},\"lines\":{}}}",
        json_string(&artifacts.source.id.to_string()),
        json_string(&artifacts.source.name),
        artifacts.source.source.len(),
        artifacts.source.source.lines().count()
    );
    let _ = write!(
        out,
        ",\"runtime\":{{\"summary\":{},\"class\":{}}}",
        json_string(runtime_summary(artifacts.runtime_result.as_ref()).as_str()),
        json_string(runtime_summary(artifacts.runtime_result.as_ref()).class)
    );
    out.push_str(",\"grammarStack\":[");
    for (index, grammar_id) in artifacts
        .grammar_parse
        .grammar_stack
        .grammars
        .iter()
        .enumerate()
    {
        if index > 0 {
            out.push(',');
        }
        let name = artifacts
            .grammar_parse
            .grammars
            .iter()
            .find(|grammar| grammar.id == *grammar_id)
            .map_or("<unknown>", |grammar| grammar.name.as_str());
        let _ = write!(
            out,
            "{{\"id\":{},\"name\":{}}}",
            json_string(&grammar_id.to_string()),
            json_string(name)
        );
    }
    out.push(']');
    out.push_str(",\"diagnostics\":[");
    for (index, diagnostic) in artifacts.diagnostics.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&diagnostic_json(diagnostic));
    }
    out.push(']');
    out.push_str(",\"stagedRegions\":[");
    for (index, region) in artifacts.grammar_parse.staged_regions.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&region_json(region));
    }
    out.push(']');
    out.push_str(",\"parseViews\":[");
    for (index, form) in artifacts.source_forms().iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&parse_view_json(artifacts, form));
    }
    out.push(']');
    out.push_str(",\"objects\":[");
    for (index, object) in objects.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let _ = write!(
            out,
            "{{\"id\":{},\"kind\":{},\"title\":{},\"sourceSpan\":{},\"documentation\":{},\"artifactKeys\":[",
            json_string(object.id.as_str()),
            json_string(object.kind.as_str()),
            json_string(&object.presentation.title),
            object
                .source
                .span
                .map_or_else(|| "null".to_string(), span_json),
            json_string_array(&object.documentation.lines)
        );
        for (key_index, key) in object.artifacts.keys.iter().enumerate() {
            if key_index > 0 {
                out.push(',');
            }
            out.push_str(&json_string(&key.to_string()));
        }
        out.push_str("],\"affordances\":[");
        for (affordance_index, affordance) in object.affordances.names.iter().enumerate() {
            if affordance_index > 0 {
                out.push(',');
            }
            out.push_str(&json_string(affordance.as_str()));
        }
        out.push_str("]}");
    }
    out.push(']');
    out.push_str(",\"artifacts\":[");
    for (index, report) in artifacts.artifact_metrics().iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let _ = write!(
            out,
            "{{\"key\":{},\"label\":{},\"work\":{},\"elapsedNanos\":{}}}",
            json_string(&report.key.to_string()),
            json_string(&artifact_label(report.artifact)),
            report.metrics.work_units,
            report.metrics.elapsed_nanos
        );
    }
    out.push(']');
    out.push_str(",\"executionTrace\":[");
    if let Some(trace) = &artifacts.runtime_trace {
        let rows = trace_rows(&artifacts.execution_trace_view(trace));
        for (index, row) in rows.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            let _ = write!(
                out,
                "{{\"index\":{},\"kind\":{},\"summary\":{},\"detail\":{}}}",
                index,
                json_string(row.class),
                json_string(&row.summary),
                json_string(&row.detail)
            );
        }
    }
    out.push(']');
    out.push('}');
    out
}

fn diagnostic_json(diagnostic: &Diagnostic) -> String {
    let mut out = String::new();
    let _ = write!(
        out,
        "{{\"severity\":{},\"code\":{},\"span\":{},\"args\":[",
        json_string(&format!("{:?}", diagnostic.severity)),
        json_string(&diagnostic.code.to_string()),
        span_json(diagnostic.span)
    );
    for (index, arg) in diagnostic.args.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let _ = write!(
            out,
            "{{\"name\":{},\"value\":{}}}",
            json_string(&arg.name),
            json_string(&arg.value.render())
        );
    }
    out.push_str("],\"labels\":[");
    for (index, label) in diagnostic.labels.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let _ = write!(
            out,
            "{{\"style\":{},\"span\":{},\"message\":{}}}",
            json_string(match label.style {
                DiagnosticLabelStyle::Primary => "primary",
                DiagnosticLabelStyle::Secondary => "secondary",
            }),
            span_json(label.span),
            json_string(label.message.as_deref().unwrap_or(""))
        );
    }
    out.push_str("]}");
    out
}

fn parse_view_json(artifacts: &SessionArtifacts, form: &SourceFormProvenance) -> String {
    let view = artifacts.parse_view_for_span(form.span);
    let mut out = String::new();
    let _ = write!(
        out,
        "{{\"id\":{},\"kind\":{},\"span\":{},\"docComments\":{},\"rules\":[",
        json_string(&form.id),
        json_string(form.kind.as_str()),
        span_json(form.span),
        json_string_array(&form.doc_comments)
    );
    for (index, application) in form.rule_applications.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let object_id = effective_rule_object_id_for_application(artifacts, application);
        let _ = write!(
            out,
            "{{\"name\":{},\"span\":{},\"object\":{}}}",
            json_string(&application.rule_name),
            span_json(application.span),
            object_id.map_or_else(|| "null".to_string(), |id| json_string(&id))
        );
    }
    out.push_str("],\"tokens\":[");
    for (index, token) in view.tokens.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let _ = write!(
            out,
            "{{\"kind\":{},\"text\":{},\"span\":{}}}",
            json_string(&format!("{:?}", token.kind)),
            json_string(&token.text),
            span_json(token.span)
        );
    }
    out.push_str("],\"diagnostics\":[");
    for (index, diagnostic) in view.diagnostics.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let _ = write!(
            out,
            "{{\"code\":{},\"span\":{},\"unexpected\":{},\"expected\":{}}}",
            json_string(&diagnostic.code.to_string()),
            span_json(diagnostic.span),
            diagnostic
                .unexpected_fragment
                .as_ref()
                .map_or_else(|| "null".to_string(), |value| json_string(value)),
            diagnostic
                .expected_fragment
                .as_ref()
                .map_or_else(|| "null".to_string(), |value| json_string(value))
        );
    }
    out.push_str("]}");
    out
}

fn region_json(region: &StagedParseRegion) -> String {
    let mut out = String::new();
    let _ = write!(
        out,
        "{{\"id\":{},\"adapter\":{},\"grammar\":{},\"driver\":{},\"oracle\":{},\"activation\":{},\"directive\":{},\"span\":{},\"forms\":[",
        json_string(&grammar_frontend::staged_parse_region_object_id(region).to_string()),
        json_string(&region.adapter),
        json_string(&region.grammar.to_string()),
        json_string(region.driver.as_str()),
        json_string(region.oracle.as_str()),
        json_string(&region.activation.to_string()),
        span_json(region.activation_directive),
        span_json(region.source_span)
    );
    for (index, form) in region.forms.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&form_json(form));
    }
    let _ = write!(
        out,
        "],\"formsData\":{},\"materialized\":{},\"data\":{}}}",
        json_string(&region.forms_data.to_string()),
        json_string(&region.materialized_source),
        json_string(&region.data.to_string())
    );
    out
}

fn grammar_name_for_region<'a>(
    artifacts: &'a SessionArtifacts,
    region: &StagedParseRegion,
) -> &'a str {
    artifacts
        .grammar_parse
        .grammars
        .iter()
        .find(|grammar| grammar.id == region.grammar)
        .map_or("<unknown>", |grammar| grammar.name.as_str())
}

fn form_json(form: &FrontendForm) -> String {
    match &form.kind {
        FrontendFormKind::Function(function) => format!(
            "{{\"kind\":\"frontend-function\",\"name\":{},\"span\":{},\"body\":{}}}",
            json_string(&function.name),
            span_json(form.source_span),
            json_string(&frontend_expression_label(&function.body))
        ),
        FrontendFormKind::Class(class) => {
            let mut out = String::new();
            let _ = write!(
                out,
                "{{\"kind\":\"frontend-class\",\"name\":{},\"superclass\":{},\"span\":{},\"slots\":[",
                json_string(&class.name),
                json_string(&class.superclass),
                span_json(form.source_span)
            );
            for (index, slot) in class.slots.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                let _ = write!(
                    out,
                    "{{\"name\":{},\"type\":{},\"constant\":{},\"initKeyword\":{},\"required\":{},\"span\":{}}}",
                    json_string(&slot.name),
                    json_string(&slot.type_name),
                    slot.constant,
                    json_string(slot.init_keyword.as_deref().unwrap_or("")),
                    slot.required_init_keyword,
                    span_json(slot.source_span)
                );
            }
            out.push_str("]}");
            out
        }
    }
}

fn span_json(span: Span) -> String {
    format!("{{\"start\":{},\"end\":{}}}", span.start, span.end)
}

fn related_objects_by_id(artifacts: &SessionArtifacts) -> BTreeMap<String, String> {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for region in &artifacts.grammar_parse.staged_regions {
        let region_rel = relation_for_region(region);
        map.entry(format!("grammar-package:{}", region.grammar_slug))
            .or_default()
            .push(region_rel.clone());
        for provenance in artifacts
            .grammar_parse
            .extension_provenance
            .iter()
            .filter(|provenance| {
                provenance.activation == region.activation
                    && provenance.object_slug == region.object_slug
            })
        {
            for id in [
                format!(
                    "grammar-rule:{}/{}",
                    region.grammar_slug, provenance.rule_name
                ),
                format!(
                    "effective-grammar-rule:{}/{}",
                    region.grammar_slug, provenance.rule_name
                ),
                format!(
                    "grammar-action:{}/{}",
                    region.grammar_slug, provenance.rule_name
                ),
            ] {
                map.entry(id).or_default().push(region_rel.clone());
            }
        }
        for (index, form) in region.forms.iter().enumerate() {
            let form_rel = relation_for_form(region, index);
            let flow_rel = flow_relation_for_form(region, index);
            map.entry(format!(
                "grammar-extension-emission:{}/{}/f{}",
                region.adapter, region.object_slug, index
            ))
            .or_default()
            .extend([region_rel.clone(), form_rel.clone(), flow_rel.clone()]);
            for object_id in object_ids_for_form(artifacts, form) {
                map.entry(object_id).or_default().extend([
                    region_rel.clone(),
                    form_rel.clone(),
                    flow_rel.clone(),
                ]);
            }
        }
    }
    map.into_iter()
        .map(|(id, relations)| {
            let mut unique = Vec::new();
            for relation in relations {
                if !unique.contains(&relation) {
                    unique.push(relation);
                }
            }
            (id, unique.join(" "))
        })
        .collect()
}

fn generated_objects_for_region<'a>(
    artifacts: &SessionArtifacts,
    region: &StagedParseRegion,
    objects: &'a [EnvironmentObject],
) -> Vec<&'a EnvironmentObject> {
    let mut ids = Vec::new();
    for form in &region.forms {
        ids.extend(object_ids_for_form(artifacts, form));
    }
    ids.sort();
    ids.dedup();
    objects
        .iter()
        .filter(|object| ids.iter().any(|id| id == object.id.as_str()))
        .collect()
}

fn object_ids_for_form(artifacts: &SessionArtifacts, form: &FrontendForm) -> Vec<String> {
    let mut ids = Vec::new();
    match &form.kind {
        FrontendFormKind::Function(function) => {
            for ast_function in &artifacts.ast.functions {
                if ast_function.name == function.name {
                    ids.push(format!("ast-function:{}", ast_function.definition));
                }
            }
            for function_def in &artifacts.sem.functions {
                if function_def.name == function.name {
                    ids.push(format!("function:f{}", function_def.id.0));
                }
            }
        }
        FrontendFormKind::Class(class_form) => {
            for ast_class in &artifacts.ast.classes {
                if ast_class.name == class_form.name {
                    ids.push(format!("ast-class:{}", ast_class.definition));
                }
            }
            let class_ids = artifacts
                .sem
                .classes
                .iter()
                .filter(|class| class.name == class_form.name)
                .map(|class| class.id)
                .collect::<Vec<_>>();
            for class_id in &class_ids {
                ids.push(format!("class:c{}", class_id.0));
            }
            for class in &artifacts.sem.classes {
                if !class_ids.contains(&class.id) {
                    continue;
                }
                for slot in &class.effective_slots {
                    ids.push(format!("slot:c{}.s{}", class.id.0, slot.id.0));
                    ids.push(format!("accessor:c{}.s{}", class.id.0, slot.id.0));
                    if slot.mutability == SlotMutability::Mutable {
                        ids.extend(slot_setter_object_ids(&artifacts.sem, class.id, slot.id));
                    }
                }
            }
        }
    }
    ids.sort();
    ids.dedup();
    ids
}

fn relation_for_generated_id(id: &str) -> String {
    if id.starts_with("ast-") {
        id.to_string()
    } else {
        format!("object:{id}")
    }
}

fn slot_setter_object_ids(
    module: &sem::Module,
    class: sem::ClassId,
    slot: sem::SlotId,
) -> Vec<String> {
    let mut ids = Vec::new();
    for generic in &module.generics {
        if matches!(
            generic.origin,
            GenericFunctionOrigin::SlotSetter {
                class: origin_class,
                slot: origin_slot,
            } if origin_class == class && origin_slot == slot
        ) {
            ids.push(format!("generic:g{}", generic.id.0));
        }
    }
    for method in &module.methods {
        if matches!(
            method.origin,
            MethodOrigin::SlotSetter {
                class: origin_class,
                slot: origin_slot,
            } if origin_class == class && origin_slot == slot
        ) {
            ids.push(format!("method:m{}", method.id.0));
        }
    }
    ids
}

fn report_for(reports: &[ArtifactReport], artifact: QueryArtifact) -> Option<&ArtifactReport> {
    reports.iter().find(|report| report.artifact == artifact)
}

fn artifact_label(artifact: QueryArtifact) -> String {
    artifact.label()
}

fn runtime_summary(result: Option<&RuntimeResult>) -> RuntimeSummary {
    match result {
        Some(RuntimeResult::Returned(value)) => RuntimeSummary {
            text: format!("returned {value}"),
            class: "metric--ok",
        },
        Some(RuntimeResult::MissingMain) => RuntimeSummary {
            text: "missing main".into(),
            class: "metric--empty",
        },
        Some(RuntimeResult::MainHasParameters) => RuntimeSummary {
            text: "main has parameters".into(),
            class: "metric--warn",
        },
        Some(result @ RuntimeResult::Trap { .. }) => RuntimeSummary {
            text: result.to_string(),
            class: "metric--warn",
        },
        Some(RuntimeResult::HostFunctionFailed { name, failure }) => RuntimeSummary {
            text: format!("host function `{name}` failed: {failure}"),
            class: "metric--warn",
        },
        Some(RuntimeResult::UnsupportedResult { declared }) => RuntimeSummary {
            text: format!("unsupported main result {declared}"),
            class: "metric--warn",
        },
        Some(result @ RuntimeResult::UnhandledCondition { .. }) => RuntimeSummary {
            text: result.to_string(),
            class: "metric--warn",
        },
        None => RuntimeSummary {
            text: "not requested".into(),
            class: "metric--empty",
        },
    }
}

struct RuntimeSummary {
    text: String,
    class: &'static str,
}

impl RuntimeSummary {
    fn as_str(&self) -> &str {
        &self.text
    }
}

fn grammar_stack_summary(artifacts: &SessionArtifacts) -> String {
    artifacts
        .grammar_parse
        .grammar_stack
        .grammars
        .iter()
        .filter_map(|grammar_id| {
            artifacts
                .grammar_parse
                .grammars
                .iter()
                .find(|grammar| grammar.id == *grammar_id)
                .map(|grammar| grammar.name.as_str())
        })
        .collect::<Vec<_>>()
        .join(" -> ")
}

fn grammar_source_kind(
    artifacts: &SessionArtifacts,
    grammar: grammar_frontend::GrammarId,
) -> &'static str {
    if artifacts.grammar_parse.rules.iter().any(|rule| {
        rule.grammar == grammar && rule.projection.origin == ProjectionOrigin::SourceAuthored
    }) {
        "source-authored"
    } else if artifacts.grammar_parse.rules.iter().any(|rule| {
        rule.grammar == grammar && rule.projection.origin == ProjectionOrigin::CompilerProjected
    }) {
        "compiler-projected"
    } else {
        "host-provided"
    }
}

fn short_source_name(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
}

fn frontend_expression_label(expression: &FrontendExpression) -> String {
    match expression {
        FrontendExpression::Integer { value, .. } => format!("integer {value}"),
        FrontendExpression::Float { value, .. } => format!("float {value}"),
        FrontendExpression::Negate { .. } => "negate".into(),
        FrontendExpression::Name { name, .. } => format!("name {name}"),
        FrontendExpression::Call { callee, args, .. } => {
            format!("call {callee}/{}", args.len())
        }
        FrontendExpression::Binary { op, .. } => format!("binary {}", op.as_str()),
    }
}

fn relation_for_region(region: &StagedParseRegion) -> String {
    format!("region:{}:{}", region.adapter, region.object_slug)
}

fn relation_for_form(region: &StagedParseRegion, index: usize) -> String {
    format!("form:{}:{}:{index}", region.adapter, region.object_slug)
}

fn flow_relation_for_form(region: &StagedParseRegion, index: usize) -> String {
    format!("flow:{}:{}:{index}", region.adapter, region.object_slug)
}

fn flow_relations_for_region(region: &StagedParseRegion) -> String {
    (0..region.forms.len())
        .map(|index| flow_relation_for_form(region, index))
        .collect::<Vec<_>>()
        .join(" ")
}

fn all_flow_relations(artifacts: &SessionArtifacts) -> String {
    artifacts
        .grammar_parse
        .staged_regions
        .iter()
        .map(flow_relations_for_region)
        .filter(|relations| !relations.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn artifact_relation_tokens(artifacts: &SessionArtifacts, artifact: QueryArtifact) -> String {
    let reports = artifacts.artifact_reports();
    let key = report_for(&reports, artifact)
        .map(|report| report.key.to_string())
        .unwrap_or_else(|| "not-emitted".to_string());
    let mut tokens = Vec::from([
        artifact_stage_relation(artifact).to_string(),
        artifact_key_relation(&key),
    ]);
    if matches!(
        artifact,
        QueryArtifact::Ast
            | QueryArtifact::Bind
            | QueryArtifact::LoweredModule
            | QueryArtifact::TapeProgram
            | QueryArtifact::RuntimeResult
    ) {
        tokens.extend(
            all_flow_relations(artifacts)
                .split_whitespace()
                .map(ToString::to_string),
        );
    }
    tokens.join(" ")
}

fn artifact_stage_relation(artifact: QueryArtifact) -> &'static str {
    match artifact {
        QueryArtifact::Source => "artifact-stage:source",
        QueryArtifact::EnvironmentResolution => "artifact-stage:environment",
        QueryArtifact::GrammarStack => "artifact-stage:grammar-stack",
        QueryArtifact::GrammarParse => "artifact-stage:grammar-parse",
        QueryArtifact::Parse => "artifact-stage:syntax",
        QueryArtifact::Ast => "artifact-stage:ast",
        QueryArtifact::Bind => "artifact-stage:sem",
        QueryArtifact::Diagnostics => "artifact-stage:diagnostics",
        QueryArtifact::MethodLookup(_) => "artifact-stage:method-lookup",
        QueryArtifact::MethodLowering(_) => "artifact-stage:method-lowering",
        QueryArtifact::MethodTape(_) => "artifact-stage:method-tape",
        QueryArtifact::GenericDispatcher(_) => "artifact-stage:generic-dispatcher",
        QueryArtifact::LoweredModule => "artifact-stage:lowered",
        QueryArtifact::TapeProgram => "artifact-stage:tape",
        QueryArtifact::RuntimeResult => "artifact-stage:runtime",
        QueryArtifact::TypingConfig => "artifact-stage:typing-config",
        QueryArtifact::DefinitionTyping(_) => "artifact-stage:definition-typing",
        QueryArtifact::Typing => "artifact-stage:typing",
    }
}

fn artifact_key_relation(key: &str) -> String {
    format!("artifact-key:{key}")
}

fn object_anchor(object_id: &str) -> String {
    format!("object-{}", anchor_slug(object_id))
}

fn object_by_id<'a>(objects: &'a [EnvironmentObject], id: &str) -> Option<&'a EnvironmentObject> {
    objects.iter().find(|object| object.id.as_str() == id)
}

fn artifact_anchor(key: &str) -> String {
    format!("artifact-{}", anchor_slug(key))
}

fn anchor_slug(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push('-');
        }
    }
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').to_string()
}

fn source_line_for_byte(source: &str, byte: usize) -> usize {
    source
        .char_indices()
        .take_while(|(index, _)| *index < byte)
        .filter(|(_, ch)| *ch == '\n')
        .count()
        + 1
}

fn parse_form_anchor(id: &str) -> String {
    format!("parse-form-{}", anchor_slug(id))
}

fn source_form_origin_label(origin: &SourceFormOrigin) -> String {
    match origin {
        SourceFormOrigin::BaseCst => "base CST".to_string(),
        SourceFormOrigin::StagedGrammar { region, form_index } => {
            format!("staged {region}/f{form_index}")
        }
    }
}

fn effective_rule_object_id_for_application(
    artifacts: &SessionArtifacts,
    application: &interloko_compiler::grammar_frontend::GrammarRuleApplication,
) -> Option<String> {
    artifacts
        .grammar_parse
        .effective_rules
        .iter()
        .find(|rule| rule.id == application.rule)
        .map(grammar_frontend::effective_grammar_rule_object_id)
}

fn parse_rule_relation_key(
    form_span: Span,
    application: &interloko_compiler::grammar_frontend::GrammarRuleApplication,
) -> String {
    format!(
        "parse-rule:{}-{}:{}:{}",
        form_span.start,
        form_span.end,
        anchor_slug(&application.rule_name),
        application.span.start
    )
}

fn parse_rule_depth(
    applications: &[interloko_compiler::grammar_frontend::GrammarRuleApplication],
    span: Span,
) -> usize {
    applications
        .iter()
        .filter(|candidate| candidate.span != span)
        .filter(|candidate| {
            candidate.span.start <= span.start
                && span.end <= candidate.span.end
                && candidate.span.end.saturating_sub(candidate.span.start)
                    > span.end.saturating_sub(span.start)
        })
        .count()
        .min(8)
}

fn parse_rule_bar_style(form_span: Span, rule_span: Span) -> String {
    let form_width = form_span.end.saturating_sub(form_span.start).max(1) as f64;
    let start = rule_span.start.max(form_span.start);
    let end = rule_span.end.min(form_span.end).max(start);
    let left = ((start.saturating_sub(form_span.start)) as f64 / form_width) * 100.0;
    let width = ((end.saturating_sub(start)) as f64 / form_width) * 100.0;
    format!(
        "--rule-left:{left:.2}%; --rule-width:{:.2}%;",
        width.max(3.0)
    )
}

#[derive(Clone)]
struct SourceBand {
    start: usize,
    end: usize,
    relation: String,
}

#[derive(Clone)]
struct SourceMark {
    start: usize,
    end: usize,
    relation: String,
    class: &'static str,
    label: &'static str,
}

impl SourceMark {
    fn clipped(&self, start: usize, end: usize) -> Option<Self> {
        if self.start >= end || self.end <= start {
            return None;
        }
        let mut mark = self.clone();
        mark.start = mark.start.max(start);
        mark.end = mark.end.min(end);
        Some(mark)
    }
}

fn source_bands(artifacts: &SessionArtifacts) -> Vec<SourceBand> {
    artifacts
        .grammar_parse
        .staged_regions
        .iter()
        .map(|region| SourceBand {
            start: region.source_span.start,
            end: region.source_span.end,
            relation: relation_for_region(region),
        })
        .collect()
}

fn source_marks(artifacts: &SessionArtifacts) -> Vec<SourceMark> {
    let mut marks = Vec::new();
    for region in &artifacts.grammar_parse.staged_regions {
        marks.push(SourceMark {
            start: region.activation_directive.start,
            end: region.activation_directive.end,
            relation: relation_for_region(region),
            class: "source-mark--activation",
            label: "grammar activation directive",
        });
        for (index, form) in region.forms.iter().enumerate() {
            marks.push(SourceMark {
                start: form.source_span.start,
                end: form.source_span.end,
                relation: format!(
                    "{} {}",
                    relation_for_form(region, index),
                    flow_relation_for_form(region, index)
                ),
                class: "source-mark--form",
                label: "frontend form source",
            });
        }
    }
    for (index, diagnostic) in artifacts.diagnostics.iter().enumerate() {
        marks.push(SourceMark {
            start: diagnostic.span.start,
            end: diagnostic.span.end,
            relation: format!("diagnostic:{index}"),
            class: "source-mark--diagnostic",
            label: "diagnostic",
        });
    }
    marks
}

fn intersecting_relations(bands: &[SourceBand], start: usize, end: usize) -> Vec<String> {
    let mut relations = Vec::new();
    for band in bands {
        if band.start < end && band.end > start && !relations.contains(&band.relation) {
            relations.push(band.relation.clone());
        }
    }
    relations
}

fn escape_html(input: &str) -> String {
    let mut out = String::new();
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn escape_attr(input: &str) -> String {
    escape_html(input)
}

fn json_string(input: &str) -> String {
    let mut out = String::from("\"");
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            ch if ch.is_control() => {
                let _ = write!(out, "\\u{:04x}", ch as u32);
            }
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn json_string_array(values: &[String]) -> String {
    let mut out = String::new();
    out.push('[');
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&json_string(value));
    }
    out.push(']');
    out
}

const REPORT_CSS: &str = r#"
:root {
  color-scheme: light;
  --ink: #17201d;
  --muted: #5f6a66;
  --surface: #ffffff;
  --surface-strong: #f4f7f6;
  --line: #d7dedb;
  --line-strong: #aab7b3;
  --accent: #9b4d2d;
  --accent-ink: #4f2110;
  --teal: #126d65;
  --green: #537a35;
  --violet: #5d5488;
  --warning: #a45517;
  --danger: #a93c3c;
  --shadow: 0 20px 60px rgba(23, 32, 29, 0.12);
  font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
}

* {
  box-sizing: border-box;
}

body {
  margin: 0;
  color: var(--ink);
  background:
    linear-gradient(90deg, rgba(23, 32, 29, 0.035) 1px, transparent 1px),
    linear-gradient(0deg, rgba(23, 32, 29, 0.035) 1px, transparent 1px),
    #f8faf9;
  background-size: 34px 34px;
}

code,
pre {
  font-family: "SFMono-Regular", Consolas, "Liberation Mono", Menlo, monospace;
}

a {
  color: inherit;
  text-decoration-color: rgba(18, 109, 101, 0.42);
  text-underline-offset: 2px;
}

.report-shell {
  min-height: 100vh;
}

.hero {
  min-height: 58vh;
  display: grid;
  grid-template-columns: minmax(0, 1fr) minmax(340px, 0.62fr);
  gap: 28px;
  padding: clamp(28px, 6vw, 72px);
  background: #17201d;
  color: #f6fbf8;
  border-bottom: 8px solid var(--accent);
}

.hero__copy {
  align-self: center;
  min-width: 0;
}

.eyebrow {
  margin: 0 0 10px;
  color: var(--teal);
  font-size: 0.76rem;
  font-weight: 800;
  letter-spacing: 0;
  text-transform: uppercase;
}

.hero .eyebrow {
  color: #8ed0c5;
}

.hero h1 {
  margin: 0;
  max-width: 820px;
  font-size: clamp(2.35rem, 5.3vw, 4.85rem);
  line-height: 1.02;
  letter-spacing: 0;
  overflow-wrap: anywhere;
}

.hero__summary {
  max-width: 820px;
  margin: 24px 0 0;
  color: #c6d2cd;
  font-size: 1.05rem;
  line-height: 1.65;
}

.hero__metrics {
  align-self: center;
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 12px;
}

.metric {
  min-height: 112px;
  padding: 16px;
  background: rgba(255, 255, 255, 0.07);
  border: 1px solid rgba(255, 255, 255, 0.16);
  border-radius: 8px;
}

.metric span {
  display: block;
  color: #a9b7b2;
  font-size: 0.78rem;
  font-weight: 700;
  text-transform: uppercase;
}

.metric strong {
  display: block;
  margin-top: 12px;
  color: #ffffff;
  font-size: clamp(1rem, 1.35vw, 1.18rem);
  line-height: 1.25;
  overflow-wrap: normal;
  word-break: normal;
}

.metric--ok {
  border-color: rgba(142, 208, 197, 0.55);
}

.metric--warn {
  border-color: rgba(250, 176, 95, 0.65);
}

.metric--accent {
  border-color: rgba(196, 112, 73, 0.75);
}

.pipeline-panel {
  margin-bottom: 18px;
  padding: 16px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: var(--surface-strong);
}

.pipeline-panel__copy {
  display: grid;
  grid-template-columns: minmax(180px, 0.35fr) minmax(0, 1fr);
  gap: 18px;
  align-items: start;
  margin-bottom: 14px;
}

.pipeline-panel__copy h3 {
  margin: 0;
  font-size: 1.05rem;
  letter-spacing: 0;
}

.pipeline-panel__copy p {
  margin: 0;
  color: var(--muted);
  line-height: 1.45;
}

.pipeline {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(132px, 1fr));
  gap: 8px;
  margin: 0;
  padding: 0;
  list-style: none;
}

.pipeline__node {
  position: relative;
  min-height: 70px;
  padding: 12px;
  background: var(--surface);
  color: var(--ink);
  border: 1px solid var(--line);
  border-radius: 8px;
  box-shadow: none;
}

.pipeline__node::after {
  display: none;
}

.pipeline__node span {
  display: block;
  margin-bottom: 8px;
  font-size: 0.76rem;
  font-weight: 800;
  text-transform: uppercase;
}

.pipeline__node code {
  display: block;
  color: var(--muted);
  font-size: 0.68rem;
  line-height: 1.35;
  overflow-wrap: anywhere;
}

.pipeline__node small {
  display: block;
  margin-top: 8px;
  color: var(--muted);
  font-size: 0.72rem;
  line-height: 1.35;
}

.report-main {
  padding: clamp(24px, 4vw, 56px);
}

.section {
  max-width: 1500px;
  margin: 0 auto 44px;
  padding: clamp(18px, 3vw, 32px);
  background: rgba(255, 255, 255, 0.86);
  border: 1px solid var(--line);
  border-radius: 8px;
  box-shadow: var(--shadow);
}

.section__heading {
  display: flex;
  justify-content: space-between;
  gap: 24px;
  align-items: start;
  margin-bottom: 20px;
  border-bottom: 1px solid var(--line);
  padding-bottom: 14px;
}

.section__heading > div {
  min-width: min(100%, 320px);
}

.section__heading h2,
.group-heading {
  margin: 0;
  font-size: clamp(1.4rem, 3vw, 2.2rem);
  letter-spacing: 0;
}

.section__intro {
  max-width: 680px;
  margin: 0;
  color: var(--muted);
  font-size: 0.96rem;
  line-height: 1.55;
}

.group-heading {
  margin: 28px 0 14px;
  font-size: 1rem;
  color: var(--muted);
  text-transform: uppercase;
}

.source-frame,
.dump,
.object-card,
.diagnostic-card,
.artifact-card,
.region-card {
  border: 1px solid var(--line);
  border-radius: 8px;
  background: var(--surface);
}

.source-frame {
  overflow: hidden;
}

.source-legend {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  margin: -4px 0 14px;
  color: var(--muted);
  font-size: 0.82rem;
}

.source-legend span {
  display: inline-flex;
  align-items: center;
  min-height: 28px;
  padding: 4px 8px;
  border: 1px solid var(--line);
  border-radius: 999px;
  background: var(--surface-strong);
}

.legend-swatch {
  display: inline-block;
  width: 0.85rem;
  height: 0.85rem;
  margin-right: 7px;
  border-radius: 3px;
}

.legend-swatch--activation {
  background: rgba(196, 112, 73, 0.62);
}

.legend-swatch--staged {
  background: rgba(18, 109, 101, 0.18);
  border: 1px solid rgba(142, 208, 197, 0.52);
}

.legend-swatch--form {
  background: rgba(147, 210, 199, 0.28);
  border-bottom: 2px solid #93d2c7;
}

.legend-swatch--diagnostic {
  background: rgba(227, 122, 122, 0.65);
}

.source-code {
  margin: 0;
  overflow: auto;
  background: #101614;
  color: #edf4f1;
  font-size: 0.88rem;
  line-height: 1.16;
}

.source-line {
  display: grid;
  grid-template-columns: 4rem minmax(0, 1fr);
  min-width: max-content;
}

.source-line--staged {
  background: rgba(18, 109, 101, 0.12);
}

.source-line__num {
  user-select: none;
  color: #74837e;
  padding: 0 14px;
  text-align: right;
  border-right: 1px solid rgba(255, 255, 255, 0.12);
}

.source-line__text {
  padding: 0 18px;
  white-space: pre;
}

.source-mark {
  padding: 0 2px;
  border-radius: 2px;
  color: #edf4f1;
}

.source-mark--activation {
  background: rgba(196, 112, 73, 0.42);
  box-shadow: inset 0 -2px #c47049;
}

.source-mark--form {
  background: rgba(147, 210, 199, 0.18);
  box-shadow: inset 0 -2px #93d2c7;
}

.source-mark--diagnostic {
  background: rgba(227, 122, 122, 0.62);
  box-shadow: inset 0 -2px #e37a7a;
}

.parse-grid {
  display: grid;
  gap: 16px;
}

.parse-card {
  overflow: hidden;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: var(--surface);
  box-shadow: var(--shadow);
}

.parse-card--staged {
  border-color: rgba(155, 77, 45, 0.34);
}

.parse-card__header {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto auto;
  gap: 12px;
  align-items: center;
  padding: 14px 16px;
  border-bottom: 1px solid var(--line);
  background: var(--surface-strong);
}

.parse-card__header div {
  min-width: 0;
}

.parse-card__header span,
.parse-lens__label {
  display: block;
  color: var(--accent);
  font-size: 0.72rem;
  font-weight: 850;
  letter-spacing: 0;
  text-transform: uppercase;
}

.parse-card__header strong {
  display: block;
  margin-top: 4px;
  overflow-wrap: anywhere;
}

.parse-card__header code {
  color: var(--muted);
}

.parse-card__body {
  display: grid;
  grid-template-columns: minmax(280px, 0.86fr) minmax(0, 1.14fr);
  gap: 0;
}

.parse-source {
  margin: 0;
  padding: 16px;
  max-height: 360px;
  overflow: auto;
  background: #101614;
  color: #edf4f1;
  font-size: 0.82rem;
  line-height: 1.22;
  white-space: pre;
}

.parse-source-token {
  padding: 0 1px;
  border-radius: 2px;
  background: transparent;
  color: inherit;
}

.parse-source-token.is-related,
.parse-source-token.is-focus {
  background: rgba(142, 208, 197, 0.32);
  box-shadow: inset 0 -2px #8ed0c5;
}

.parse-materialized {
  margin: 0;
  padding: 10px;
  overflow: auto;
  background: #101614;
  color: #edf4f1;
  font-size: 0.78rem;
  line-height: 1.24;
}

.parse-staged-meta,
.parse-frontend-form {
  padding: 9px 10px;
  border: 1px solid rgba(155, 77, 45, 0.24);
  background: rgba(155, 77, 45, 0.08);
}

.parse-staged-meta {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  align-items: center;
}

.parse-staged-meta a {
  font-weight: 800;
}

.parse-staged-meta code,
.parse-staged-meta span,
.parse-frontend-form small {
  color: var(--muted);
}

.parse-frontend-form strong,
.parse-frontend-form code,
.parse-frontend-form small {
  display: block;
}

.parse-frontend-form code {
  margin-top: 3px;
}

.parse-lens {
  display: grid;
  gap: 10px;
  padding: 16px;
  border-left: 1px solid var(--line);
}

.parse-rule-ladder {
  display: grid;
  gap: 6px;
}

.parse-rule-row {
  position: relative;
  min-height: 42px;
  margin-left: calc(var(--rule-depth) * 18px);
  padding: 7px 10px;
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 10px;
  align-items: center;
  border: 1px solid rgba(18, 109, 101, 0.28);
  background: rgba(18, 109, 101, 0.09);
  overflow: hidden;
}

.parse-rule-row.is-focus,
.parse-rule-row.is-related {
  border-color: rgba(18, 109, 101, 0.62);
  background: rgba(18, 109, 101, 0.16);
}

.parse-rule-row::before {
  content: "";
  position: absolute;
  left: var(--rule-left);
  right: calc(100% - var(--rule-left) - var(--rule-width));
  bottom: 0;
  height: 3px;
  background: rgba(18, 109, 101, 0.45);
}

.parse-rule-row a,
.parse-rule-row span {
  min-width: 0;
  overflow: hidden;
  font-weight: 750;
}

.parse-rule-row strong,
.parse-rule-row small {
  display: block;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.parse-rule-row strong {
  font-size: 0.92rem;
}

.parse-rule-row small {
  margin-top: 2px;
  color: var(--muted);
  font-size: 0.7rem;
  font-weight: 650;
}

.parse-rule-row code {
  flex: 0 0 auto;
  color: var(--muted);
}

.parse-token-strip {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}

.parse-token {
  display: inline-flex;
  gap: 6px;
  align-items: center;
  padding: 4px 7px;
  border: 1px solid rgba(155, 77, 45, 0.24);
  background: rgba(155, 77, 45, 0.08);
  font-size: 0.82rem;
}

.parse-token code {
  color: var(--muted);
  font-size: 0.72rem;
}

.parse-cst-path {
  margin: 0;
  padding: 0;
  display: grid;
  gap: 4px;
  list-style: none;
}

.parse-cst-path li {
  margin-left: calc(var(--depth) * 14px);
  padding: 4px 7px;
  display: flex;
  justify-content: space-between;
  gap: 8px;
  border-left: 3px solid rgba(93, 84, 136, 0.35);
  background: rgba(93, 84, 136, 0.07);
}

.parse-diagnostics {
  display: grid;
  gap: 6px;
}

.parse-diagnostic {
  padding: 8px 10px;
  border: 1px solid rgba(169, 60, 60, 0.32);
  background: rgba(169, 60, 60, 0.08);
}

.parse-diagnostic strong,
.parse-diagnostic span,
.parse-diagnostic small {
  display: block;
}

.parse-diagnostic small,
.parse-empty {
  color: var(--muted);
}

.region-grid {
  display: grid;
  gap: 18px;
}

.grammar-layout {
  display: grid;
  grid-template-columns: minmax(0, 1.35fr) minmax(280px, 0.85fr);
  gap: 14px;
}

.grammar-panel {
  padding: 18px;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: 8px;
  box-shadow: var(--shadow);
}

.grammar-panel__title {
  display: flex;
  justify-content: space-between;
  align-items: start;
  gap: 16px;
  margin-bottom: 12px;
}

.grammar-panel__title span {
  font-weight: 850;
  font-size: 1.1rem;
}

.grammar-panel__title code {
  color: var(--muted);
  overflow-wrap: anywhere;
  text-align: right;
}

.grammar-panel p {
  margin: 0 0 14px;
  color: var(--muted);
  line-height: 1.5;
}

.grammar-stack-list {
  display: grid;
  gap: 8px;
  margin: 0;
  padding: 0;
  list-style: none;
}

.grammar-stack-list li,
.grammar-registry-links a {
  display: grid;
  gap: 4px;
  padding: 10px;
  background: var(--surface-strong);
  border: 1px solid var(--line);
  border-radius: 8px;
}

.grammar-stack-list a,
.grammar-registry-links a {
  color: var(--ink);
  font-weight: 800;
  text-decoration: none;
}

.grammar-stack-list span {
  justify-self: start;
  padding: 2px 8px;
  border-radius: 999px;
  background: #d8ebe7;
  color: #2c635c;
  font-size: 0.72rem;
  font-weight: 800;
  text-transform: uppercase;
}

.grammar-stack-list small,
.grammar-registry-links code {
  color: var(--muted);
  overflow-wrap: anywhere;
}

.grammar-registry-links {
  display: grid;
  gap: 8px;
}

.region-card {
  overflow: hidden;
}

.region-card__head {
  display: flex;
  justify-content: space-between;
  gap: 18px;
  align-items: start;
  padding: 18px 20px;
  background: #26302c;
  color: #f9fcfb;
}

.region-card__head h3 {
  margin: 0;
  font-size: 1.35rem;
}

.region-card__head code,
.region-card__head p {
  color: #cbd8d3;
}

.region-card__head p {
  margin: 8px 0 0;
}

.expansion-chain {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(250px, 1fr));
  gap: 12px;
  padding: 14px;
  background: var(--surface-strong);
}

.chain-step {
  min-width: 0;
  padding: 16px;
  border-left: 1px solid var(--line);
}

.chain-step--adapter {
  grid-column: 1 / -1;
  display: grid;
  grid-template-columns: minmax(120px, 0.2fr) minmax(0, 1fr);
  gap: 12px;
  align-items: start;
  border-left: 0;
  border-bottom: 1px solid var(--line);
}

.chain-step strong,
.chain-step small {
  display: block;
  min-width: 0;
  overflow-wrap: anywhere;
}

.chain-step pre,
.dump pre {
  margin: 12px 0 0;
  overflow: auto;
  padding: 14px;
  background: #101614;
  color: #edf4f1;
  border-radius: 6px;
  line-height: 1.55;
}

.step-label {
  display: block;
  margin-bottom: 12px;
  color: var(--accent);
  font-size: 0.73rem;
  font-weight: 800;
  text-transform: uppercase;
}

.form-row {
  padding: 12px 0;
  border-top: 1px solid var(--line);
}

.form-row:first-of-type {
  border-top: 0;
}

.form-row code {
  display: block;
  margin-top: 6px;
  color: var(--teal);
  white-space: normal;
  overflow-wrap: anywhere;
}

.form-row p,
.form-row small {
  color: var(--muted);
}

.slot-list,
.object-links,
.stage-links,
.label-list {
  margin: 10px 0 0;
  padding: 0;
  list-style: none;
}

.slot-list li,
.object-links li,
.stage-links li {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  padding: 7px 0;
  border-top: 1px solid var(--line);
}

.slot-list code,
.slot-list span {
  display: inline-flex;
  align-items: center;
  min-height: 24px;
  padding: 2px 7px;
  border: 1px solid var(--line);
  border-radius: 999px;
  background: var(--surface-strong);
  white-space: normal;
}

.object-links li,
.stage-links li {
  display: block;
}

.object-links code,
.object-links span,
.stage-links code,
.stage-links span {
  display: block;
  min-width: 0;
  overflow-wrap: anywhere;
}

.object-links span,
.stage-links span {
  margin-top: 4px;
  color: var(--muted);
}

.object-grid,
.artifact-grid,
.diagnostic-list {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
  gap: 12px;
}

.object-card,
.diagnostic-card,
.artifact-card {
  padding: 15px;
}

.object-card__title {
  display: block;
}

.object-card__title span,
.object-card__title code {
  display: block;
  min-width: 0;
}

.object-card__title span {
  font-weight: 800;
  line-height: 1.25;
  overflow-wrap: break-word;
  word-break: normal;
}

.object-card__title code {
  margin-top: 7px;
  color: var(--muted);
  font-size: 0.76rem;
  line-height: 1.35;
  overflow-wrap: anywhere;
}

.object-card p {
  color: var(--muted);
  line-height: 1.45;
}

.doc-comment-block {
  margin: 12px 0;
  padding: 10px 12px;
  border-left: 3px solid var(--teal);
  background: rgba(62, 126, 112, 0.08);
}

.doc-comment-block span {
  display: block;
  margin-bottom: 6px;
  color: var(--accent);
  font-size: 0.7rem;
  font-weight: 850;
  letter-spacing: 0;
  text-transform: uppercase;
}

.doc-comment-block p {
  margin: 0;
  color: var(--text);
  line-height: 1.45;
}

.doc-comment-block p + p {
  margin-top: 4px;
}

.meta-chip-row {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  margin: 10px 0;
}

.object-link-row {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  margin: 0 0 12px;
}

.object-link-row a {
  display: inline-flex;
  align-items: center;
  max-width: 100%;
  min-height: 26px;
  padding: 4px 8px;
  border: 1px solid var(--line);
  border-radius: 999px;
  background: var(--surface-strong);
  color: var(--teal);
  font-size: 0.76rem;
  font-weight: 700;
  text-decoration: none;
}

.object-link-row code {
  overflow-wrap: anywhere;
}

.meta-chip {
  display: inline-flex;
  align-items: center;
  min-height: 24px;
  padding: 3px 8px;
  border: 1px solid var(--line);
  border-radius: 999px;
  background: var(--surface-strong);
  color: var(--accent);
  font-size: 0.75rem;
  font-weight: 800;
}

.object-facts {
  margin: 12px 0;
}

.object-facts div {
  display: grid;
  grid-template-columns: minmax(74px, 0.42fr) minmax(0, 1fr);
  gap: 10px;
  padding: 7px 0;
  border-top: 1px solid var(--line);
}

.object-facts dt {
  color: var(--muted);
  font-size: 0.74rem;
  font-weight: 800;
  text-transform: uppercase;
}

.object-facts dd {
  margin: 0;
  min-width: 0;
  overflow-wrap: anywhere;
}

.raw-inspection {
  margin: 10px 0;
  color: var(--muted);
  font-size: 0.8rem;
}

.raw-inspection summary {
  cursor: pointer;
}

.raw-inspection code {
  display: block;
  max-height: 7rem;
  margin-top: 8px;
  overflow: auto;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}

.affordance-row {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}

.affordance-row span,
.artifact-card span {
  display: inline-flex;
  align-items: center;
  min-height: 24px;
  padding: 3px 8px;
  border: 1px solid var(--line);
  border-radius: 999px;
  background: var(--surface-strong);
  color: var(--muted);
  font-size: 0.75rem;
  font-weight: 700;
}

.diagnostic-card strong {
  color: var(--danger);
}

.diagnostic-card code,
.artifact-card code {
  display: block;
  margin-top: 8px;
  color: var(--teal);
  overflow-wrap: anywhere;
}

.artifact-card dl {
  margin: 12px 0 0;
}

.artifact-card dl div {
  display: flex;
  justify-content: space-between;
  gap: 10px;
  padding: 7px 0;
  border-top: 1px solid var(--line);
}

.artifact-card dt {
  color: var(--muted);
  font-size: 0.74rem;
  font-weight: 800;
  text-transform: uppercase;
}

.artifact-card dd {
  margin: 0;
  min-width: 0;
  overflow-wrap: anywhere;
}

.arg-list {
  margin: 12px 0 0;
}

.arg-list div {
  display: grid;
  grid-template-columns: 110px minmax(0, 1fr);
  gap: 10px;
}

.arg-list dt {
  color: var(--muted);
  font-weight: 800;
}

.arg-list dd {
  margin: 0;
}

.dump {
  margin-top: 14px;
  padding: 0;
  overflow: hidden;
}

.dump summary {
  cursor: pointer;
  padding: 14px 18px;
  font-weight: 800;
  background: var(--surface-strong);
  border-bottom: 1px solid var(--line);
}

.empty-state {
  padding: 20px;
  border: 1px dashed var(--line-strong);
  border-radius: 8px;
  color: var(--muted);
  background: var(--surface-strong);
}

.empty-state--small {
  padding: 12px;
  font-size: 0.9rem;
}

.trace-summary {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(130px, 1fr));
  gap: 10px;
  margin-bottom: 14px;
}

.trace-summary div {
  padding: 12px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: var(--surface-strong);
}

.trace-summary span {
  display: block;
  color: var(--muted);
  font-size: 0.74rem;
  font-weight: 800;
  text-transform: uppercase;
}

.trace-summary strong {
  display: block;
  margin-top: 4px;
  font-size: 1.45rem;
}

.trace-timeline {
  display: grid;
  gap: 8px;
  margin: 0;
  padding: 0;
  list-style: none;
}

.trace-event {
  display: grid;
  grid-template-columns: 4.5rem minmax(160px, 0.45fr) minmax(0, 1fr);
  gap: 10px;
  align-items: center;
  padding: 9px 12px;
  border: 1px solid var(--line);
  border-left: 4px solid var(--teal);
  border-radius: 8px;
  background: var(--surface);
}

.trace-event span {
  color: var(--muted);
  font-family: "SFMono-Regular", Consolas, "Liberation Mono", Menlo, monospace;
}

.trace-event strong,
.trace-event small {
  min-width: 0;
  overflow-wrap: anywhere;
}

.trace-event small {
  color: var(--muted);
}

.trace-event--run {
  border-left-color: var(--accent);
}

.trace-event--host {
  border-left-color: #c47049;
}

.trace-event--instr {
  border-left-color: #74837e;
}

.trace-event--more {
  opacity: 0.82;
}

.is-focus {
  outline: 3px solid var(--accent);
  outline-offset: 3px;
}

.is-related {
  box-shadow: 0 0 0 3px rgba(18, 109, 101, 0.26), var(--shadow);
}

@media (max-width: 1100px) {
  .hero {
    min-height: auto;
    grid-template-columns: 1fr;
  }

  .expansion-chain {
    grid-template-columns: 1fr;
  }
}

@media (max-width: 720px) {
  .hero,
  .report-main {
    padding: 20px;
  }

  .hero__metrics,
  .grammar-layout,
  .pipeline-panel__copy,
  .object-card__title,
  .section__heading,
  .region-card__head {
    display: block;
  }

  .section__intro {
    margin-top: 12px;
  }

  .source-line {
    grid-template-columns: 3rem minmax(0, 1fr);
  }

  .chain-step--adapter {
    display: block;
  }

  .arg-list div {
    grid-template-columns: 1fr;
  }

  .trace-event {
    grid-template-columns: 1fr;
  }
}
"#;

const REPORT_JS: &str = r#"
(function () {
  function keysFor(element) {
    var raw = ((element.dataset.node || "") + " " + (element.dataset.rel || "")).trim();
    if (!raw) {
      return [];
    }
    return raw.split(/\s+/);
  }

  function clear() {
    document.querySelectorAll(".is-focus, .is-related").forEach(function (element) {
      element.classList.remove("is-focus", "is-related");
    });
  }

  document.addEventListener("click", function (event) {
    var target = event.target.closest("[data-node], [data-rel]");
    if (!target) {
      clear();
      return;
    }
    var keys = keysFor(target);
    if (keys.length === 0) {
      return;
    }
    clear();
    target.classList.add("is-focus");
    document.querySelectorAll("[data-node], [data-rel]").forEach(function (element) {
      if (element === target) {
        return;
      }
      var elementKeys = keysFor(element);
      if (elementKeys.some(function (key) { return keys.indexOf(key) !== -1; })) {
        element.classList.add("is-related");
      }
    });
  });

  document.addEventListener("keydown", function (event) {
    if (event.key === "Escape") {
      clear();
    }
  });
})();
"#;
