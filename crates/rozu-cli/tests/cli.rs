use assert_cmd::Command;
use predicates::str::contains;

fn rozu() -> Command {
    Command::cargo_bin("rozu").expect("rozu binary is built")
}

#[test]
fn prints_version() {
    rozu()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains(format!("rozu {}", env!("CARGO_PKG_VERSION"))));
}

#[test]
fn doctor_lists_every_crate() {
    let output = rozu().arg("doctor").assert().success().get_output().clone();
    let stdout = String::from_utf8(output.stdout).expect("utf-8 output");
    for name in [
        "rozu-cli",
        "rozu-core",
        "rozu-providers",
        "rozu-tools",
        "rozu-pty",
        "rozu-secrets",
        "rozu-store",
    ] {
        assert!(stdout.contains(name), "missing {name} in:\n{stdout}");
    }
}
