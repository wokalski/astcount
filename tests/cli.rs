use std::process::Command;

#[test]
fn help_describes_the_metric() {
    let output = Command::new(env!("CARGO_BIN_EXE_astcount"))
        .arg("--help")
        .output()
        .expect("run astcount");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8 help");
    assert!(stdout.contains("Tree-sitter node counter"));
    assert!(stdout.contains("count"));
    assert!(stdout.contains("compare"));

    let output = Command::new(env!("CARGO_BIN_EXE_astcount"))
        .args(["count", "--help"])
        .output()
        .expect("run astcount count --help");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8 help");
    assert!(stdout.contains("--exclude"));
    assert!(stdout.contains("--stats"));
    assert!(stdout.contains("named, anonymous, extra, error, missing"));
    assert!(!stdout.contains("--require"));
}

#[test]
fn implicit_count_matches_explicit_count() {
    let binary = env!("CARGO_BIN_EXE_astcount");
    let report = std::env::temp_dir().join(format!("astcount-cli-{}.json", std::process::id()));
    let missing = "this-path-must-not-exist.astcount-test";
    let implicit = Command::new(binary)
        .arg(missing)
        .output()
        .expect("run implicit count");
    let explicit = Command::new(binary)
        .args(["count", missing])
        .output()
        .expect("run explicit count");
    assert!(!implicit.status.success());
    assert_eq!(implicit.status, explicit.status);
    assert_eq!(implicit.stdout, explicit.stdout);
    assert_eq!(implicit.stderr, explicit.stderr);

    std::fs::write(
        &report,
        r#"{"schema":3,"tool_version":"0.2.0","parser_backend":"test","filter":{"excluded":["anonymous"]},"totals":{"files":0,"bytes":0,"nodes":{"selected":1,"total":1,"by_property":{"named":1,"extra":0,"error":0,"missing":0}}},"files":[]}"#,
    )
    .expect("write report");
    let output = Command::new(binary)
        .arg("compare")
        .args([&report, &report])
        .arg("--json")
        .output()
        .expect("compare reports");
    assert!(output.status.success());
    let comparison: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse comparison JSON");
    assert_eq!(comparison["schema"], 1);
    assert_eq!(comparison["delta_nodes"], 0);
    assert_eq!(comparison["percent_change"], 0.0);
    std::fs::remove_file(report).expect("remove temporary report");
}

#[test]
fn rejects_contradictory_exclusions_and_stats_with_json() {
    let binary = env!("CARGO_BIN_EXE_astcount");
    let output = Command::new(binary)
        .args([
            "count",
            "this-path-is-never-read",
            "--exclude",
            "named,anonymous",
        ])
        .output()
        .expect("run contradictory filter");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stderr)
            .expect("utf8 stderr")
            .contains("cannot exclude both named and anonymous nodes")
    );

    let output = Command::new(binary)
        .args(["count", ".", "--json", "--stats"])
        .output()
        .expect("run conflicting output flags");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stderr)
            .expect("utf8 stderr")
            .contains("cannot be used with '--stats'")
    );
}
