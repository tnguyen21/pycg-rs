use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::Value;
use tempfile::tempdir;

fn fixture_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn normalize_json_paths(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(file_value) = map.get_mut("file")
                && let Some(file) = file_value.as_str()
            {
                let basename = Path::new(file)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(file)
                    .to_string();
                *file_value = Value::String(basename);
            }
            for child in map.values_mut() {
                normalize_json_paths(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                normalize_json_paths(item);
            }
        }
        _ => {}
    }
}

fn run_pycg(args: &[&str]) -> assert_cmd::assert::Assert {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("pycg"));
    cmd.args(args);
    cmd.assert()
}

#[test]
fn cli_defaults_to_uses_edges_only() {
    let fixture = fixture_path("tests/test_code/accuracy_factory.py");
    let output = run_pycg(&[
        fixture.to_str().unwrap(),
        "--format",
        "text",
        "--root",
        env!("CARGO_MANIFEST_DIR"),
    ])
    .success()
    .get_output()
    .stdout
    .clone();

    let stdout = String::from_utf8(output).expect("stdout should be utf8");
    assert!(stdout.contains("[U]"), "default output should include uses edges");
    assert!(!stdout.contains("[D]"), "default output should not include defines edges");
}

#[test]
fn cli_can_render_defines_and_uses() {
    let fixture = fixture_path("tests/test_code/accuracy_factory.py");
    let output = run_pycg(&[
        fixture.to_str().unwrap(),
        "--format",
        "text",
        "--defines",
        "--uses",
        "--root",
        env!("CARGO_MANIFEST_DIR"),
    ])
    .success()
    .get_output()
    .stdout
    .clone();

    let stdout = String::from_utf8(output).expect("stdout should be utf8");
    assert!(stdout.contains("[U]"), "combined output should include uses edges");
    assert!(stdout.contains("[D]"), "combined output should include defines edges");
}

#[test]
fn cli_json_snapshot_symbol_graph() {
    let fixture = fixture_path("tests/test_code/accuracy_factory.py");
    let output = run_pycg(&[
        fixture.to_str().unwrap(),
        "--format",
        "json",
        "--root",
        env!("CARGO_MANIFEST_DIR"),
    ])
    .success()
    .get_output()
    .stdout
    .clone();

    let mut json: Value = serde_json::from_slice(&output).expect("valid json output");
    normalize_json_paths(&mut json);
    insta::assert_snapshot!(
        "cli_symbol_graph_json",
        serde_json::to_string_pretty(&json).expect("snapshot json should serialize")
    );
}

#[test]
fn cli_json_snapshot_module_graph() {
    let fixture = fixture_path("tests/test_code/import_coverage");
    let output = run_pycg(&[
        fixture.to_str().unwrap(),
        "--format",
        "json",
        "--modules",
        "--root",
        "tests",
    ])
    .success()
    .get_output()
    .stdout
    .clone();

    let mut json: Value = serde_json::from_slice(&output).expect("valid json output");
    normalize_json_paths(&mut json);
    insta::assert_snapshot!(
        "cli_module_graph_json",
        serde_json::to_string_pretty(&json).expect("snapshot json should serialize")
    );
}

#[test]
fn cli_errors_when_no_python_files_are_found() {
    let empty_dir = tempdir().expect("temp dir should be created");
    let output = run_pycg(&[empty_dir.path().to_str().unwrap()])
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8(output.stderr).expect("stderr should be utf8");
    assert!(
        stderr.contains("No Python files found"),
        "expected missing-file error, got: {stderr}"
    );
}
