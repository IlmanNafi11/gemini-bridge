use std::path::Path;
use std::process::{Command, Output};

fn run_in(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gemini-bridge"))
        .current_dir(root)
        .args(args)
        .env("HOME", root.join("home"))
        .env("XDG_DATA_HOME", root.join("xdg"))
        .env_remove("BRIDGE_SECRET")
        .env_remove("RUST_LOG")
        .output()
        .expect("run gemini-bridge")
}

fn combined_output(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn temporary_root() -> tempfile::TempDir {
    tempfile::tempdir().expect("create isolated CLI working directory")
}

#[test]
fn explicitly_missing_config_path_fails_instead_of_using_defaults() {
    let root = temporary_root();
    let missing = root.path().join("missing.toml");

    let output = run_in(
        root.path(),
        &["--config", missing.to_str().unwrap(), "doctor"],
    );

    assert!(!output.status.success());
    assert!(combined_output(&output).contains("Configuration file not found"));
}

#[test]
fn explicitly_unreadable_config_path_fails_instead_of_using_defaults() {
    let root = temporary_root();
    let directory_path = root.path().join("config-directory");
    std::fs::create_dir(&directory_path).expect("create directory as unreadable TOML path");

    let output = run_in(
        root.path(),
        &["--config", directory_path.to_str().unwrap(), "doctor"],
    );

    assert!(!output.status.success());
    assert!(combined_output(&output).contains("could not read"));
}

#[test]
fn absent_default_config_uses_built_in_defaults() {
    let root = temporary_root();

    let output = run_in(root.path(), &["doctor"]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Session status: Unconfigured"));
}
