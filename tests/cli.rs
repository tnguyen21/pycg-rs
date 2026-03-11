use std::path::{Path, PathBuf};

use assert_cmd::Command;
use jsonschema::validator_for;
use serde_json::Value;
use tempfile::tempdir;

fn fixture_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn normalize_json_paths(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for key in ["file", "path", "root"] {
                if let Some(file_value) = map.get_mut(key)
                    && let Some(file) = file_value.as_str()
                {
                    let normalized = normalize_path_string(file);
                    *file_value = Value::String(normalized);
                }
            }
            if let Some(Value::Array(inputs)) = map.get_mut("inputs") {
                for input in inputs {
                    if let Some(value) = input.as_str() {
                        *input = Value::String(normalize_path_string(value));
                    }
                }
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

fn normalize_path_string(file: &str) -> String {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let file_path = Path::new(file);
    if let Ok(relative) = file_path.strip_prefix(&manifest_dir) {
        if relative.as_os_str().is_empty() {
            ".".to_string()
        } else {
            relative.to_string_lossy().replace('\\', "/")
        }
    } else if let Some(stripped) = file.strip_prefix(&format!("{}/", manifest_dir.display())) {
        stripped.replace('\\', "/")
    } else if file == manifest_dir.to_string_lossy() {
        ".".to_string()
    } else {
        file.replace('\\', "/")
    }
}

fn load_json_schema() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("json-schema")
        .join("pycg-graph-v1.schema.json");
    serde_json::from_str(
        &std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display())),
    )
    .unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()))
}

fn validate_json_contract(value: &Value) {
    let schema = load_json_schema();
    let validator = validator_for(&schema).expect("schema should compile");
    validator
        .validate(value)
        .expect("CLI JSON output should match the v1 schema");
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
    assert!(
        stdout.contains("[U]"),
        "default output should include uses edges"
    );
    assert!(
        !stdout.contains("[D]"),
        "default output should not include defines edges"
    );
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
    assert!(
        stdout.contains("[U]"),
        "combined output should include uses edges"
    );
    assert!(
        stdout.contains("[D]"),
        "combined output should include defines edges"
    );
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
    validate_json_contract(&json);
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
    validate_json_contract(&json);
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

#[test]
fn cli_json_reports_external_references() {
    let fixture = fixture_path("tests/test_code/regression_issue5.py");
    let output = run_pycg(&[
        fixture.to_str().unwrap(),
        "--format",
        "json",
        "--root",
        "tests",
    ])
    .success()
    .get_output()
    .stdout
    .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid json output");
    let external_refs = json["diagnostics"]["external_references"]
        .as_array()
        .expect("external references should be an array");
    assert!(
        external_refs
            .iter()
            .filter_map(|entry| entry["canonical_name"].as_str())
            .any(|name| name == "numpy" || name == "os.path" || name == "pandas.io.parsers"),
        "expected diagnostics to include unresolved external imports, got: {external_refs:?}"
    );
    let numpy = external_refs
        .iter()
        .find(|entry| entry["canonical_name"].as_str() == Some("numpy"))
        .expect("expected numpy external reference");
    assert_eq!(numpy["kind"].as_str(), Some("module"));
    assert_eq!(
        numpy["path"].as_str(),
        Some("test_code/regression_issue5.py")
    );
    assert_eq!(numpy["line"].as_u64(), Some(1));
}

#[test]
fn cli_json_reports_external_references_in_module_mode() {
    let fixture = fixture_path("tests/test_code/regression_issue5.py");
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

    let json: Value = serde_json::from_slice(&output).expect("valid json output");
    let node_ids: std::collections::HashSet<&str> = json["nodes"]
        .as_array()
        .expect("nodes should be an array")
        .iter()
        .filter_map(|node| node["id"].as_str())
        .collect();
    let external_refs = json["diagnostics"]["external_references"]
        .as_array()
        .expect("external references should be an array");
    assert!(
        !external_refs.is_empty(),
        "expected module-mode external reference diagnostics",
    );
    assert!(
        external_refs.iter().all(|entry| {
            entry["source"]
                .as_str()
                .is_some_and(|source| node_ids.contains(source))
        }),
        "expected module-mode external refs to point at emitted module nodes, got: {external_refs:?}"
    );
}

#[test]
fn cli_json_reports_unresolved_references() {
    let src = fixture_path("tests/test_code/star_private_src.py");
    let user = fixture_path("tests/test_code/star_private_user.py");
    let output = run_pycg(&[
        src.to_str().unwrap(),
        user.to_str().unwrap(),
        "--format",
        "json",
        "--root",
        "tests",
    ])
    .success()
    .get_output()
    .stdout
    .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid json output");
    let unresolved = json["diagnostics"]["unresolved_references"]
        .as_array()
        .expect("unresolved references should be an array");
    assert!(
        unresolved
            .iter()
            .filter_map(|entry| entry["symbol"].as_str())
            .any(|symbol| symbol == "_private_impl"),
        "expected unresolved private star import reference, got: {unresolved:?}"
    );
    let private_impl = unresolved
        .iter()
        .find(|entry| entry["symbol"].as_str() == Some("_private_impl"))
        .expect("expected unresolved _private_impl entry");
    assert_eq!(
        private_impl["path"].as_str(),
        Some("test_code/star_private_user.py")
    );
    assert_eq!(private_impl["line"].as_u64(), Some(11));
}

#[test]
fn cli_json_suppresses_external_and_synthetic_unresolved_noise() {
    let fixture = fixture_path("tests/test_code/regression_issue5.py");
    let output = run_pycg(&[
        fixture.to_str().unwrap(),
        "--format",
        "json",
        "--root",
        "tests",
    ])
    .success()
    .get_output()
    .stdout
    .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid json output");
    let unresolved = json["diagnostics"]["unresolved_references"]
        .as_array()
        .expect("unresolved references should be an array");
    let unresolved_symbols: std::collections::HashSet<&str> = unresolved
        .iter()
        .filter_map(|entry| entry["symbol"].as_str())
        .collect();
    for suppressed in ["numpy", "os.path", "pandas.io.parsers", "^^^argument^^^"] {
        assert!(
            !unresolved_symbols.contains(suppressed),
            "expected unresolved diagnostics to suppress {suppressed}, got: {unresolved:?}"
        );
    }
}

#[test]
fn cli_json_reports_ambiguous_resolutions_and_approximations() {
    let fixture = fixture_path("tests/test_code/accuracy_multi_return.py");
    let output = run_pycg(&[
        fixture.to_str().unwrap(),
        "--format",
        "json",
        "--root",
        "tests",
    ])
    .success()
    .get_output()
    .stdout
    .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid json output");
    let ambiguous = json["diagnostics"]["ambiguous_resolutions"]
        .as_array()
        .expect("ambiguous resolutions should be an array");
    assert!(
        ambiguous.iter().any(|entry| {
            entry["symbol"].as_str() == Some("method")
                && entry["candidate_targets"]
                    .as_array()
                    .is_some_and(|targets| targets.len() == 2)
        }),
        "expected multi-return method ambiguity diagnostics, got: {ambiguous:?}"
    );
    let method_resolution = ambiguous
        .iter()
        .find(|entry| entry["source"].as_str() == Some("n5"))
        .expect("expected ambiguity entry for caller");
    assert_eq!(
        method_resolution["path"].as_str(),
        Some("test_code/accuracy_multi_return.py")
    );
    assert_eq!(method_resolution["line"].as_u64(), Some(33));

    let approximations = json["diagnostics"]["approximations"]
        .as_array()
        .expect("approximations should be an array");
    assert!(
        approximations.iter().any(|entry| {
            entry["reason"].as_str() == Some("multiple_candidate_targets")
                && entry["symbol"].as_str() == Some("method")
                && entry["candidate_targets"]
                    .as_array()
                    .is_some_and(|targets| targets.len() == 2)
        }),
        "expected approximation entry for widened method resolution, got: {approximations:?}"
    );
    let approximation = approximations
        .iter()
        .find(|entry| entry["source"].as_str() == Some("n5"))
        .expect("expected approximation entry for caller");
    assert_eq!(
        approximation["path"].as_str(),
        Some("test_code/accuracy_multi_return.py")
    );
    assert_eq!(approximation["line"].as_u64(), Some(33));
}

#[test]
fn cli_json_reports_three_way_ambiguity() {
    let fixture = fixture_path("tests/test_code/diagnostics_multi_return_three.py");
    let output = run_pycg(&[
        fixture.to_str().unwrap(),
        "--format",
        "json",
        "--root",
        "tests",
    ])
    .success()
    .get_output()
    .stdout
    .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid json output");
    let ambiguous = json["diagnostics"]["ambiguous_resolutions"]
        .as_array()
        .expect("ambiguous resolutions should be an array");
    let three_way = ambiguous
        .iter()
        .find(|entry| entry["symbol"].as_str() == Some("method"))
        .expect("expected a three-way method ambiguity");
    assert_eq!(
        three_way["candidate_targets"].as_array().map(Vec::len),
        Some(3)
    );
    assert_eq!(
        three_way["path"].as_str(),
        Some("test_code/diagnostics_multi_return_three.py")
    );
    assert_eq!(three_way["line"].as_u64(), Some(24));

    let approximations = json["diagnostics"]["approximations"]
        .as_array()
        .expect("approximations should be an array");
    let three_way_approx = approximations
        .iter()
        .find(|entry| entry["symbol"].as_str() == Some("method"))
        .expect("expected a matching approximation entry");
    assert_eq!(
        three_way_approx["candidate_targets"]
            .as_array()
            .map(Vec::len),
        Some(3)
    );
    assert_eq!(
        three_way_approx["path"].as_str(),
        Some("test_code/diagnostics_multi_return_three.py")
    );
    assert_eq!(three_way_approx["line"].as_u64(), Some(24));
}
