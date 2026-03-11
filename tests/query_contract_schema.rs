use serde_json::{Value, json};
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn load_schema(file_name: &str) -> Value {
    let path = repo_root().join("docs").join("json-schema").join(file_name);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()))
}

fn validate_schema_examples(file_name: &str, valid_examples: &[Value], invalid_examples: &[Value]) {
    let schema = load_schema(file_name);
    jsonschema::meta::validate(&schema)
        .unwrap_or_else(|e| panic!("schema {file_name} should be meta-valid: {e}"));
    let validator = jsonschema::validator_for(&schema)
        .unwrap_or_else(|e| panic!("{file_name} should compile: {e}"));

    for example in valid_examples {
        validator.validate(example).unwrap_or_else(|e| {
            panic!("{file_name} should accept example:\n{example:#}\nerror: {e}")
        });
    }

    for example in invalid_examples {
        assert!(
            validator.is_valid(example).not(),
            "{file_name} should reject invalid example:\n{example:#}"
        );
    }
}

trait BoolExt {
    fn not(self) -> bool;
}

impl BoolExt for bool {
    fn not(self) -> bool {
        !self
    }
}

fn empty_diagnostics() -> Value {
    json!({
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
    })
}

fn symbol_ref(name: &str, kind: &str, path: &str, line: u64) -> Value {
    json!({
        "canonical_name": name,
        "kind": kind,
        "location": {
            "path": path,
            "line": line
        }
    })
}

#[test]
fn query_contract_schemas_accept_representative_examples() {
    validate_schema_examples(
        "pycg-symbols-in-v1.schema.json",
        &[
            json!({
                "schema_version": "1",
                "query_kind": "symbols_in",
                "status": "ok",
                "query": {
                    "target": "pkg/mod.py",
                    "target_kind": "path",
                    "graph_mode": "symbol"
                },
                "symbols": [
                    symbol_ref("pkg.mod.Helper", "class", "pkg/mod.py", 10)
                ],
                "diagnostics": empty_diagnostics()
            }),
            json!({
                "schema_version": "1",
                "query_kind": "symbols_in",
                "status": "error",
                "query": {
                    "target": "pkg.missing",
                    "target_kind": "module"
                },
                "error": {
                    "code": "target_not_found",
                    "message": "No module matched query"
                }
            }),
        ],
        &[json!({
            "schema_version": "1",
            "query_kind": "symbols_in",
            "status": "ok",
            "query": {
                "target": "pkg/mod.py",
                "target_kind": "path"
            },
            "diagnostics": empty_diagnostics()
        })],
    );

    validate_schema_examples(
        "pycg-summary-v1.schema.json",
        &[json!({
            "schema_version": "1",
            "query_kind": "summary",
            "status": "ok",
            "query": {
                "target": "pkg/mod.py",
                "target_kind": "path",
                "graph_mode": "symbol"
            },
            "summary": {
                "file_count": 1,
                "symbol_counts": {
                    "class": 1,
                    "function": 2
                },
                "edge_counts": {
                    "incoming_uses": 1,
                    "outgoing_uses": 3
                },
                "top_level_symbols": [
                    symbol_ref("pkg.mod.Helper", "class", "pkg/mod.py", 10),
                    symbol_ref("pkg.mod.run", "function", "pkg/mod.py", 20)
                ]
            },
            "diagnostics": empty_diagnostics()
        })],
        &[json!({
            "schema_version": "1",
            "query_kind": "summary",
            "status": "ok",
            "query": {
                "target": "pkg/mod.py",
                "target_kind": "path"
            },
            "summary": {
                "file_count": 1,
                "top_level_symbols": []
            },
            "diagnostics": empty_diagnostics()
        })],
    );

    validate_schema_examples(
        "pycg-callees-v1.schema.json",
        &[
            json!({
                "schema_version": "1",
                "query_kind": "callees",
                "status": "ok",
                "query": {
                    "symbol": "pkg.mod.run",
                    "match_mode": "exact",
                    "graph_mode": "symbol"
                },
                "node": symbol_ref("pkg.mod.run", "function", "pkg/mod.py", 20),
                "edges": [
                    {
                        "kind": "uses",
                        "target": symbol_ref("pkg.other.helper", "function", "pkg/other.py", 5)
                    }
                ],
                "diagnostics": empty_diagnostics()
            }),
            json!({
                "schema_version": "1",
                "query_kind": "callees",
                "status": "error",
                "query": {
                    "symbol": "make",
                    "match_mode": "suffix"
                },
                "error": {
                    "code": "ambiguous_query",
                    "message": "Query matched multiple symbols",
                    "matches": ["pkg.a.make", "pkg.b.make"]
                }
            }),
        ],
        &[json!({
            "schema_version": "1",
            "query_kind": "callees",
            "status": "ok",
            "query": {
                "symbol": "pkg.mod.run",
                "match_mode": "exact"
            },
            "node": symbol_ref("pkg.mod.run", "function", "pkg/mod.py", 20),
            "edges": [
                {
                    "kind": "uses",
                    "source": symbol_ref("pkg.other.helper", "function", "pkg/other.py", 5)
                }
            ],
            "diagnostics": empty_diagnostics()
        })],
    );

    validate_schema_examples(
        "pycg-callers-v1.schema.json",
        &[json!({
            "schema_version": "1",
            "query_kind": "callers",
            "status": "ok",
            "query": {
                "symbol": "pkg.other.helper",
                "match_mode": "exact"
            },
            "node": symbol_ref("pkg.other.helper", "function", "pkg/other.py", 5),
            "edges": [
                {
                    "kind": "uses",
                    "source": symbol_ref("pkg.mod.run", "function", "pkg/mod.py", 20)
                }
            ],
            "diagnostics": empty_diagnostics()
        })],
        &[json!({
            "schema_version": "1",
            "query_kind": "callers",
            "status": "ok",
            "query": {
                "symbol": "pkg.other.helper",
                "match_mode": "exact"
            },
            "node": symbol_ref("pkg.other.helper", "function", "pkg/other.py", 5),
            "edges": [
                {
                    "kind": "defines",
                    "source": symbol_ref("pkg.mod.run", "function", "pkg/mod.py", 20)
                }
            ],
            "diagnostics": empty_diagnostics()
        })],
    );

    validate_schema_examples(
        "pycg-neighbors-v1.schema.json",
        &[json!({
            "schema_version": "1",
            "query_kind": "neighbors",
            "status": "ok",
            "query": {
                "symbol": "pkg.mod.run",
                "match_mode": "exact"
            },
            "node": symbol_ref("pkg.mod.run", "function", "pkg/mod.py", 20),
            "incoming": [
                {
                    "kind": "uses",
                    "source": symbol_ref("pkg.app.main", "function", "pkg/app.py", 40)
                }
            ],
            "outgoing": [
                {
                    "kind": "uses",
                    "target": symbol_ref("pkg.other.helper", "function", "pkg/other.py", 5)
                }
            ],
            "diagnostics": empty_diagnostics()
        })],
        &[json!({
            "schema_version": "1",
            "query_kind": "neighbors",
            "status": "ok",
            "query": {
                "symbol": "pkg.mod.run",
                "match_mode": "exact"
            },
            "node": symbol_ref("pkg.mod.run", "function", "pkg/mod.py", 20),
            "incoming": [],
            "outgoing": [
                {
                    "kind": "uses"
                }
            ],
            "diagnostics": empty_diagnostics()
        })],
    );

    validate_schema_examples(
        "pycg-path-v1.schema.json",
        &[
            json!({
                "schema_version": "1",
                "query_kind": "path",
                "status": "ok",
                "query": {
                    "source": "pkg.app.main",
                    "target": "pkg.other.helper",
                    "match_mode": "exact"
                },
                "paths": [
                    {
                        "nodes": [
                            symbol_ref("pkg.app.main", "function", "pkg/app.py", 40),
                            symbol_ref("pkg.mod.run", "function", "pkg/mod.py", 20),
                            symbol_ref("pkg.other.helper", "function", "pkg/other.py", 5)
                        ],
                        "edges": [
                            {
                                "kind": "uses",
                                "source": "pkg.app.main",
                                "target": "pkg.mod.run"
                            },
                            {
                                "kind": "uses",
                                "source": "pkg.mod.run",
                                "target": "pkg.other.helper"
                            }
                        ]
                    }
                ],
                "diagnostics": empty_diagnostics()
            }),
            json!({
                "schema_version": "1",
                "query_kind": "path",
                "status": "error",
                "query": {
                    "source": "pkg.app.main",
                    "target": "pkg.missing.helper",
                    "match_mode": "exact"
                },
                "error": {
                    "code": "path_not_found",
                    "message": "No path connected the resolved symbols"
                }
            }),
        ],
        &[json!({
            "schema_version": "1",
            "query_kind": "path",
            "status": "ok",
            "query": {
                "source": "pkg.app.main",
                "target": "pkg.other.helper",
                "match_mode": "exact"
            },
            "paths": [
                {
                    "nodes": [],
                    "edges": [
                        {
                            "kind": "uses",
                            "source": "pkg.app.main"
                        }
                    ]
                }
            ],
            "diagnostics": empty_diagnostics()
        })],
    );
}
