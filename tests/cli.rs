use std::process::Command;

#[test]
fn help_describes_the_metric() {
    let output = Command::new(env!("CARGO_BIN_EXE_deslop"))
        .arg("--help")
        .output()
        .expect("run deslop");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8 help");
    assert!(stdout.contains("AST node counter"));
    assert!(stdout.contains("--compare"));
}
