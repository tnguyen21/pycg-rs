use std::path::{Path, PathBuf};

use assert_cmd::Command;
use jsonschema::validator_for;
use serde_json::Value;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn load_schema(file_name: &str) -> Value {
    let path = repo_root().join("docs").join("json-schema").join(file_name);
    serde_json::from_str(
        &std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display())),
    )
    .unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()))
}

fn validate_schema(file_name: &str, value: &Value) {
    let schema = load_schema(file_name);
    let validator = validator_for(&schema).expect("schema should compile");
    validator
        .validate(value)
        .unwrap_or_else(|e| panic!("{file_name} should accept output:\n{value:#}\nerror: {e}"));
}

fn run_pycg(args: &[&str]) -> assert_cmd::assert::Assert {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("pycg"));
    cmd.args(args);
    cmd.assert()
}

fn normalize_json_paths(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for key in ["file", "path", "root", "target"] {
                if let Some(file_value) = map.get_mut(key)
                    && let Some(file) = file_value.as_str()
                {
                    let normalized = normalize_path_string(file);
                    *file_value = Value::String(normalized);
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
    let manifest_dir = repo_root();
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

#[test]
fn analyze_subcommand_emits_graph_contract() {
    let output = run_pycg(&[
        "analyze",
        "tests/test_code/accuracy_factory.py",
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
    validate_schema("pycg-graph-v1.schema.json", &json);
}

#[test]
fn symbols_in_path_query_emits_schema_valid_results() {
    let output = run_pycg(&[
        "symbols-in",
        "tests/test_code/accuracy_factory.py",
        "tests/test_code/accuracy_factory.py",
        "--root",
        "tests",
        "--format",
        "json",
    ])
    .success()
    .get_output()
    .stdout
    .clone();

    let mut json: Value = serde_json::from_slice(&output).expect("valid json output");
    validate_schema("pycg-symbols-in-v1.schema.json", &json);
    normalize_json_paths(&mut json);
    let symbols = json["symbols"]
        .as_array()
        .expect("symbols should be an array");
    assert_eq!(symbols.len(), 5);
    assert!(
        symbols.iter().any(|symbol| {
            symbol["canonical_name"].as_str() == Some("test_code.accuracy_factory.consumer")
        }),
        "expected consumer symbol in result, got: {symbols:?}"
    );
}

#[test]
fn symbols_in_module_query_supports_module_mode() {
    let output = run_pycg(&[
        "symbols-in",
        "test_code.import_coverage",
        "tests/test_code/import_coverage",
        "--root",
        "tests",
        "--modules",
        "--target-kind",
        "module",
        "--format",
        "json",
    ])
    .success()
    .get_output()
    .stdout
    .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid json output");
    validate_schema("pycg-symbols-in-v1.schema.json", &json);
    let symbols = json["symbols"]
        .as_array()
        .expect("symbols should be an array");
    assert!(
        symbols.len() >= 5,
        "expected import_coverage module graph symbols, got: {symbols:?}"
    );
}

#[test]
fn summary_query_emits_counts_and_schema_valid_output() {
    let output = run_pycg(&[
        "summary",
        "tests/test_code/accuracy_factory.py",
        "tests/test_code/accuracy_factory.py",
        "--root",
        "tests",
        "--format",
        "json",
    ])
    .success()
    .get_output()
    .stdout
    .clone();

    let mut json: Value = serde_json::from_slice(&output).expect("valid json output");
    validate_schema("pycg-summary-v1.schema.json", &json);
    normalize_json_paths(&mut json);
    assert_eq!(json["summary"]["file_count"].as_u64(), Some(1));
    assert_eq!(json["summary"]["symbol_counts"]["class"].as_u64(), Some(1));
    assert_eq!(
        json["summary"]["symbol_counts"]["function"].as_u64(),
        Some(2)
    );
}

#[test]
fn callees_query_emits_expected_targets() {
    let output = run_pycg(&[
        "callees",
        "test_code.accuracy_factory.consumer",
        "tests/test_code/accuracy_factory.py",
        "--root",
        "tests",
        "--format",
        "json",
    ])
    .success()
    .get_output()
    .stdout
    .clone();

    let mut json: Value = serde_json::from_slice(&output).expect("valid json output");
    validate_schema("pycg-callees-v1.schema.json", &json);
    normalize_json_paths(&mut json);
    let edges = json["edges"].as_array().expect("edges should be an array");
    assert!(
        edges.iter().any(|edge| {
            edge["target"]["canonical_name"].as_str() == Some("test_code.accuracy_factory.factory")
        }),
        "expected factory in callees, got: {edges:?}"
    );
    assert!(
        edges.iter().any(|edge| {
            edge["target"]["canonical_name"].as_str()
                == Some("test_code.accuracy_factory.Product.make")
        }),
        "expected Product.make in callees, got: {edges:?}"
    );
}

#[test]
fn callers_query_emits_expected_sources() {
    let output = run_pycg(&[
        "callers",
        "test_code.accuracy_factory.factory",
        "tests/test_code/accuracy_factory.py",
        "--root",
        "tests",
        "--format",
        "json",
    ])
    .success()
    .get_output()
    .stdout
    .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid json output");
    validate_schema("pycg-callers-v1.schema.json", &json);
    let edges = json["edges"].as_array().expect("edges should be an array");
    assert!(
        edges.iter().any(|edge| {
            edge["source"]["canonical_name"].as_str() == Some("test_code.accuracy_factory.consumer")
        }),
        "expected consumer in callers, got: {edges:?}"
    );
}

#[test]
fn neighbors_query_emits_incoming_and_outgoing_neighbors() {
    let output = run_pycg(&[
        "neighbors",
        "test_code.accuracy_factory.consumer",
        "tests/test_code/accuracy_factory.py",
        "--root",
        "tests",
        "--format",
        "json",
    ])
    .success()
    .get_output()
    .stdout
    .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid json output");
    validate_schema("pycg-neighbors-v1.schema.json", &json);
    assert_eq!(
        json["incoming"].as_array().map(Vec::len),
        Some(0),
        "consumer should not have direct callers in this fixture"
    );
    assert_eq!(
        json["outgoing"].as_array().map(Vec::len),
        Some(3),
        "consumer should have three outgoing neighbors"
    );
}

#[test]
fn path_query_emits_shortest_path() {
    let output = run_pycg(&[
        "path",
        "test_code.accuracy_factory.consumer",
        "test_code.accuracy_factory.Product.make",
        "tests/test_code/accuracy_factory.py",
        "--root",
        "tests",
        "--format",
        "json",
    ])
    .success()
    .get_output()
    .stdout
    .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid json output");
    validate_schema("pycg-path-v1.schema.json", &json);
    let paths = json["paths"].as_array().expect("paths should be an array");
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0]["nodes"].as_array().map(Vec::len), Some(2));
}

#[test]
fn ambiguous_symbol_query_returns_nonzero_with_structured_error() {
    let output = run_pycg(&[
        "callees",
        "method",
        "tests/test_code/diagnostics_multi_return_three.py",
        "--root",
        "tests",
        "--match",
        "suffix",
        "--format",
        "json",
    ])
    .failure()
    .get_output()
    .stdout
    .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid json output");
    validate_schema("pycg-callees-v1.schema.json", &json);
    assert_eq!(json["status"].as_str(), Some("error"));
    assert_eq!(json["error"]["code"].as_str(), Some("ambiguous_query"));
    assert!(
        json["error"]["matches"]
            .as_array()
            .is_some_and(|matches| matches.len() >= 2),
        "expected multiple ambiguous matches, got: {json:#}"
    );
}

#[test]
fn path_query_text_output_is_human_readable() {
    let output = run_pycg(&[
        "path",
        "test_code.accuracy_factory.consumer",
        "test_code.accuracy_factory.Product.make",
        "tests/test_code/accuracy_factory.py",
        "--root",
        "tests",
        "--format",
        "text",
    ])
    .success()
    .get_output()
    .stdout
    .clone();

    let stdout = String::from_utf8(output).expect("stdout should be utf8");
    assert!(
        stdout.contains(
            "test_code.accuracy_factory.consumer -> test_code.accuracy_factory.Product.make"
        ),
        "expected readable path output, got: {stdout}"
    );
}

#[test]
fn summary_stats_json_includes_per_symbol_counts() {
    let output = run_pycg(&[
        "summary",
        "tests/test_code/accuracy_factory.py",
        "tests/test_code/accuracy_factory.py",
        "--root",
        "tests",
        "--stats",
        "--format",
        "json",
    ])
    .success()
    .get_output()
    .stdout
    .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid json output");
    validate_schema("pycg-summary-v1.schema.json", &json);
    let stats = json["summary"]["symbol_stats"]
        .as_array()
        .expect("symbol_stats should be present with --stats");
    assert!(!stats.is_empty(), "symbol_stats should not be empty");
    for stat in stats {
        assert!(
            stat["caller_count"].is_u64(),
            "caller_count should be integer"
        );
        assert!(
            stat["callee_count"].is_u64(),
            "callee_count should be integer"
        );
        assert!(
            stat["canonical_name"].is_string(),
            "canonical_name should be string"
        );
        assert!(stat["kind"].is_string(), "kind should be string");
    }
    // Verify sorted ascending by caller_count
    let counts: Vec<u64> = stats
        .iter()
        .map(|s| s["caller_count"].as_u64().unwrap())
        .collect();
    for window in counts.windows(2) {
        assert!(
            window[0] <= window[1],
            "symbol_stats should be sorted by caller_count ascending, got: {counts:?}"
        );
    }
}

#[test]
fn summary_stats_module_mode_omits_stats() {
    let output = run_pycg(&[
        "summary",
        "test_code.import_coverage",
        "tests/test_code/import_coverage",
        "--root",
        "tests",
        "--modules",
        "--target-kind",
        "module",
        "--stats",
        "--format",
        "json",
    ])
    .success()
    .get_output()
    .stdout
    .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid json output");
    validate_schema("pycg-summary-v1.schema.json", &json);
    assert!(
        json["summary"]["symbol_stats"].is_null(),
        "symbol_stats should be absent in module mode, got: {json:#}"
    );
}

#[test]
fn summary_stats_text_includes_stats_section() {
    let output = run_pycg(&[
        "summary",
        "tests/test_code/accuracy_factory.py",
        "tests/test_code/accuracy_factory.py",
        "--root",
        "tests",
        "--stats",
        "--format",
        "text",
    ])
    .success()
    .get_output()
    .stdout
    .clone();

    let stdout = String::from_utf8(output).expect("stdout should be utf8");
    assert!(
        stdout.contains("symbol stats"),
        "expected 'symbol stats' section header, got: {stdout}"
    );
    assert!(
        stdout.contains("callers:"),
        "expected 'callers:' in stats output, got: {stdout}"
    );
    assert!(
        stdout.contains("callees:"),
        "expected 'callees:' in stats output, got: {stdout}"
    );
}
