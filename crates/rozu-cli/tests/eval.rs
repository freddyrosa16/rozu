mod common;

use std::path::PathBuf;

use common::{json_lines, rozu, stdout_of};
use predicates::str::contains;

fn evals(sub: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../evals")
        .join(sub)
}

#[test]
fn scripted_tasks_pass() {
    let out = stdout_of(rozu().args(["eval", "tasks", "--dir"]).arg(evals("tasks")));
    assert!(out.contains("6 of 6 tasks passed"), "{out}");
}

#[test]
fn a_recorded_session_replays_and_drift_is_caught() {
    let mut replay = rozu();
    replay
        .args(["eval", "replay"])
        .arg(evals("recorded/fix-failing-test.jsonl"))
        .arg("--workspace");
    let out = stdout_of(replay.arg(evals("recorded/fix-failing-test")));
    assert!(out.contains("every result matches"), "{out}");

    let drifted = tempfile::tempdir().unwrap();
    for file in ["math.sh", "test.sh"] {
        let text = std::fs::read_to_string(evals("recorded/fix-failing-test").join(file)).unwrap();
        std::fs::write(
            drifted.path().join(file),
            text.replace("$1 - $2", "$1 + $2"),
        )
        .unwrap();
    }
    rozu()
        .args(["eval", "replay"])
        .arg(evals("recorded/fix-failing-test.jsonl"))
        .arg("--workspace")
        .arg(drifted.path())
        .assert()
        .failure()
        .stdout(contains("tool result 1 differs"));
}

#[test]
fn memory_eval_logs_a_row_per_run_and_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("rows.jsonl");
    let run = |extra: &[&str]| {
        let mut cmd = rozu();
        cmd.args(["eval", "memory", "--dir"])
            .arg(evals("memory"))
            .arg("--out")
            .arg(&out)
            .args(extra);
        cmd
    };
    let summary = stdout_of(&mut run(&["--seeds", "2"]));
    assert_eq!(summary.lines().count(), 4, "{summary}");
    let rows = json_lines(&std::fs::read_to_string(&out).unwrap());
    assert_eq!(rows.len(), 2 * 4 * 2);
    for row in &rows {
        assert_eq!(row["status"], "ok", "{row}");
        assert!(row["cost_usd"].is_null(), "{row}");
        assert!(row["input_tokens"].as_u64().unwrap() > 0, "{row}");
        assert!(row["latency_ms"].is_u64(), "{row}");
        if row["condition"] == "off" {
            assert_eq!(row["recall"], 1.0, "{row}");
            assert_eq!(row["compactions"], 0, "{row}");
        } else {
            assert!(row["compactions"].as_u64().unwrap() > 0, "{row}");
        }
    }
    // The pinned rule is recalled under every condition.
    let pinned: Vec<_> = rows
        .iter()
        .filter(|r| r["scenario"] == "facts-in-chat-and-files")
        .collect();
    assert!(
        pinned
            .iter()
            .all(|r| r["recall"].as_f64().unwrap() >= 2.0 / 3.0)
    );

    run(&[]).assert().failure().stderr(contains("--resume"));
    run(&["--seeds", "3", "--resume", "--condition", "off"])
        .assert()
        .success();
    let rows = json_lines(&std::fs::read_to_string(&out).unwrap());
    assert_eq!(rows.len(), 16 + 2);
}
