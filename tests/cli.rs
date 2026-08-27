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
    assert!(stdout.contains("--require"));
    assert!(stdout.contains("--exclude"));
    assert!(stdout.contains("named, anonymous, extra, error, missing"));
    assert!(stdout.contains("--compare"));
}
