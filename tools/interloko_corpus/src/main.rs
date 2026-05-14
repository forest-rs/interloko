//! Small-program corpus runner for Interloko compiler sessions.

use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use interloko_compiler::diagnostic::DiagnosticCode;
use interloko_compiler::grammar_frontend::GrammarEnvironmentSelection;
use interloko_compiler::sem::{HostClass, HostMetaRegistry, HostSlot, ValueKind};
use interloko_compiler::session::{
    CompilerSession, DumpStage, IncrementalCompilerSession, RuntimeMode, RuntimeResult,
};
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
            "usage: interloko_corpus dump <file.loko> --stage syntax|ast|sem|mop|eop|lowered|tape|all"
                .into(),
        ));
    }
    let stage = parse_stage(&args[3]).ok_or_else(|| {
        CorpusError::Usage("stage must be one of syntax|ast|sem|mop|eop|lowered|tape|all".into())
    })?;
    let source = read_to_string(Path::new(&args[1]))?;
    let registry = parse_host_registry(Path::new(&args[1]), &source)?;
    let grammar_environment = parse_grammar_environment(Path::new(&args[1]), &source)?;
    let artifacts = CompilerSession::compile_source_with_grammar_and_host_registry(
        &args[1],
        &source,
        RuntimeMode::CompileOnly,
        grammar_environment,
        &registry,
    );
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
        let display_path = display_path(root, &path);
        cases.push(Case {
            display_path,
            source,
            expectations,
            host_registry,
            grammar_environment,
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
    let mut grammar_environment = GrammarEnvironmentSelection::BaseLoko;
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            if let Some(rest) = trimmed.strip_prefix("// expect:") {
                expectations.push(parse_expectation(path, index + 1, rest.trim())?);
            } else if let Some(rest) = trimmed.strip_prefix("// host-class:") {
                host_registry.add_class(parse_host_class(path, index + 1, rest.trim())?);
            } else if let Some(rest) = trimmed.strip_prefix("// grammar:") {
                grammar_environment = parse_grammar_selection(path, index + 1, rest.trim())?;
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

fn parse_grammar_environment(path: &Path, source: &str) -> Result<GrammarEnvironmentSelection> {
    let mut selection = GrammarEnvironmentSelection::BaseLoko;
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            if let Some(rest) = trimmed.strip_prefix("// grammar:") {
                selection = parse_grammar_selection(path, index + 1, rest.trim())?;
            }
            continue;
        }
        break;
    }
    Ok(selection)
}

fn parse_grammar_selection(
    path: &Path,
    line: usize,
    input: &str,
) -> Result<GrammarEnvironmentSelection> {
    match input {
        "base-loko" => Ok(GrammarEnvironmentSelection::BaseLoko),
        "derived-host" => Ok(GrammarEnvironmentSelection::DerivedHost),
        _ => Err(invalid_header(
            path,
            line,
            "grammar must be `base-loko` or `derived-host`",
        )),
    }
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

fn parse_expectation(path: &Path, line: usize, input: &str) -> Result<Expectation> {
    if let Some(value) = input.strip_prefix("run ") {
        let value = value
            .parse()
            .map_err(|_| invalid_header(path, line, "run expectation must be an integer"))?;
        return Ok(Expectation::Run(value));
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
                "dump stage must be syntax|ast|sem|mop|eop|lowered|tape|all",
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
    let mode = if case
        .expectations
        .iter()
        .any(|expectation| matches!(expectation, Expectation::Run(_)))
    {
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
                Some(RuntimeResult::ReturnedI64(actual)) if actual == expected => {}
                Some(actual) => {
                    failures.push(format!("expected run result {expected}, got {actual:?}"))
                }
                None => failures.push(format!(
                    "expected run result {expected}, but main did not run"
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
    if has_host_registry(&case.host_registry) {
        CompilerSession::compile_source_with_grammar_and_host_registry(
            &case.display_path,
            &case.source,
            mode,
            case.grammar_environment,
            &case.host_registry,
        )
    } else {
        let mut session = IncrementalCompilerSession::new();
        let source = session.add_source_with_grammar(
            &case.display_path,
            &case.source,
            case.grammar_environment,
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
        "usage:\n  {binary} check [corpus-root]\n  {binary} list [corpus-root]\n  {binary} dump <file.loko> --stage syntax|ast|sem|mop|eop|lowered|tape|all"
    )
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Outcome {
    Success,
    CheckFailed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Expectation {
    Run(i64),
    Diagnostics(usize),
    DiagnosticContains(String),
    DiagnosticCode(DiagnosticCode),
    DumpContains { stage: DumpStage, text: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Case {
    display_path: String,
    source: String,
    expectations: Vec<Expectation>,
    host_registry: HostMetaRegistry,
    grammar_environment: GrammarEnvironmentSelection,
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
