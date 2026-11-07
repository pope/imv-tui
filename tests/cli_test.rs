//! Integration tests for imv-tui CLI arguments and options.

use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn test_cli_help_flag() {
    let output = Command::new(env!("CARGO_BIN_EXE_imv-tui"))
        .arg("--help")
        .output()
        .expect("Failed to execute process");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("imv-tui: A fast keyboard-driven terminal image viewer"));
    assert!(stdout.contains("Usage: imv-tui [path] [options]"));
    assert!(stdout.contains("Options:"));
    assert!(stdout.contains("-f, --filter"));
    assert!(stdout.contains("-v, --version"));

    // Also test short -h flag
    let output_short = Command::new(env!("CARGO_BIN_EXE_imv-tui"))
        .arg("-h")
        .output()
        .expect("Failed to execute process");
    assert!(output_short.status.success());
    let stdout_short = String::from_utf8_lossy(&output_short.stdout);
    assert!(stdout_short.contains("Usage: imv-tui [path] [options]"));
}

#[test]
fn test_cli_version_flag() {
    let output = Command::new(env!("CARGO_BIN_EXE_imv-tui"))
        .arg("--version")
        .output()
        .expect("Failed to execute process");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(concat!("imv-tui ", env!("CARGO_PKG_VERSION"))));

    // Also test short -v and -V flags
    let output_short = Command::new(env!("CARGO_BIN_EXE_imv-tui"))
        .arg("-v")
        .output()
        .expect("Failed to execute process");
    assert!(output_short.status.success());
    assert!(String::from_utf8_lossy(&output_short.stdout).contains("imv-tui"));

    let output_cap = Command::new(env!("CARGO_BIN_EXE_imv-tui"))
        .arg("-V")
        .output()
        .expect("Failed to execute process");
    assert!(output_cap.status.success());
    assert!(String::from_utf8_lossy(&output_cap.stdout).contains("imv-tui"));
}

#[test]
fn test_cli_invalid_flags() {
    // Unknown flag
    let output = Command::new(env!("CARGO_BIN_EXE_imv-tui"))
        .arg("--unknown-flag")
        .output()
        .expect("Failed to execute process");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Error: Unknown option '--unknown-flag'"));

    // Invalid filter
    let output = Command::new(env!("CARGO_BIN_EXE_imv-tui"))
        .args(["--filter", "nonexistent_filter"])
        .output()
        .expect("Failed to execute process");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Unknown filter 'nonexistent_filter'"));

    // Invalid scale
    let output = Command::new(env!("CARGO_BIN_EXE_imv-tui"))
        .args(["--scale", "invalid_scale"])
        .output()
        .expect("Failed to execute process");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Unknown scale mode 'invalid_scale'"));

    // Invalid slideshow (negative, zero, non-numeric)
    let output = Command::new(env!("CARGO_BIN_EXE_imv-tui"))
        .args(["--slideshow", "-5"])
        .output()
        .expect("Failed to execute process");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("requires a positive integer argument"));

    let output = Command::new(env!("CARGO_BIN_EXE_imv-tui"))
        .args(["--slideshow", "0"])
        .output()
        .expect("Failed to execute process");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("requires a positive integer argument"));

    let output = Command::new(env!("CARGO_BIN_EXE_imv-tui"))
        .args(["--slideshow", "abc"])
        .output()
        .expect("Failed to execute process");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("requires a positive integer argument"));
}

#[test]
fn test_cli_recursive_conflicts() {
    // --recursive with a regular file
    let output = Command::new(env!("CARGO_BIN_EXE_imv-tui"))
        .args(["Cargo.toml", "-R"])
        .output()
        .expect("Failed to execute process");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Option --recursive (-R) is not applicable when specifying a file path")
    );

    // --recursive with piped stdin input
    let mut child = Command::new(env!("CARGO_BIN_EXE_imv-tui"))
        .arg("-R")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn process");

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(b"Cargo.toml\n")
            .expect("Failed to write to stdin");
    }
    let output = child.wait_with_output().expect("Failed to wait on child");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Option --recursive (-R) cannot be used with piped stdin input"));
}

#[test]
fn test_cli_piped_stdin_with_positional_path() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_imv-tui"))
        .arg("Cargo.toml")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn process");

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(b"Cargo.toml\n")
            .expect("Failed to write to stdin");
    }
    let output = child.wait_with_output().expect("Failed to wait on child");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr
            .contains("Positional path argument cannot be specified when piping input from stdin")
    );
}
