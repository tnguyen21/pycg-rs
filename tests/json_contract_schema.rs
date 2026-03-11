use serde_json::{Value, json};
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn load_schema() -> Value {
    let path = repo_root()
        .join("docs")
        .join("json-schema")
        .join("pycg-graph-v1.schema.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()))
}

fn sample_symbol_graph() -> Value {
    json!({
        "schema_version": "1",
        "tool": {
            "name": "pycg-rs",
            "version": "0.1.0",
            "commit": "36969ec"
        },
        "graph_mode": "symbol",
        "analysis": {
            "root": "tests",
            "inputs": [
                "tests/test_code/accuracy_factory.py"
            ],
            "node_inclusion_policy": "defined_only",
            "path_kind": "root_relative"
        },
        "stats": {
            "nodes": 5,
            "edges": 4,
            "files_analyzed": 1,
            "by_node_kind": {
                "module": 1,
                "class": 1,
                "function": 2,
                "method": 1
            },
            "by_edge_kind": {
                "defines": 1,
                "uses": 3
            }
        },
        "nodes": [
            {
                "id": "n1",
                "kind": "module",
                "canonical_name": "tests.test_code.accuracy_factory",
                "name": "accuracy_factory",
                "namespace": "tests.test_code",
                "location": {
                    "path": "tests/test_code/accuracy_factory.py",
                    "line": 1
                }
            },
            {
                "id": "n2",
                "kind": "class",
                "canonical_name": "tests.test_code.accuracy_factory.Product",
                "name": "Product",
                "namespace": "tests.test_code.accuracy_factory",
                "location": {
                    "path": "tests/test_code/accuracy_factory.py",
                    "line": 15
                }
            },
            {
                "id": "n3",
                "kind": "function",
                "canonical_name": "tests.test_code.accuracy_factory.factory",
                "name": "factory",
                "namespace": "tests.test_code.accuracy_factory",
                "location": {
                    "path": "tests/test_code/accuracy_factory.py",
                    "line": 20
                }
            },
            {
                "id": "n4",
                "kind": "function",
                "canonical_name": "tests.test_code.accuracy_factory.consumer",
                "name": "consumer",
                "namespace": "tests.test_code.accuracy_factory",
                "location": {
                    "path": "tests/test_code/accuracy_factory.py",
                    "line": 25
                }
            },
            {
                "id": "n5",
                "kind": "method",
                "canonical_name": "tests.test_code.accuracy_factory.Product.make",
                "name": "make",
                "namespace": "tests.test_code.accuracy_factory.Product",
                "location": {
                    "path": "tests/test_code/accuracy_factory.py",
                    "line": 16
                }
            }
        ],
        "edges": [
            {
                "kind": "defines",
                "source": "n1",
                "target": "n2"
            },
            {
                "kind": "uses",
                "source": "n3",
                "target": "n2"
            },
            {
                "kind": "uses",
                "source": "n4",
                "target": "n3"
            },
            {
                "kind": "uses",
                "source": "n4",
                "target": "n5"
            }
        ],
        "diagnostics": {
            "summary": {
                "warnings": 1,
                "unresolved_references": 1,
                "ambiguous_resolutions": 1,
                "external_references": 1,
                "approximations": 1
            },
            "warnings": [
                {
                    "code": "unsupported_dynamic_import",
                    "message": "Could not statically resolve dynamic import",
                    "path": "tests/test_code/accuracy_factory.py",
                    "line": 4
                }
            ],
            "unresolved_references": [
                {
                    "kind": "call",
                    "source": "n4",
                    "symbol": "unknown_func",
                    "path": "tests/test_code/accuracy_factory.py",
                    "line": 29
                }
            ],
            "ambiguous_resolutions": [
                {
                    "kind": "call",
                    "source": "n4",
                    "symbol": "make",
                    "candidate_targets": ["n5"],
                    "path": "tests/test_code/accuracy_factory.py",
                    "line": 30
                }
            ],
            "external_references": [
                {
                    "kind": "import",
                    "source": "n1",
                    "canonical_name": "requests.sessions.Session",
                    "path": "tests/test_code/accuracy_factory.py",
                    "line": 2
                }
            ],
            "approximations": [
                {
                    "kind": "wildcard_expansion",
                    "source": "n4",
                    "symbol": "make",
                    "reason": "expanded_from_unknown_receiver",
                    "candidate_targets": ["n5"],
                    "path": "tests/test_code/accuracy_factory.py",
                    "line": 31
                }
            ]
        }
    })
}

fn sample_module_graph() -> Value {
    json!({
        "schema_version": "1",
        "tool": {
            "name": "pycg-rs",
            "version": "0.1.0"
        },
        "graph_mode": "module",
        "analysis": {
            "inputs": [
                "tests/test_code/import_coverage"
            ],
            "node_inclusion_policy": "defined_only",
            "path_kind": "root_relative"
        },
        "stats": {
            "nodes": 2,
            "edges": 1,
            "files_analyzed": 2,
            "by_node_kind": {
                "module": 2
            },
            "by_edge_kind": {
                "uses": 1
            }
        },
        "nodes": [
            {
                "id": "m1",
                "kind": "module",
                "canonical_name": "test_code.import_coverage.user",
                "name": "user",
                "namespace": "test_code.import_coverage",
                "location": {
                    "path": "tests/test_code/import_coverage/user.py",
                    "line": 1
                }
            },
            {
                "id": "m2",
                "kind": "module",
                "canonical_name": "test_code.import_coverage.sibling",
                "name": "sibling",
                "namespace": "test_code.import_coverage",
                "location": {
                    "path": "tests/test_code/import_coverage/sibling.py",
                    "line": 1
                }
            }
        ],
        "edges": [
            {
                "kind": "uses",
                "source": "m1",
                "target": "m2"
            }
        ],
        "diagnostics": {
            "summary": {
                "warnings": 0,
                "unresolved_references": 0,
                "ambiguous_resolutions": 0,
                "external_references": 0,
                "approximations": 0
            },
            "warnings": [],
            "unresolved_references": [],
            "ambiguous_resolutions": [],
            "external_references": [],
            "approximations": []
        }
    })
}

#[test]
fn json_contract_schema_is_meta_valid() {
    let schema = load_schema();
    jsonschema::meta::validate(&schema).expect("schema should validate against its meta-schema");
}

#[test]
fn json_contract_schema_accepts_symbol_graph_examples() {
    let schema = load_schema();
    let instance = sample_symbol_graph();
    jsonschema::validate(&schema, &instance).expect("symbol graph example should match schema");
}

#[test]
fn json_contract_schema_accepts_module_graph_examples() {
    let schema = load_schema();
    let instance = sample_module_graph();
    jsonschema::validate(&schema, &instance).expect("module graph example should match schema");
}

#[test]
fn json_contract_schema_rejects_missing_diagnostics() {
    let schema = load_schema();
    let mut instance = sample_symbol_graph();
    instance
        .as_object_mut()
        .expect("sample symbol graph should be an object")
        .remove("diagnostics");
    assert!(
        jsonschema::validate(&schema, &instance).is_err(),
        "instance without diagnostics should not match schema"
    );
}
