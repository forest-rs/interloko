//! Acceptance target for the break loop: the lab scenario of
//! `interloko_repl`'s `tests/fixtures/break_loop/break_loop.transcript`,
//! where a scripted chooser answers the signal through the structured
//! `choose-restart` affordance with no REPL involved. The fixture's header
//! explains what each step proves and which features it waits on.

#[allow(
    dead_code,
    reason = "each acceptance test uses part of the shared reader"
)]
#[path = "../../interloko_repl/tests/support/transcript.rs"]
mod transcript;

use std::path::Path;
use std::process::Command;

use transcript::ScenarioKind;

#[test]
#[ignore = "acceptance target: the lab chooser (`run --chooser`)"]
fn break_loop_transcript_lab_scenario() {
    // Transcript paths are relative to the `interloko_repl` package root.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../interloko_repl");
    let text =
        std::fs::read_to_string(root.join("tests/fixtures/break_loop/break_loop.transcript"))
            .expect("read break-loop transcript");
    let scenarios = transcript::parse(&text);
    let lab_scenarios: Vec<_> = scenarios
        .iter()
        .filter(|scenario| scenario.kind == ScenarioKind::Lab)
        .collect();
    assert!(!lab_scenarios.is_empty(), "no lab scenario in transcript");

    for scenario in lab_scenarios {
        for step in &scenario.steps {
            let output = Command::new(env!("CARGO_BIN_EXE_interloko_lab"))
                .current_dir(&root)
                .args(&step.args)
                .output()
                .expect("run interloko_lab");
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
