//! Acceptance target for the break loop: the REPL scenario of
//! `tests/fixtures/break_loop/break_loop.transcript`. The fixture's header
//! explains what each step proves and which features it waits on. The lab
//! scenario of the same transcript runs from `interloko_lab`'s tests.

#[allow(
    dead_code,
    reason = "each acceptance test uses part of the shared reader"
)]
#[path = "support/transcript.rs"]
mod transcript;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use transcript::ScenarioKind;

#[test]
#[ignore = "acceptance target: break loop (conditions + code units)"]
fn break_loop_transcript_repl_scenario() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let text =
        std::fs::read_to_string(root.join("tests/fixtures/break_loop/break_loop.transcript"))
            .expect("read break-loop transcript");
    let scenarios = transcript::parse(&text);
    let repl_scenarios: Vec<_> = scenarios
        .iter()
        .filter(|scenario| scenario.kind == ScenarioKind::Repl)
        .collect();
    assert!(!repl_scenarios.is_empty(), "no repl scenario in transcript");

    for scenario in repl_scenarios {
        for step in &scenario.steps {
            let mut child = Command::new(env!("CARGO_BIN_EXE_interloko_repl"))
                .current_dir(root)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn interloko_repl");
            let mut input = step.input.join("\n");
            input.push('\n');
            child
                .stdin
                .as_mut()
                .expect("stdin")
                .write_all(input.as_bytes())
                .expect("write stdin");
            let output = child.wait_with_output().expect("wait for interloko_repl");
            assert!(
                output.status.success(),
                "scenario `{}` exited unsuccessfully\nstderr:\n{}",
                scenario.title,
                String::from_utf8_lossy(&output.stderr)
            );

            let stdout = String::from_utf8(output.stdout).expect("stdout utf8");
            let stderr = String::from_utf8(output.stderr).expect("stderr utf8");
            transcript::assert_output(&scenario.title, "stdout", &step.stdout, &stdout);
            transcript::assert_output(&scenario.title, "stderr", &step.stderr, &stderr);
        }
    }
}
