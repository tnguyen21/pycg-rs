//! Integration tests for pyan-rs.
//!
//! Uses Python test fixtures in tests/test_code/ and tests/old_tests/.

use std::collections::HashSet;
use std::path::PathBuf;

use pyan_rs::analyzer::CallGraph;
use pyan_rs::visgraph::{VisualGraph, VisualOptions};
use pyan_rs::writer;

fn test_code_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("test_code")
}

fn collect_py_files(dir: &std::path::Path) -> Vec<String> {
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path().extension().is_some_and(|ext| ext == "py")
                && !e.path().to_string_lossy().contains("__pycache__")
        })
    {
        files.push(entry.path().to_string_lossy().to_string());
    }
    files.sort();
    files
}

fn make_call_graph(dir: &std::path::Path) -> CallGraph {
    let files = collect_py_files(dir);
    let root = dir.parent().unwrap().to_string_lossy().to_string();
    CallGraph::new(&files, Some(&root)).expect("analysis should succeed")
}

/// Find all nodes with the given short name, or whose fully qualified name ends with the given name.
fn find_nodes_by_name(cg: &CallGraph, name: &str) -> Vec<usize> {
    let mut result: Vec<usize> = cg.nodes_by_name
        .get(name)
        .cloned()
        .unwrap_or_default();
    for (idx, node) in cg.nodes_arena.iter().enumerate() {
        if node.get_name() == name || node.get_name().ends_with(&format!(".{name}")) {
            if !result.contains(&idx) {
                result.push(idx);
            }
        }
    }
    result
}

/// Get the set of short names that `source_name` defines.
fn get_defines(cg: &CallGraph, source_name: &str) -> HashSet<String> {
    let mut result = HashSet::new();
    for &nid in find_nodes_by_name(cg, source_name).iter() {
        if let Some(targets) = cg.defines_edges.get(&nid) {
            for &tid in targets {
                result.insert(cg.nodes_arena[tid].name.clone());
            }
        }
    }
    result
}

/// Get the set of short names that `source_name` uses.
fn get_uses(cg: &CallGraph, source_name: &str) -> HashSet<String> {
    let mut result = HashSet::new();
    for &nid in find_nodes_by_name(cg, source_name).iter() {
        if let Some(targets) = cg.uses_edges.get(&nid) {
            for &tid in targets {
                result.insert(cg.nodes_arena[tid].name.clone());
            }
        }
    }
    result
}

/// Check if there is a defines edge from a node matching `from_name` to one matching `to_name`.
fn has_defines_edge(cg: &CallGraph, from_name: &str, to_name: &str) -> bool {
    for &fid in find_nodes_by_name(cg, from_name).iter() {
        if let Some(targets) = cg.defines_edges.get(&fid) {
            for &tid in targets {
                if cg.nodes_arena[tid].name == to_name {
                    return true;
                }
            }
        }
    }
    false
}

/// Check if there is a uses edge from a node matching `from_name` to one matching `to_name`.
fn has_uses_edge(cg: &CallGraph, from_name: &str, to_name: &str) -> bool {
    for &fid in find_nodes_by_name(cg, from_name).iter() {
        if let Some(targets) = cg.uses_edges.get(&fid) {
            for &tid in targets {
                if cg.nodes_arena[tid].name == to_name {
                    return true;
                }
            }
        }
    }
    false
}

// ===================================================================
// Core analysis tests
// ===================================================================

#[test]
fn test_modules_found() {
    let cg = make_call_graph(&test_code_dir());
    let module_names: Vec<_> = cg.nodes_arena.iter()
        .filter(|n| n.flavor == pyan_rs::node::Flavor::Module)
        .map(|n| n.get_name())
        .collect();
    assert!(module_names.iter().any(|n| n.contains("submodule1")), "submodule1 not found");
    assert!(module_names.iter().any(|n| n.contains("submodule2")), "submodule2 not found");
}

#[test]
fn test_class_found() {
    let cg = make_call_graph(&test_code_dir());
    let classes: Vec<_> = cg.nodes_arena.iter()
        .filter(|n| n.flavor == pyan_rs::node::Flavor::Class)
        .map(|n| n.name.clone())
        .collect();
    assert!(classes.contains(&"A".to_string()), "Class A not found, got: {:?}", classes);
}

#[test]
fn test_function_found() {
    let cg = make_call_graph(&test_code_dir());
    let functions: Vec<_> = cg.nodes_arena.iter()
        .filter(|n| matches!(n.flavor, pyan_rs::node::Flavor::Function | pyan_rs::node::Flavor::Method))
        .map(|n| n.name.clone())
        .collect();
    assert!(functions.contains(&"test_func1".to_string()), "test_func1 not found, got: {:?}", functions);
}

#[test]
fn test_submodule_defines() {
    let cg = make_call_graph(&test_code_dir());
    let defs = get_defines(&cg, "submodule2");
    assert!(defs.contains("test_2"), "submodule2 should define test_2, got: {:?}", defs);
}

#[test]
fn test_uses_edge_exists() {
    let cg = make_call_graph(&test_code_dir());
    let uses = get_uses(&cg, "test_2");
    assert!(
        uses.contains("test_func1") || uses.contains("test_func2"),
        "test_2 should use test_func1 or test_func2, got: {:?}", uses
    );
}

// ===================================================================
// DOT output format tests
// ===================================================================

#[test]
fn test_dot_output_valid() {
    let cg = make_call_graph(&test_code_dir());
    let opts = VisualOptions {
        draw_defines: true,
        draw_uses: true,
        colored: true,
        grouped: false,
        annotated: false,
    };
    let vg = VisualGraph::from_call_graph(
        &cg.nodes_arena, &cg.defined, &cg.defines_edges, &cg.uses_edges, &opts,
    );
    let dot = writer::write_dot(&vg, &["rankdir=TB".to_string()]);
    assert!(dot.starts_with("digraph G {"), "DOT output should start with 'digraph G {{'");
    assert!(dot.trim().ends_with('}'), "DOT output should end with '}}'");
    assert!(dot.contains("->"), "DOT output should contain edges");
    assert!(dot.contains("style=\"dashed\""), "DOT output should have defines edges (dashed)");
    assert!(dot.contains("style=\"solid\""), "DOT output should have uses edges (solid)");
}

#[test]
fn test_dot_output_grouped() {
    let cg = make_call_graph(&test_code_dir());
    let opts = VisualOptions {
        draw_defines: true,
        draw_uses: true,
        colored: true,
        grouped: true,
        annotated: false,
    };
    let vg = VisualGraph::from_call_graph(
        &cg.nodes_arena, &cg.defined, &cg.defines_edges, &cg.uses_edges, &opts,
    );
    let dot = writer::write_dot(&vg, &["rankdir=TB".to_string()]);
    assert!(dot.contains("subgraph cluster_"), "Grouped DOT should have subgraphs");
}

// ===================================================================
// TGF output format tests
// ===================================================================

#[test]
fn test_tgf_output_valid() {
    let cg = make_call_graph(&test_code_dir());
    let opts = VisualOptions {
        draw_defines: true,
        draw_uses: true,
        colored: false,
        grouped: false,
        annotated: false,
    };
    let vg = VisualGraph::from_call_graph(
        &cg.nodes_arena, &cg.defined, &cg.defines_edges, &cg.uses_edges, &opts,
    );
    let tgf = writer::write_tgf(&vg);
    assert!(tgf.contains('#'), "TGF should have # separator");
    let parts: Vec<&str> = tgf.splitn(2, '#').collect();
    assert_eq!(parts.len(), 2);
    let edges_section = parts[1].trim();
    assert!(!edges_section.is_empty(), "TGF should have edges");
}

// ===================================================================
// Text output format tests
// ===================================================================

#[test]
fn test_text_output_valid() {
    let cg = make_call_graph(&test_code_dir());
    let opts = VisualOptions {
        draw_defines: true,
        draw_uses: true,
        colored: false,
        grouped: false,
        annotated: false,
    };
    let vg = VisualGraph::from_call_graph(
        &cg.nodes_arena, &cg.defined, &cg.defines_edges, &cg.uses_edges, &opts,
    );
    let text = writer::write_text(&vg);
    assert!(text.contains("[D]") || text.contains("[U]"), "Text should have tagged edges");
    for line in text.lines() {
        if line.starts_with("    ") {
            assert!(
                line.contains("[D]") || line.contains("[U]"),
                "Indented lines should be tagged edges: {line}"
            );
        }
    }
}

// ===================================================================
// Regression: don't crash on edge cases
// ===================================================================

#[test]
fn test_regression_annotated_assignments() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("old_tests")
        .join("issue2");
    if dir.exists() {
        let files = collect_py_files(&dir);
        let _ = CallGraph::new(&files, None);
    }
}

#[test]
fn test_regression_comprehensions() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("old_tests")
        .join("issue3");
    if dir.exists() {
        let files = collect_py_files(&dir);
        let _ = CallGraph::new(&files, None);
    }
}

#[test]
fn test_regression_external_deps() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("old_tests")
        .join("issue5");
    if dir.exists() {
        let files = collect_py_files(&dir);
        let _ = CallGraph::new(&files, None);
    }
}

// ===================================================================
// Feature coverage (features.py)
// ===================================================================

fn make_features_graph() -> CallGraph {
    let features_file = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("test_code")
        .join("features.py");
    let files = vec![features_file.to_string_lossy().to_string()];
    CallGraph::new(&files, None).expect("should parse features.py")
}

#[test]
fn test_features_classes_found() {
    let cg = make_features_graph();
    let class_names: HashSet<_> = cg.nodes_arena.iter()
        .filter(|n| n.flavor == pyan_rs::node::Flavor::Class)
        .map(|n| n.name.as_str())
        .collect();
    for expected in ["Decorated", "Base", "Derived", "MixinA", "MixinB", "Combined"] {
        assert!(class_names.contains(expected), "Class {expected} not found, got: {class_names:?}");
    }
}

#[test]
fn test_features_decorators() {
    let cg = make_features_graph();
    assert!(has_defines_edge(&cg, "Decorated", "static_method"));
    assert!(has_defines_edge(&cg, "Decorated", "class_method"));
    assert!(has_defines_edge(&cg, "Decorated", "my_prop"));
    assert!(has_defines_edge(&cg, "Decorated", "regular"));

    let sm: Vec<_> = find_nodes_by_name(&cg, "static_method").into_iter()
        .filter(|&id| cg.nodes_arena[id].flavor == pyan_rs::node::Flavor::StaticMethod)
        .collect();
    assert!(!sm.is_empty(), "static_method should have StaticMethod flavor");

    let cm: Vec<_> = find_nodes_by_name(&cg, "class_method").into_iter()
        .filter(|&id| cg.nodes_arena[id].flavor == pyan_rs::node::Flavor::ClassMethod)
        .collect();
    assert!(!cm.is_empty(), "class_method should have ClassMethod flavor");
}

#[test]
fn test_features_inheritance() {
    let cg = make_features_graph();
    assert!(has_uses_edge(&cg, "Derived", "Base"),
            "Derived should use Base (inheritance)");
    assert!(has_uses_edge(&cg, "bar", "foo"),
            "bar should use foo");
}

#[test]
fn test_features_multiple_inheritance() {
    let cg = make_features_graph();
    assert!(has_uses_edge(&cg, "Combined", "MixinA"),
            "Combined should use MixinA");
    assert!(has_uses_edge(&cg, "Combined", "MixinB"),
            "Combined should use MixinB");
}

// ===================================================================
// Performance
// ===================================================================

#[test]
fn test_performance() {
    let dir = test_code_dir();
    let files = collect_py_files(&dir);
    let root = dir.parent().unwrap().to_string_lossy().to_string();

    let start = std::time::Instant::now();
    for _ in 0..100 {
        let _ = CallGraph::new(&files, Some(&root)).unwrap();
    }
    let elapsed = start.elapsed();
    let per_run = elapsed / 100;
    eprintln!("Average analysis time: {:?} (100 runs over {} files)", per_run, files.len());
    assert!(per_run.as_millis() < 200, "Analysis too slow: {:?}", per_run);
}
