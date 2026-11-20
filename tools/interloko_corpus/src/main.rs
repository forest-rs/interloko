//! Small-program corpus runner for Interloko compiler sessions.

use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use interloko_compiler::diagnostic::DiagnosticCode;
use interloko_compiler::grammar_frontend::{GrammarEnvironmentSelection, GrammarFrontendRegistry};
use interloko_compiler::lower::CodegenPolicy;
use interloko_compiler::sem::{HostClass, HostMetaRegistry, HostSlot, ValueKind};
use interloko_compiler::session::{
    CompilerSession, DumpStage, FunctionCallError, FunctionValue, IncrementalCompilerSession,
    RunLimits, RuntimeMode, RuntimeResult, TurnStats,
};
use interloko_compiler::typing::TypingConfig;
use interloko_diagnostics::DiagnosticRenderer;

type Result<T> = std::result::Result<T, CorpusError>;

fn main() {
    match run(env::args().skip(1).collect()) {
        Ok(Outcome::Success) => {}
        Ok(Outcome::CheckFailed) => std::process::exit(1),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(error.exit_code());
        }
    }
}

fn run(args: Vec<String>) -> Result<Outcome> {
    let command = args.first().map_or("check", String::as_str);
    match command {
        "check" => check_command(args.get(1).map(String::as_str)),
        "list" => list_command(args.get(1).map(String::as_str)),
        "dump" => dump_command(&args),
        "heap" => heap_command(&args[1..]),
        "strictness" => strictness_command(args.get(1).map(String::as_str)),
        "-h" | "--help" | "help" => Err(CorpusError::Usage(usage())),
        _ => Err(CorpusError::Usage(usage())),
    }
}

fn check_command(root: Option<&str>) -> Result<Outcome> {
    let root = root.map_or_else(default_corpus_root, PathBuf::from);
    let cases = load_cases(&root)?;
    let mut passed = 0_usize;
    let mut failed = 0_usize;

    for case in &cases {
        let failures = check_case(case);
        if failures.is_empty() {
            passed += 1;
            println!("ok {}", case.display_path);
        } else {
            println!("FAILED {}", case.display_path);
            for failure in &failures {
                println!("  {failure}");
            }
            failed += 1;
        }
    }

    println!(
        "corpus: {passed} passed, {} failed, {} total",
        failed,
        cases.len()
    );

    if failed == 0 {
        Ok(Outcome::Success)
    } else {
        Ok(Outcome::CheckFailed)
    }
}

/// Runs `main` in every program under the given roots and reports what each
/// turn allocated and what was still reachable at its end.
///
/// Programs need no expectation header, so example program directories can
/// be measured too; programs that do not compile, have no zero-argument
/// `main`, or trap are skipped.
fn heap_command(roots: &[String]) -> Result<Outcome> {
    let roots: Vec<PathBuf> = if roots.is_empty() {
        vec![default_corpus_root()]
    } else {
        roots.iter().map(PathBuf::from).collect()
    };
    let mut total = TurnStats::default();
    let (mut measured, mut skipped) = (0_usize, 0_usize);
    println!(
        "program objects reachable/allocated boxes reachable/allocated bytes reachable/allocated survival"
    );
    for root in &roots {
        let mut paths = Vec::new();
        collect_loko_files(root, &mut paths)?;
        paths.sort();
        for path in paths {
            let source = read_to_string(&path)?;
            let (host_registry, grammar_environment) = match parse_header(&path, &source) {
                Ok((_, host_registry, grammar_environment)) => (host_registry, grammar_environment),
                Err(_) => (
                    HostMetaRegistry::default(),
                    GrammarEnvironmentSelection::default(),
                ),
            };
            let case = Case {
                display_path: path.display().to_string(),
                typing_config: parse_typing_config(&path, &source).unwrap_or_default(),
                codegen_policy: parse_codegen_policy(&path, &source).unwrap_or_default(),
                run_limits: parse_run_limits(&path, &source).unwrap_or_default(),
                source,
                expectations: Vec::new(),
                host_registry,
                grammar_environment,
            };
            let artifacts = compile_case(&case, RuntimeMode::CompileOnly);
            let Ok(outcome) = artifacts.call_function("main", &[], None) else {
                skipped += 1;
                continue;
            };
            let turn = outcome.turn;
            println!(
                "{} {}/{} {}/{} {}/{} {}",
                case.display_path,
                turn.reachable.objects,
                turn.allocated.objects,
                turn.reachable.boxed_values,
                turn.allocated.boxed_values,
                turn.reachable.bytes,
                turn.allocated.bytes,
                survival_label(&turn),
            );
            for (sum, part) in [
                (&mut total.allocated, turn.allocated),
                (&mut total.reachable, turn.reachable),
            ] {
                sum.objects += part.objects;
                sum.boxed_values += part.boxed_values;
                sum.bytes += part.bytes;
            }
            measured += 1;
        }
    }
    println!(
        "heap: {measured} measured, {skipped} skipped; objects {}/{} boxes {}/{} bytes {}/{} survival {}",
        total.reachable.objects,
        total.allocated.objects,
        total.reachable.boxed_values,
        total.allocated.boxed_values,
        total.reachable.bytes,
        total.allocated.bytes,
        survival_label(&total),
    );
    Ok(Outcome::Success)
}

/// Compiles every case as if the session held every definition to strict
/// typing (`strict.scope=all`, on top of the case's own typing header) and
/// reports how much of the corpus would pass: per case, its strict errors
/// by kind (`unproved-dispatch`, `refuted-cast`, ...), then totals. Cases semantic analysis
/// rejects are not typed and are counted apart.
///
/// This measures how usable strict typing is on code written without it;
/// it changes nothing and always succeeds.
fn strictness_command(root: Option<&str>) -> Result<Outcome> {
    let root = root.map_or_else(default_corpus_root, PathBuf::from);
    let cases = load_cases(&root)?;
    let (mut typed, mut passing, mut untyped) = (0_usize, 0_usize, 0_usize);
    let (mut definitions, mut clean_definitions) = (0_usize, 0_usize);
    let mut codes = std::collections::BTreeMap::<String, usize>::new();
    for case in &cases {
        let mut strict = case.clone();
        strict.typing_config = case
            .typing_config
            .apply_spec("strict.scope=all")
            .expect("a valid strict setting");
        let artifacts = compile_case(&strict, RuntimeMode::CompileOnly);
        if artifacts.typing.status != interloko_compiler::typing::TypingStatus::Checked {
            untyped += 1;
            println!("untyped {}", case.display_path);
            continue;
        }
        typed += 1;
        let typing = &artifacts.typing;
        let mut errors = std::collections::BTreeMap::<String, usize>::new();
        for decision in typing.decisions() {
            if let Some(violation) = typing.strict_violation(decision) {
                let kind = format!("{}-{}", violation.as_str(), decision.check.as_str());
                *errors.entry(kind).or_default() += 1;
            }
        }
        definitions += typing.strict.len();
        clean_definitions += typing
            .strict
            .iter()
            .filter(|strict| {
                typing.definition(strict.definition).is_none_or(|typed| {
                    typed
                        .decisions
                        .iter()
                        .all(|decision| typing.strict_violation(decision).is_none())
                })
            })
            .count();
        if errors.is_empty() {
            passing += 1;
            println!("pass {}", case.display_path);
        } else {
            let summary = errors
                .iter()
                .map(|(code, count)| format!("{code}={count}"))
                .collect::<Vec<_>>()
                .join(" ");
            println!("fail {} {summary}", case.display_path);
        }
        for (code, count) in errors {
            *codes.entry(code).or_default() += count;
        }
    }
    let totals = codes
        .iter()
        .map(|(code, count)| format!("{code}={count}"))
        .collect::<Vec<_>>()
        .join(" ");
    println!(
        "strictness: {passing}/{typed} typed cases pass, {clean_definitions}/{definitions} definitions; \
         {untyped} untyped; errors {totals}"
    );
    Ok(Outcome::Success)
}

fn survival_label(turn: &TurnStats) -> String {
    turn.survival_ratio()
        .map_or_else(|| "-".into(), |ratio| format!("{:.1}%", ratio * 100.0))
}

fn list_command(root: Option<&str>) -> Result<Outcome> {
    let root = root.map_or_else(default_corpus_root, PathBuf::from);
    for case in load_cases(&root)? {
        println!("{}", case.display_path);
    }
    Ok(Outcome::Success)
}

fn dump_command(args: &[String]) -> Result<Outcome> {
    if args.len() != 4 || args[2] != "--stage" {
        return Err(CorpusError::Usage(
            "usage: interloko_corpus dump <file.loko> --stage syntax|ast|sem|mop|eop|lowered|tape|typing|types|all"
                .into(),
        ));
    }
    let stage = parse_stage(&args[3]).ok_or_else(|| {
        CorpusError::Usage(
            "stage must be one of syntax|ast|sem|mop|eop|lowered|tape|typing|types|all".into(),
        )
    })?;
    let source = read_to_string(Path::new(&args[1]))?;
    let grammar_registry = GrammarFrontendRegistry::builtin();
    let host_registry = parse_host_registry(Path::new(&args[1]), &source)?;
    let grammar_environment =
        parse_grammar_environment(Path::new(&args[1]), &source, &grammar_registry)?;
    let mut session = CompilerSession::with_grammar_registry(grammar_registry);
    session.set_typing_config(parse_typing_config(Path::new(&args[1]), &source)?);
    session.set_codegen_policy(parse_codegen_policy(Path::new(&args[1]), &source)?);
    session.set_run_limits(parse_run_limits(Path::new(&args[1]), &source)?);
    let source_file = session.add_source_with_grammar(&args[1], &source, grammar_environment);
    let artifacts = session
        .compile_with_host_registry(source_file, RuntimeMode::CompileOnly, &host_registry)
        .expect("source was just added");
    print!("{}", artifacts.dump_stage(stage));
    Ok(Outcome::Success)
}

fn load_cases(root: &Path) -> Result<Vec<Case>> {
    let mut paths = Vec::new();
    collect_loko_files(root, &mut paths)?;
    paths.sort();

    let mut cases = Vec::with_capacity(paths.len());
    for path in paths {
        let source = read_to_string(&path)?;
        let (expectations, host_registry, grammar_environment) = parse_header(&path, &source)?;
        let typing_config = parse_typing_config(&path, &source)?;
        let codegen_policy = parse_codegen_policy(&path, &source)?;
        let run_limits = parse_run_limits(&path, &source)?;
        let display_path = display_path(root, &path);
        cases.push(Case {
            display_path,
            source,
            expectations,
            host_registry,
            grammar_environment,
            typing_config,
            codegen_policy,
            run_limits,
        });
    }
    Ok(cases)
}

fn collect_loko_files(root: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    let entries = fs::read_dir(root).map_err(|error| CorpusError::Io {
        path: root.display().to_string(),
        error,
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| CorpusError::Io {
            path: root.display().to_string(),
            error,
        })?;
        let path = entry.path();
        if path.is_dir() {
            collect_loko_files(&path, paths)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "loko")
        {
            paths.push(path);
        }
    }
    Ok(())
}

fn parse_header(
    path: &Path,
    source: &str,
) -> Result<(
    Vec<Expectation>,
    HostMetaRegistry,
    GrammarEnvironmentSelection,
)> {
    let mut expectations = Vec::new();
    let mut host_registry = HostMetaRegistry::new();
    let grammar_registry = GrammarFrontendRegistry::builtin();
    let mut grammar_environment = GrammarEnvironmentSelection::base_loko();
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            if let Some(rest) = trimmed.strip_prefix("// expect:") {
                expectations.push(parse_expectation(path, index + 1, rest.trim())?);
            } else if let Some(rest) = trimmed.strip_prefix("// host-class:") {
                host_registry.add_class(parse_host_class(path, index + 1, rest.trim())?);
            } else if let Some(rest) = trimmed.strip_prefix("// grammar:") {
                grammar_environment =
                    parse_grammar_selection(path, index + 1, rest.trim(), &grammar_registry)?;
            }
            continue;
        }
        break;
    }

    if expectations.is_empty() {
        return Err(CorpusError::InvalidHeader {
            path: path.display().to_string(),
            line: 1,
            message: "missing // expect: header".into(),
        });
    }
    Ok((expectations, host_registry, grammar_environment))
}

fn parse_host_registry(path: &Path, source: &str) -> Result<HostMetaRegistry> {
    let mut host_registry = HostMetaRegistry::new();
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            if let Some(rest) = trimmed.strip_prefix("// host-class:") {
                host_registry.add_class(parse_host_class(path, index + 1, rest.trim())?);
            }
            continue;
        }
        break;
    }
    Ok(host_registry)
}

fn parse_grammar_environment(
    path: &Path,
    source: &str,
    registry: &GrammarFrontendRegistry,
) -> Result<GrammarEnvironmentSelection> {
    let mut selection = GrammarEnvironmentSelection::base_loko();
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            if let Some(rest) = trimmed.strip_prefix("// grammar:") {
                selection = parse_grammar_selection(path, index + 1, rest.trim(), registry)?;
            }
            continue;
        }
        break;
    }
    Ok(selection)
}

fn parse_typing_config(path: &Path, source: &str) -> Result<TypingConfig> {
    let mut config = TypingConfig::default();
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            if let Some(rest) = trimmed.strip_prefix("// typing:") {
                config = TypingConfig::default()
                    .apply_spec(rest)
                    .map_err(|error| invalid_header(path, index + 1, &error.to_string()))?;
            }
            continue;
        }
        break;
    }
    Ok(config)
}

/// The run limits a `// call-depth: N` header sets, or the defaults.
fn parse_run_limits(path: &Path, source: &str) -> Result<RunLimits> {
    let mut limits = RunLimits::default();
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            if let Some(rest) = trimmed.strip_prefix("// call-depth:") {
                limits.max_call_depth = rest.trim().parse().map_err(|_| {
                    invalid_header(path, index + 1, "call-depth must be a frame count")
                })?;
            }
            continue;
        }
        break;
    }
    Ok(limits)
}

fn parse_codegen_policy(path: &Path, source: &str) -> Result<CodegenPolicy> {
    let mut policy = CodegenPolicy::default();
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            if let Some(rest) = trimmed.strip_prefix("// codegen:") {
                policy = CodegenPolicy::from_label(rest.trim()).ok_or_else(|| {
                    invalid_header(
                        path,
                        index + 1,
                        &format!(
                            "codegen must be one of {}",
                            CodegenPolicy::ALL
                                .iter()
                                .map(|policy| format!("`{}`", policy.as_str()))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    )
                })?;
            }
            continue;
        }
        break;
    }
    Ok(policy)
}

fn parse_grammar_selection(
    path: &Path,
    line: usize,
    input: &str,
    registry: &GrammarFrontendRegistry,
) -> Result<GrammarEnvironmentSelection> {
    registry
        .environment_selection(input)
        .ok_or_else(|| invalid_header(path, line, &grammar_header_message(registry)))
}

fn grammar_header_message(registry: &GrammarFrontendRegistry) -> String {
    format!(
        "grammar must be one of {}",
        registry
            .environment_slugs()
            .into_iter()
            .map(|slug| format!("`{slug}`"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn parse_host_class(path: &Path, line: usize, input: &str) -> Result<HostClass> {
    let mut parts = input.split_whitespace();
    let name = parts
        .next()
        .ok_or_else(|| invalid_header(path, line, "host-class requires a class name"))?;
    if !name.starts_with('<') || !name.ends_with('>') {
        return Err(invalid_header(
            path,
            line,
            "host-class name must be a Loko class name",
        ));
    }
    let mut class = HostClass::new(name);
    for part in parts {
        let Some((slot_name, type_name)) = part.split_once(':') else {
            return Err(invalid_header(
                path,
                line,
                "host-class slots must be `slot:<type>`",
            ));
        };
        let kind = parse_host_slot_kind(path, line, type_name)?;
        class = class.with_slot(HostSlot::new(slot_name, kind, type_name));
    }
    Ok(class)
}

fn parse_host_slot_kind(path: &Path, line: usize, input: &str) -> Result<ValueKind> {
    match input {
        "<object>" => Ok(ValueKind::Any),
        "<integer>" => Ok(ValueKind::I64),
        "<boolean>" => Ok(ValueKind::Bool),
        _ => Err(invalid_header(
            path,
            line,
            "host-class corpus slots currently support <object>, <integer>, or <boolean>",
        )),
    }
}

/// Parses the Loko literal text of an expected `main` result.
///
/// The accepted spellings are exactly the ones [`FunctionValue`]'s `Display`
/// produces, so a run expectation always names the value's type as well as
/// its contents: `42` is an integer, `42.0` a float, `#t` a boolean, and
/// `"42"` a string, `'4'` a character. Floats also accept `inf`, `-inf`,
/// and `nan`.
fn parse_run_value(text: &str) -> Option<FunctionValue> {
    match text {
        "#t" => return Some(FunctionValue::Boolean(true)),
        "#f" => return Some(FunctionValue::Boolean(false)),
        _ => {}
    }
    if text.starts_with('"') {
        return interloko_compiler::ast::decode_string_literal(text).map(FunctionValue::String);
    }
    if text.starts_with('\'') {
        return interloko_compiler::ast::decode_character_literal(text)
            .map(FunctionValue::Character);
    }
    if let Some(digits) = text.strip_suffix("i32") {
        return digits.parse().ok().map(FunctionValue::I32);
    }
    if let Some(digits) = text.strip_suffix("u32") {
        return digits.parse().ok().map(FunctionValue::U32);
    }
    if let Some(digits) = text.strip_suffix("f32") {
        return match digits {
            "inf" => Some(f32::INFINITY),
            "-inf" => Some(f32::NEG_INFINITY),
            "nan" => Some(f32::NAN),
            _ => digits.parse().ok(),
        }
        .map(FunctionValue::SingleFloat);
    }
    match text {
        "inf" => return Some(FunctionValue::Float(f64::INFINITY)),
        "-inf" => return Some(FunctionValue::Float(f64::NEG_INFINITY)),
        "nan" => return Some(FunctionValue::Float(f64::NAN)),
        _ => {}
    }
    if text.contains(['.', 'e', 'E']) {
        return text.parse().ok().map(FunctionValue::Float);
    }
    text.parse().ok().map(FunctionValue::Integer)
}

/// Compares a run result with its expectation by literal identity.
///
/// Rendered literal text distinguishes value types and, for floats, compares
/// exact values rather than IEEE equality: `nan` matches a NaN result and
/// `-0.0` does not match `0.0`.
fn run_value_matches(actual: &FunctionValue, expected: &FunctionValue) -> bool {
    actual.kind() == expected.kind() && actual.to_string() == expected.to_string()
}

fn parse_expectation(path: &Path, line: usize, input: &str) -> Result<Expectation> {
    if let Some(text) = input.strip_prefix("run reports ") {
        return Ok(Expectation::RunReports(text.into()));
    }

    if input == "run blocked" {
        return Ok(Expectation::RunBlocked);
    }

    if let Some(value) = input.strip_prefix("run ") {
        let value = parse_run_value(value).ok_or_else(|| {
            invalid_header(
                path,
                line,
                "run expectation must be an integer, float, #t, #f, or a string literal",
            )
        })?;
        return Ok(Expectation::Run(value));
    }

    if let Some(code) = input.strip_prefix("runtime code ") {
        let code = DiagnosticCode::parse(code)
            .ok_or_else(|| invalid_header(path, line, "unknown diagnostic code"))?;
        return Ok(Expectation::RuntimeCode(code));
    }

    if let Some(value) = input.strip_prefix("diagnostics ") {
        let value = value
            .parse()
            .map_err(|_| invalid_header(path, line, "diagnostics expectation must be a count"))?;
        return Ok(Expectation::Diagnostics(value));
    }

    for prefix in ["diagnostic contains ", "diagnostics contains "] {
        if let Some(text) = input.strip_prefix(prefix) {
            return Ok(Expectation::DiagnosticContains(text.into()));
        }
    }

    for prefix in ["diagnostic code ", "diagnostics code "] {
        if let Some(code) = input.strip_prefix(prefix) {
            let code = DiagnosticCode::parse(code)
                .ok_or_else(|| invalid_header(path, line, "unknown diagnostic code"))?;
            return Ok(Expectation::DiagnosticCode(code));
        }
    }

    if let Some(rest) = input.strip_prefix("dump ") {
        let (stage, text, lacks) = match (rest.split_once(" contains "), rest.split_once(" lacks "))
        {
            (Some((stage, text)), _) => (stage, text, false),
            (None, Some((stage, text))) => (stage, text, true),
            (None, None) => {
                return Err(invalid_header(
                    path,
                    line,
                    "dump expectation must be `dump STAGE contains TEXT` or `dump STAGE lacks TEXT`",
                ));
            }
        };
        let stage = parse_stage(stage).ok_or_else(|| {
            invalid_header(
                path,
                line,
                "dump stage must be syntax|ast|sem|mop|eop|lowered|tape|typing|types|all",
            )
        })?;
        let text = text.into();
        return Ok(if lacks {
            Expectation::DumpLacks { stage, text }
        } else {
            Expectation::DumpContains { stage, text }
        });
    }

    Err(invalid_header(path, line, "unknown expectation"))
}

fn check_case(case: &Case) -> Vec<String> {
    let mode = if case.expectations.iter().any(|expectation| {
        matches!(
            expectation,
            Expectation::Run(_)
                | Expectation::RunBlocked
                | Expectation::RuntimeCode(_)
                | Expectation::RunReports(_)
        )
    }) {
        RuntimeMode::RunMain
    } else {
        RuntimeMode::CompileOnly
    };
    let artifacts = compile_case(case, mode);
    let mut failures = Vec::new();
    let renderer = DiagnosticRenderer::english();

    for expectation in &case.expectations {
        match expectation {
            Expectation::Run(expected) => match &artifacts.runtime_result {
                Some(actual)
                    if actual
                        .value()
                        .is_some_and(|actual| run_value_matches(actual, expected)) => {}
                Some(actual) => {
                    failures.push(format!("expected run result {expected}, got {actual}"))
                }
                None => failures.push(format!(
                    "expected run result {expected}, but {}",
                    did_not_run(&artifacts)
                )),
            },
            Expectation::RunBlocked => {
                match (&artifacts.runtime_result, artifacts.check_runnable()) {
                    (None, Err(FunctionCallError::CompilationFailed { .. })) => {}
                    (Some(actual), _) => failures.push(format!(
                        "expected compilation errors to block main, but it ran: {actual}"
                    )),
                    (None, _) => failures.push(format!(
                        "expected compilation errors to block main, but {}",
                        did_not_run(&artifacts)
                    )),
                }
            }
            Expectation::RuntimeCode(expected) => match &artifacts.runtime_result {
                Some(actual) if actual.code() == Some(*expected) => {}
                Some(actual) => failures.push(format!(
                    "expected runtime outcome `{expected}`, got {actual}"
                )),
                None => failures.push(format!(
                    "expected runtime outcome `{expected}`, but {}",
                    did_not_run(&artifacts)
                )),
            },
            Expectation::RunReports(text) => match &artifacts.runtime_result {
                Some(actual) if actual.to_string().contains(text.as_str()) => {}
                Some(actual) => {
                    failures.push(format!(
                        "expected run result containing `{text}`, got {actual}"
                    ));
                }
                None => failures.push(format!(
                    "expected run result containing `{text}`, but {}",
                    did_not_run(&artifacts)
                )),
            },
            Expectation::Diagnostics(expected) => {
                let actual = artifacts.diagnostics.len();
                if actual != *expected {
                    failures.push(format!("expected {expected} diagnostics, got {actual}"));
                }
            }
            Expectation::DiagnosticContains(text) => {
                let Ok(renderer) = &renderer else {
                    failures.push("failed to compile English diagnostic catalog".into());
                    continue;
                };
                let mut found = false;
                for diagnostic in &artifacts.diagnostics {
                    match renderer.render(diagnostic) {
                        Ok(rendered) if rendered.contains(text) => {
                            found = true;
                            break;
                        }
                        Ok(_) => {}
                        Err(error) => {
                            failures.push(format!(
                                "failed to render diagnostic `{}`: {error}",
                                diagnostic.code
                            ));
                        }
                    }
                }
                if !found {
                    failures.push(format!("expected diagnostic containing `{text}`"));
                }
            }
            Expectation::DiagnosticCode(code) => {
                if !artifacts
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == *code)
                {
                    failures.push(format!("expected diagnostic code `{code}`"));
                }
            }
            Expectation::DumpContains { stage, text } => {
                let dump = artifacts.dump_stage(*stage);
                if !dump.contains(text) {
                    failures.push(format!("expected {stage:?} dump containing `{text}`"));
                }
            }
            Expectation::DumpLacks { stage, text } => {
                let dump = artifacts.dump_stage(*stage);
                if dump.contains(text) {
                    failures.push(format!("expected {stage:?} dump without `{text}`"));
                }
            }
        }
    }

    if mode == RuntimeMode::RunMain {
        failures.extend(differential_failure(case, &artifacts));
    }
    failures
}

/// Runs a case under every codegen policy and reports when a run's result
/// differs from the one under the case's own policy: acting on typing facts
/// must never change what a program computes.
fn differential_failure(
    case: &Case,
    artifacts: &interloko_compiler::session::SessionArtifacts,
) -> Option<String> {
    let expected = run_label(artifacts);
    CodegenPolicy::ALL
        .iter()
        .copied()
        .filter(|policy| *policy != case.codegen_policy)
        .find_map(|policy| {
            let other = compile_case_under(case, RuntimeMode::RunMain, policy);
            let actual = run_label(&other);
            (actual != expected).then(|| {
                format!(
                    "codegen {} ran to {expected}, but codegen {} ran to {actual}",
                    case.codegen_policy.as_str(),
                    policy.as_str()
                )
            })
        })
}

/// A run's outcome as compared across codegen policies: its value, or how
/// it failed. Where a trap happened (function, pc, span) is left out, since
/// different code may trap at a different place for the same reason, such
/// as an overflow in a numeric method that one policy runs inline.
fn run_label(artifacts: &interloko_compiler::session::SessionArtifacts) -> String {
    match &artifacts.runtime_result {
        None => did_not_run(artifacts),
        Some(result @ (RuntimeResult::Trap { .. } | RuntimeResult::Failed { .. })) => result
            .code()
            .map_or_else(|| result.to_string(), |code| format!("`{code}`")),
        Some(result) => result.to_string(),
    }
}

/// Explains a missing runtime result by the error codes that stopped
/// compilation.
fn did_not_run(artifacts: &interloko_compiler::session::SessionArtifacts) -> String {
    let codes = artifacts
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.is_error())
        .map(|diagnostic| diagnostic.code.to_string())
        .collect::<Vec<_>>();
    if codes.is_empty() {
        "main did not run".into()
    } else {
        format!(
            "main did not run: compilation failed with {}",
            codes.join(", ")
        )
    }
}

fn compile_case(case: &Case, mode: RuntimeMode) -> interloko_compiler::session::SessionArtifacts {
    compile_case_under(case, mode, case.codegen_policy)
}

fn compile_case_under(
    case: &Case,
    mode: RuntimeMode,
    codegen_policy: CodegenPolicy,
) -> interloko_compiler::session::SessionArtifacts {
    let grammar_registry = GrammarFrontendRegistry::builtin();
    if has_host_registry(&case.host_registry) {
        let mut session = CompilerSession::with_grammar_registry(grammar_registry);
        session.set_typing_config(case.typing_config);
        session.set_codegen_policy(codegen_policy);
        session.set_run_limits(case.run_limits);
        let source_file = session.add_source_with_grammar(
            &case.display_path,
            &case.source,
            case.grammar_environment.clone(),
        );
        session
            .compile_with_host_registry(source_file, mode, &case.host_registry)
            .expect("source was just added")
    } else {
        let mut session = IncrementalCompilerSession::with_grammar_registry(grammar_registry);
        session.set_typing_config(case.typing_config);
        session.set_codegen_policy(codegen_policy);
        session.set_run_limits(case.run_limits);
        let source = session.add_source_with_grammar(
            &case.display_path,
            &case.source,
            case.grammar_environment.clone(),
        );
        session
            .compile(source, mode)
            .expect("source was just added to incremental session")
            .artifacts
    }
}

fn has_host_registry(registry: &HostMetaRegistry) -> bool {
    !registry.classes().is_empty() || !registry.functions().is_empty()
}

fn parse_stage(value: &str) -> Option<DumpStage> {
    match value {
        "syntax" => Some(DumpStage::Syntax),
        "ast" => Some(DumpStage::Ast),
        "sem" => Some(DumpStage::Sem),
        "mop" => Some(DumpStage::Mop),
        "eop" => Some(DumpStage::Eop),
        "lowered" => Some(DumpStage::Lowered),
        "tape" => Some(DumpStage::Tape),
        "typing" => Some(DumpStage::Typing),
        "types" => Some(DumpStage::Types),
        "all" => Some(DumpStage::All),
        _ => None,
    }
}

fn invalid_header(path: &Path, line: usize, message: &str) -> CorpusError {
    CorpusError::InvalidHeader {
        path: path.display().to_string(),
        line,
        message: message.into(),
    }
}

fn read_to_string(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|error| CorpusError::Io {
        path: path.display().to_string(),
        error,
    })
}

fn display_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn default_corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("corpus")
}

fn usage() -> String {
    let binary = env::args()
        .next()
        .and_then(|path| {
            Path::new(&path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "interloko_corpus".into());
    format!(
        "usage:\n  {binary} check [corpus-root]\n  {binary} list [corpus-root]\n  {binary} heap [root...]\n  {binary} strictness [corpus-root]\n  {binary} dump <file.loko> --stage syntax|ast|sem|mop|eop|lowered|tape|typing|types|all"
    )
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Outcome {
    Success,
    CheckFailed,
}

#[derive(Clone, Debug, PartialEq)]
enum Expectation {
    Run(FunctionValue),
    /// Compilation errors keep `main` from running.
    RunBlocked,
    RuntimeCode(DiagnosticCode),
    RunReports(String),
    Diagnostics(usize),
    DiagnosticContains(String),
    DiagnosticCode(DiagnosticCode),
    DumpContains {
        stage: DumpStage,
        text: String,
    },
    DumpLacks {
        stage: DumpStage,
        text: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
struct Case {
    display_path: String,
    source: String,
    expectations: Vec<Expectation>,
    host_registry: HostMetaRegistry,
    grammar_environment: GrammarEnvironmentSelection,
    typing_config: TypingConfig,
    codegen_policy: CodegenPolicy,
    run_limits: RunLimits,
}

#[derive(Debug)]
enum CorpusError {
    Usage(String),
    Io {
        path: String,
        error: std::io::Error,
    },
    InvalidHeader {
        path: String,
        line: usize,
        message: String,
    },
}

impl CorpusError {
    const fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) | Self::InvalidHeader { .. } => 2,
            Self::Io { .. } => 1,
        }
    }
}

impl fmt::Display for CorpusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => write!(f, "{message}"),
            Self::Io { path, error } => write!(f, "failed to read {path}: {error}"),
            Self::InvalidHeader {
                path,
                line,
                message,
            } => {
                write!(f, "{path}:{line}: invalid corpus expectation: {message}")
            }
        }
    }
}

impl Error for CorpusError {}
