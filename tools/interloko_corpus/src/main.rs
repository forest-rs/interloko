//! Small-program corpus runner for Interloko compiler sessions.

use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use interloko_compiler::diagnostic::DiagnosticCode;
use interloko_compiler::grammar_frontend::{GrammarEnvironmentSelection, GrammarFrontendRegistry};
use interloko_compiler::sem::{HostClass, HostMetaRegistry, HostSlot, ValueKind};
use interloko_compiler::session::{
    CompilerSession, DumpStage, FunctionValue, IncrementalCompilerSession, RuntimeMode, TurnStats,
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
            "usage: interloko_corpus dump <file.loko> --stage syntax|ast|sem|mop|eop|lowered|tape|typing|all"
                .into(),
        ));
    }
    let stage = parse_stage(&args[3]).ok_or_else(|| {
        CorpusError::Usage(
            "stage must be one of syntax|ast|sem|mop|eop|lowered|tape|typing|all".into(),
        )
    })?;
    let source = read_to_string(Path::new(&args[1]))?;
    let grammar_registry = GrammarFrontendRegistry::builtin();
    let host_registry = parse_host_registry(Path::new(&args[1]), &source)?;
    let grammar_environment =
        parse_grammar_environment(Path::new(&args[1]), &source, &grammar_registry)?;
    let mut session = CompilerSession::with_grammar_registry(grammar_registry);
    session.set_typing_config(parse_typing_config(Path::new(&args[1]), &source)?);
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
        let display_path = display_path(root, &path);
        cases.push(Case {
            display_path,
            source,
            expectations,
            host_registry,
            grammar_environment,
            typing_config,
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
                config = TypingConfig::preset(rest.trim()).ok_or_else(|| {
                    invalid_header(
                        path,
                        index + 1,
                        &format!(
                            "typing must be one of {}",
                            TypingConfig::PRESETS
                                .iter()
                                .map(|name| format!("`{name}`"))
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
    Ok(config)
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
/// `"42"` a string. Floats also accept `inf`, `-inf`, and `nan`.
fn parse_run_value(text: &str) -> Option<FunctionValue> {
    match text {
        "#t" => return Some(FunctionValue::Boolean(true)),
        "#f" => return Some(FunctionValue::Boolean(false)),
        _ => {}
    }
    if let Some(body) = text
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
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
        return Some(FunctionValue::String(decoded));
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
        let Some((stage, text)) = rest.split_once(" contains ") else {
            return Err(invalid_header(
                path,
                line,
                "dump expectation must be `dump STAGE contains TEXT`",
            ));
        };
        let stage = parse_stage(stage).ok_or_else(|| {
            invalid_header(
                path,
                line,
                "dump stage must be syntax|ast|sem|mop|eop|lowered|tape|typing|all",
            )
        })?;
        return Ok(Expectation::DumpContains {
            stage,
            text: text.into(),
        });
    }

    Err(invalid_header(path, line, "unknown expectation"))
}

fn check_case(case: &Case) -> Vec<String> {
    let mode = if case.expectations.iter().any(|expectation| {
        matches!(
            expectation,
            Expectation::Run(_) | Expectation::RuntimeCode(_) | Expectation::RunReports(_)
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
                    "expected run result {expected}, but main did not run"
                )),
            },
            Expectation::RuntimeCode(expected) => match &artifacts.runtime_result {
                Some(actual) if actual.code() == Some(*expected) => {}
                Some(actual) => failures.push(format!(
                    "expected runtime outcome `{expected}`, got {actual}"
                )),
                None => failures.push(format!(
                    "expected runtime outcome `{expected}`, but main did not run"
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
                    "expected run result containing `{text}`, but main did not run"
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
        }
    }

    failures
}

fn compile_case(case: &Case, mode: RuntimeMode) -> interloko_compiler::session::SessionArtifacts {
    let grammar_registry = GrammarFrontendRegistry::builtin();
    if has_host_registry(&case.host_registry) {
        let mut session = CompilerSession::with_grammar_registry(grammar_registry);
        session.set_typing_config(case.typing_config);
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
        "usage:\n  {binary} check [corpus-root]\n  {binary} list [corpus-root]\n  {binary} heap [root...]\n  {binary} dump <file.loko> --stage syntax|ast|sem|mop|eop|lowered|tape|typing|all"
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
    RuntimeCode(DiagnosticCode),
    RunReports(String),
    Diagnostics(usize),
    DiagnosticContains(String),
    DiagnosticCode(DiagnosticCode),
    DumpContains { stage: DumpStage, text: String },
}

#[derive(Clone, Debug, PartialEq)]
struct Case {
    display_path: String,
    source: String,
    expectations: Vec<Expectation>,
    host_registry: HostMetaRegistry,
    grammar_environment: GrammarEnvironmentSelection,
    typing_config: TypingConfig,
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
