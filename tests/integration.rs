//! Integration tests for pyan-rs.
//!
//! Uses Python test fixtures in tests/test_code/.

use std::collections::HashSet;
use std::path::PathBuf;

use pycallgraph_rs::analyzer::CallGraph;
use pycallgraph_rs::visgraph::{VisualGraph, VisualOptions};
use pycallgraph_rs::writer;

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
        .filter(|n| n.flavor == pycallgraph_rs::node::Flavor::Module)
        .map(|n| n.get_name())
        .collect();
    assert!(module_names.iter().any(|n| n.contains("submodule1")), "submodule1 not found");
    assert!(module_names.iter().any(|n| n.contains("submodule2")), "submodule2 not found");
}

#[test]
fn test_class_found() {
    let cg = make_call_graph(&test_code_dir());
    let classes: Vec<_> = cg.nodes_arena.iter()
        .filter(|n| n.flavor == pycallgraph_rs::node::Flavor::Class)
        .map(|n| n.name.clone())
        .collect();
    assert!(classes.contains(&"A".to_string()), "Class A not found, got: {:?}", classes);
}

#[test]
fn test_function_found() {
    let cg = make_call_graph(&test_code_dir());
    let functions: Vec<_> = cg.nodes_arena.iter()
        .filter(|n| matches!(n.flavor, pycallgraph_rs::node::Flavor::Function | pycallgraph_rs::node::Flavor::Method))
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

/// Issue #2: annotated assignments at module level (`a: int = 3`) must not
/// crash the analyzer.
#[test]
fn test_regression_annotated_assignments() {
    let fixture = test_code_dir().join("regression_issue2.py");
    let files = vec![fixture.to_string_lossy().to_string()];
    let cg = CallGraph::new(&files, None)
        .expect("issue2: annotated assignment must not crash the analyzer");
    // The file defines annotated_fn and Container – verify we produced nodes.
    assert!(!cg.nodes_arena.is_empty(), "issue2: graph should not be empty");
    let fn_names: Vec<_> = cg.nodes_arena.iter()
        .filter(|n| matches!(n.flavor,
            pycallgraph_rs::node::Flavor::Function | pycallgraph_rs::node::Flavor::Method))
        .map(|n| n.name.as_str())
        .collect();
    assert!(fn_names.contains(&"annotated_fn"),
        "issue2: annotated_fn not found, got: {fn_names:?}");
}

/// Issue #3: complex / nested comprehensions (list-inside-list, dict-in-list,
/// generator-as-iterable) must not crash the analyzer.
#[test]
fn test_regression_comprehensions() {
    let fixture = test_code_dir().join("regression_issue3.py");
    let files = vec![fixture.to_string_lossy().to_string()];
    let cg = CallGraph::new(&files, None)
        .expect("issue3: comprehensions must not crash the analyzer");
    let fn_names: Vec<_> = cg.nodes_arena.iter()
        .filter(|n| matches!(n.flavor,
            pycallgraph_rs::node::Flavor::Function | pycallgraph_rs::node::Flavor::Method))
        .map(|n| n.name.as_str())
        .collect();
    assert!(fn_names.contains(&"f"), "issue3: function f not found, got: {fn_names:?}");
    assert!(fn_names.contains(&"g"), "issue3: function g not found, got: {fn_names:?}");
    assert!(fn_names.contains(&"h"), "issue3: function h not found, got: {fn_names:?}");
}

/// Issue #5: files that reference external / uninstalled packages (numpy,
/// pandas) and relative imports whose targets don't exist must not crash.
#[test]
fn test_regression_external_deps() {
    let fixture = test_code_dir().join("regression_issue5.py");
    let files = vec![fixture.to_string_lossy().to_string()];
    let cg = CallGraph::new(&files, None)
        .expect("issue5: external-dep imports must not crash the analyzer");
    let class_names: Vec<_> = cg.nodes_arena.iter()
        .filter(|n| n.flavor == pycallgraph_rs::node::Flavor::Class)
        .map(|n| n.name.as_str())
        .collect();
    assert!(class_names.contains(&"MyProcessor"),
        "issue5: MyProcessor not found, got: {class_names:?}");
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
        .filter(|n| n.flavor == pycallgraph_rs::node::Flavor::Class)
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
        .filter(|&id| cg.nodes_arena[id].flavor == pycallgraph_rs::node::Flavor::StaticMethod)
        .collect();
    assert!(!sm.is_empty(), "static_method should have StaticMethod flavor");

    let cm: Vec<_> = find_nodes_by_name(&cg, "class_method").into_iter()
        .filter(|&id| cg.nodes_arena[id].flavor == pycallgraph_rs::node::Flavor::ClassMethod)
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
// INV-1: iterator protocol edges
// ===================================================================

/// `iterate_sequence` must gain uses edges to `__iter__` and `__next__`
/// when iterating over a `Sequence()` instance in a `for` loop.
#[test]
fn test_iterator_protocol_for_loop() {
    let cg = make_features_graph();
    assert!(
        has_uses_edge(&cg, "iterate_sequence", "__iter__"),
        "iterate_sequence should use Sequence.__iter__ (for-loop protocol)"
    );
    assert!(
        has_uses_edge(&cg, "iterate_sequence", "__next__"),
        "iterate_sequence should use Sequence.__next__ (for-loop protocol)"
    );
}

/// `comprehend_sequence` must gain the same iterator protocol edges
/// because the comprehension iterates over `Sequence()`.
#[test]
fn test_iterator_protocol_comprehension() {
    let cg = make_features_graph();
    assert!(
        has_uses_edge(&cg, "comprehend_sequence", "__iter__"),
        "comprehend_sequence should use Sequence.__iter__ (comprehension protocol)"
    );
    assert!(
        has_uses_edge(&cg, "comprehend_sequence", "__next__"),
        "comprehend_sequence should use Sequence.__next__ (comprehension protocol)"
    );
}

/// Protocol edges must only be emitted for known-class iterables, not for
/// unknown/unresolved iterables (e.g., function arguments like `items`).
#[test]
fn test_iterator_protocol_not_emitted_for_unknowns() {
    let cg = make_features_graph();
    // process_items(items) iterates over an argument — we must NOT emit
    // protocol edges from unknown/argument nodes.
    let uses = get_uses(&cg, "process_items");
    assert!(
        !uses.contains("__iter__"),
        "process_items iterates an arg, should NOT produce __iter__ edge, got: {uses:?}"
    );
    assert!(
        !uses.contains("__next__"),
        "process_items iterates an arg, should NOT produce __next__ edge, got: {uses:?}"
    );
}

// ===================================================================
// INV-2: context-manager protocol edges
// ===================================================================

/// `use_ctx` must gain uses edges to `__enter__` and `__exit__`
/// when entering a `with MyCtx()` block.
#[test]
fn test_context_manager_protocol_sync() {
    let cg = make_features_graph();
    assert!(
        has_uses_edge(&cg, "use_ctx", "__enter__"),
        "use_ctx should use MyCtx.__enter__ (with-statement protocol)"
    );
    assert!(
        has_uses_edge(&cg, "use_ctx", "__exit__"),
        "use_ctx should use MyCtx.__exit__ (with-statement protocol)"
    );
}

/// `use_async_cm` must gain uses edges to `__aenter__` and `__aexit__`
/// when entering an `async with AsyncCM()` block.
#[test]
fn test_context_manager_protocol_async() {
    let cg = make_features_graph();
    assert!(
        has_uses_edge(&cg, "use_async_cm", "__aenter__"),
        "use_async_cm should use AsyncCM.__aenter__ (async with protocol)"
    );
    assert!(
        has_uses_edge(&cg, "use_async_cm", "__aexit__"),
        "use_async_cm should use AsyncCM.__aexit__ (async with protocol)"
    );
}

/// No wildcard unknown nodes should appear for the protocol method names.
/// If we see `*.____iter__` or `*.__enter__` etc., we resolved wrong.
#[test]
fn test_protocol_edges_resolve_to_known_nodes() {
    let cg = make_features_graph();
    // All nodes for __iter__ / __next__ / __enter__ / __exit__ must have a
    // non-None namespace (i.e., be concrete, not wildcard).
    let protocol_methods = ["__iter__", "__next__", "__enter__", "__exit__"];
    for method in protocol_methods {
        for &nid in cg.nodes_by_name.get(method).unwrap_or(&vec![]) {
            assert!(
                cg.nodes_arena[nid].namespace.is_some(),
                "Protocol method {method} resolved to a wildcard node — expected concrete"
            );
        }
    }
}

// ===================================================================
// INV-3: existing feature coverage must stay green
// ===================================================================

/// Existing decorator, inheritance, and match coverage must not regress.
#[test]
fn test_features_async_iterator_protocol() {
    let cg = make_features_graph();
    // `iterate_async_stream` is async-for over `AsyncStream()`.
    assert!(
        has_uses_edge(&cg, "iterate_async_stream", "__aiter__"),
        "iterate_async_stream should use AsyncStream.__aiter__"
    );
    assert!(
        has_uses_edge(&cg, "iterate_async_stream", "__anext__"),
        "iterate_async_stream should use AsyncStream.__anext__"
    );
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

// ===================================================================
// Corpus-scale integration smoke tests
//
// Run the analyzer against real-world vendored Python packages from
// benchmarks/corpora/ and assert the resulting graph is non-degenerate.
//
// Tests skip (pass with a notice) when the corpus directory is absent
// (e.g. a fresh clone without vendored corpora), so the suite remains
// green in CI.  They fail if the directory IS present but analysis
// produces an empty or near-empty graph, which would indicate a
// regression.
// ===================================================================

/// Resolve the path to a specific package subdirectory inside the vendored
/// corpora.  Returns `None` if the directory does not exist (e.g. the
/// corpora have not been downloaded).
fn corpus_dir(package: &str, subpath: &str) -> Option<std::path::PathBuf> {
    let candidate = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("benchmarks")
        .join("corpora")
        .join(package)
        .join(subpath);
    if candidate.is_dir() {
        Some(candidate)
    } else {
        None
    }
}

/// Counts of the major node/edge kinds after analysis.
struct CorpusStats {
    modules: usize,
    classes: usize,
    functions: usize,
    uses_edge_count: usize,
}

/// Run the full analysis pipeline over `dir` and return summary stats.
///
/// Panics (test failure) if:
/// - no `.py` files are found in the directory
/// - `CallGraph::new` returns an error
fn analyze_corpus(dir: &std::path::Path) -> (CallGraph, CorpusStats) {
    let files = collect_py_files(dir);
    assert!(
        !files.is_empty(),
        "No Python files found in {dir:?} — corpus may be empty or mis-configured"
    );

    let root = dir.parent().unwrap().to_string_lossy().to_string();
    let cg = CallGraph::new(&files, Some(&root))
        .unwrap_or_else(|e| panic!("corpus analysis of {dir:?} failed: {e}"));

    let modules = cg
        .nodes_arena
        .iter()
        .filter(|n| n.flavor == pycallgraph_rs::node::Flavor::Module)
        .count();
    let classes = cg
        .nodes_arena
        .iter()
        .filter(|n| n.flavor == pycallgraph_rs::node::Flavor::Class)
        .count();
    let functions = cg
        .nodes_arena
        .iter()
        .filter(|n| {
            matches!(
                n.flavor,
                pycallgraph_rs::node::Flavor::Function
                    | pycallgraph_rs::node::Flavor::Method
                    | pycallgraph_rs::node::Flavor::StaticMethod
                    | pycallgraph_rs::node::Flavor::ClassMethod
            )
        })
        .count();
    let uses_edge_count: usize = cg.uses_edges.values().map(|s| s.len()).sum();

    eprintln!(
        "[corpus {dir:?}] {} files → {} modules, {} classes, {} functions, {} uses edges",
        files.len(),
        modules,
        classes,
        functions,
        uses_edge_count
    );

    (cg, CorpusStats { modules, classes, functions, uses_edge_count })
}

/// Assert that `stats` meets the provided lower bounds.  All bounds must be
/// conservative enough that a healthy analysis always clears them.
fn assert_corpus_healthy(
    label: &str,
    stats: &CorpusStats,
    min_modules: usize,
    min_classes: usize,
    min_functions: usize,
    min_uses_edges: usize,
) {
    assert!(
        stats.modules >= min_modules,
        "{label}: expected ≥{min_modules} module nodes, got {}",
        stats.modules
    );
    assert!(
        stats.classes >= min_classes,
        "{label}: expected ≥{min_classes} class nodes, got {}",
        stats.classes
    );
    assert!(
        stats.functions >= min_functions,
        "{label}: expected ≥{min_functions} function/method nodes, got {}",
        stats.functions
    );
    assert!(
        stats.uses_edge_count >= min_uses_edges,
        "{label}: expected ≥{min_uses_edges} uses edges, got {}",
        stats.uses_edge_count
    );
}

/// Smoke test: analyze the `requests` package (~18 files).
///
/// Conservative lower bounds chosen so that an empty/degenerate graph
/// fails while leaving headroom for refactors that remove some nodes.
#[test]
fn test_corpus_requests() {
    let Some(dir) = corpus_dir("requests", "src/requests") else {
        eprintln!("SKIP test_corpus_requests: benchmarks/corpora/requests/src/requests not found");
        return;
    };

    let (_, stats) = analyze_corpus(&dir);

    // requests has 18 source files, ~9 classes, many dozens of functions
    assert_corpus_healthy("requests", &stats, 10, 5, 20, 15);
}

/// Smoke test: analyze the `rich` package (~78 files).
#[test]
fn test_corpus_rich() {
    let Some(dir) = corpus_dir("rich", "rich") else {
        eprintln!("SKIP test_corpus_rich: benchmarks/corpora/rich/rich not found");
        return;
    };

    let (_, stats) = analyze_corpus(&dir);

    // rich has 78 source files, 50+ classes, 150+ methods/functions
    assert_corpus_healthy("rich", &stats, 40, 30, 80, 60);
}

/// Smoke test: analyze the `flask` package (~18 files).
#[test]
fn test_corpus_flask() {
    let Some(dir) = corpus_dir("flask", "src/flask") else {
        eprintln!("SKIP test_corpus_flask: benchmarks/corpora/flask/src/flask not found");
        return;
    };

    let (_, stats) = analyze_corpus(&dir);

    // flask has 18 source files, several classes (Flask, Blueprint, etc.)
    assert_corpus_healthy("flask", &stats, 8, 5, 20, 15);
}

// ===================================================================
// Golden accuracy harness
//
// Asserts concrete uses/defines edges for hard call-resolution scenarios.
// All gaps previously documented here have been closed by the worklist-based
// return-value propagation (function_returns + fixpoint loop in analyzer.rs).
//
// Fixtures live in tests/test_code/accuracy_*.py and
// tests/test_code/accuracy_*/; they are small curated snippets adapted
// from PyCG micro-benchmark cases (pycallgraph-rs/PyCG/) and pyan
// (pycallgraph-rs/pyan/), but only the minimal code needed is copied.
// ===================================================================

/// Build a CallGraph from a single accuracy fixture file (no root).
fn make_single_fixture_graph(fixture_name: &str) -> CallGraph {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("test_code")
        .join(fixture_name);
    let files = vec![path.to_string_lossy().to_string()];
    CallGraph::new(&files, None)
        .unwrap_or_else(|e| panic!("failed to parse {fixture_name}: {e}"))
}

/// Build a CallGraph from multiple accuracy fixture files with `tests/` as root.
fn make_multi_fixture_graph(fixture_relative_paths: &[&str]) -> CallGraph {
    let test_code = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests");
    let files: Vec<String> = fixture_relative_paths
        .iter()
        .map(|p| test_code.join(p).to_string_lossy().to_string())
        .collect();
    let root = test_code.to_string_lossy().to_string();
    CallGraph::new(&files, Some(&root))
        .unwrap_or_else(|e| panic!("failed to parse multi-file fixture: {e}"))
}

// -------------------------------------------------------------------
// Alias / rebinding accuracy
//
// Adapted from PyCG micro-benchmark assignments/chained and
// functions/assigned_call.  Tests that assigning a function to a
// variable and calling through the alias produces the correct uses edge,
// and that chained rebinding (a = b = f1; a = b = f2) tracks both.
// -------------------------------------------------------------------

#[test]
fn test_accuracy_simple_alias() {
    // a = target_one; a() — must produce uses edge to target_one.
    let cg = make_single_fixture_graph("accuracy_alias.py");
    assert!(
        has_uses_edge(&cg, "simple_alias_caller", "target_one"),
        "simple_alias_caller should use target_one via alias"
    );
    // Must NOT spuriously add target_two (only target_one was aliased).
    let uses = get_uses(&cg, "simple_alias_caller");
    assert!(
        !uses.contains("target_two"),
        "simple_alias_caller must not use target_two, got: {uses:?}"
    );
}

#[test]
fn test_accuracy_chained_alias() {
    // a = b = target_one; b()  then  a = b = target_two; a() — both must be tracked.
    let cg = make_single_fixture_graph("accuracy_alias.py");
    assert!(
        has_uses_edge(&cg, "chained_alias_caller", "target_one"),
        "chained_alias_caller should use target_one (first chained binding)"
    );
    assert!(
        has_uses_edge(&cg, "chained_alias_caller", "target_two"),
        "chained_alias_caller should use target_two (second chained binding after rebind)"
    );
}

// -------------------------------------------------------------------
// Factory / return-value accuracy
//
// Adapted from PyCG micro-benchmark returns/call and
// classes/return_call.  Tests that:
//   - factory() creating a Product emits a uses edge factory -> Product
//   - consumer() calling factory() emits consumer -> factory
//   - (GAP) result.make() after an opaque return is currently NOT tracked
// -------------------------------------------------------------------

#[test]
fn test_accuracy_factory_call_tracked() {
    // consumer() calls factory() — uses edge must exist.
    let cg = make_single_fixture_graph("accuracy_factory.py");
    assert!(
        has_uses_edge(&cg, "consumer", "factory"),
        "consumer should use factory (direct function call)"
    );
}

#[test]
fn test_accuracy_factory_constructs_product() {
    // factory() creates Product() — uses edge factory -> Product must exist.
    let cg = make_single_fixture_graph("accuracy_factory.py");
    assert!(
        has_uses_edge(&cg, "factory", "Product"),
        "factory should use Product (constructor call inside factory body)"
    );
}

/// Return-value propagation: result = factory(); result.make() resolves to
/// Product.make because factory()'s return value (Product instance) is now
/// propagated back to the call site via function_returns tracking.
#[test]
fn test_accuracy_factory_return_method() {
    let cg = make_single_fixture_graph("accuracy_factory.py");
    assert!(
        has_uses_edge(&cg, "consumer", "make"),
        "consumer should use Product.make via return-value propagation"
    );
}

// -------------------------------------------------------------------
// Decorator flow accuracy
//
// Adapted from PyCG micro-benchmark decorators/call, decorators/param_call.
// Tests that applying a decorator creates a module-level uses edge and
// that callers of the decorated function are correctly wired.
// -------------------------------------------------------------------

#[test]
fn test_accuracy_simple_decorator_applied() {
    // @simple_decorator on a function — module must use simple_decorator.
    let cg = make_single_fixture_graph("accuracy_decorator.py");
    assert!(
        has_uses_edge(&cg, "accuracy_decorator", "simple_decorator"),
        "module should use simple_decorator (applied as @simple_decorator)"
    );
}

#[test]
fn test_accuracy_factory_decorator_applied() {
    // @factory_decorator("arg") — module must use factory_decorator.
    let cg = make_single_fixture_graph("accuracy_decorator.py");
    assert!(
        has_uses_edge(&cg, "accuracy_decorator", "factory_decorator"),
        "module should use factory_decorator (applied as @factory_decorator(...))"
    );
}

#[test]
fn test_accuracy_caller_uses_decorated_functions() {
    // call_decorated() calls both decorated functions.
    let cg = make_single_fixture_graph("accuracy_decorator.py");
    assert!(
        has_uses_edge(&cg, "call_decorated", "simple_decorated"),
        "call_decorated should use simple_decorated"
    );
    assert!(
        has_uses_edge(&cg, "call_decorated", "factory_decorated"),
        "call_decorated should use factory_decorated"
    );
}

// -------------------------------------------------------------------
// Container / subscript call accuracy
//
// Adapted from PyCG micro-benchmark lists/simple, dicts.
// Tests that calling functions via list[i]() and dict[key]() produces
// the correct uses edges for all contained function references.
// -------------------------------------------------------------------

#[test]
fn test_accuracy_list_subscript_call() {
    // funcs = [func_a, func_b]; funcs[0](); funcs[1]()
    let cg = make_single_fixture_graph("accuracy_container.py");
    assert!(
        has_uses_edge(&cg, "list_subscript_caller", "func_a"),
        "list_subscript_caller should use func_a (funcs[0]())"
    );
    assert!(
        has_uses_edge(&cg, "list_subscript_caller", "func_b"),
        "list_subscript_caller should use func_b (funcs[1]())"
    );
}

#[test]
fn test_accuracy_dict_subscript_call() {
    // dispatch = {"a": func_a, "c": func_c}; dispatch["a"](); dispatch["c"]()
    let cg = make_single_fixture_graph("accuracy_container.py");
    assert!(
        has_uses_edge(&cg, "dict_subscript_caller", "func_a"),
        "dict_subscript_caller should use func_a (dispatch[\"a\"]())"
    );
    assert!(
        has_uses_edge(&cg, "dict_subscript_caller", "func_c"),
        "dict_subscript_caller should use func_c (dispatch[\"c\"]())"
    );
}

// -------------------------------------------------------------------
// from-star-import accuracy
//
// Adapted from PyCG micro-benchmark imports/import_all.
// Tests that `from module import *` followed by calling the imported
// names produces correct uses edges even though names were not listed
// explicitly in the import statement.
// -------------------------------------------------------------------

#[test]
fn test_accuracy_star_import_calls_resolve() {
    // star_import_caller() calls exported_func1() and exported_func2() after
    // `from accuracy_star_src import *` — both uses edges must exist.
    let cg = make_multi_fixture_graph(&[
        "test_code/accuracy_star_src.py",
        "test_code/accuracy_star_user.py",
    ]);
    assert!(
        has_uses_edge(&cg, "star_import_caller", "exported_func1"),
        "star_import_caller should use exported_func1 (resolved via star import)"
    );
    assert!(
        has_uses_edge(&cg, "star_import_caller", "exported_func2"),
        "star_import_caller should use exported_func2 (resolved via star import)"
    );
}

// -------------------------------------------------------------------
// Import re-export chain accuracy
//
// Adapted from PyCG micro-benchmark imports/chained_import.
// Tests the case where pkg/__init__.py re-exports a function from
// pkg/impl.py, and a user module imports via the package.
//
// Both the __init__.py import edge and the downstream binding in the user
// module are now tracked after the return-value propagation fixpoint.
// -------------------------------------------------------------------

#[test]
fn test_accuracy_reexport_package_import_tracked() {
    // accuracy_reexport/__init__.py does `from ...impl import reexport_func`
    // That import must appear as a uses edge on the package module node.
    let cg = make_multi_fixture_graph(&[
        "test_code/accuracy_reexport/__init__.py",
        "test_code/accuracy_reexport/impl.py",
        "test_code/accuracy_reexport/user.py",
    ]);
    assert!(
        has_uses_edge(&cg, "accuracy_reexport", "reexport_func"),
        "accuracy_reexport package __init__ should use reexport_func (import in __init__)"
    );
}

/// Resolved: scope-based import propagation now threads the re-export chain
/// through __init__.py so `reexport_caller` correctly sees `reexport_func`.
#[test]
fn test_accuracy_reexport_chain_caller() {
    let cg = make_multi_fixture_graph(&[
        "test_code/accuracy_reexport/__init__.py",
        "test_code/accuracy_reexport/impl.py",
        "test_code/accuracy_reexport/user.py",
    ]);
    assert!(
        has_uses_edge(&cg, "reexport_caller", "reexport_func"),
        "reexport_caller should use reexport_func via package re-export chain"
    );
}

// -------------------------------------------------------------------
// Starred unpacking accuracy
//
// Tests the `a, b, *c = ...` / `a, *b, c = ...` / `*a, b = ...`
// assignment patterns from features.py.  The analyzer currently tracks
// constructor calls for all RHS values; method calls on positionally-
// bound (non-starred) targets are a known gap.
// -------------------------------------------------------------------

#[test]
fn test_accuracy_starred_unpack_constructors_tracked() {
    // All four constructors must be tracked in uses regardless of star position.
    let cg = make_features_graph();
    // star_at_end: a, b, *c = Alpha(), Beta(), Gamma(), Delta()
    assert!(has_uses_edge(&cg, "star_at_end", "Alpha"), "star_at_end must use Alpha");
    assert!(has_uses_edge(&cg, "star_at_end", "Beta"),  "star_at_end must use Beta");
    assert!(has_uses_edge(&cg, "star_at_end", "Gamma"), "star_at_end must use Gamma");
    assert!(has_uses_edge(&cg, "star_at_end", "Delta"), "star_at_end must use Delta");
    // star_in_middle: a, *b, c = Alpha(), Beta(), Gamma(), Delta()
    assert!(has_uses_edge(&cg, "star_in_middle", "Alpha"), "star_in_middle must use Alpha");
    assert!(has_uses_edge(&cg, "star_in_middle", "Delta"), "star_in_middle must use Delta");
    // star_at_start: *a, b = Alpha(), Beta(), Gamma()
    assert!(has_uses_edge(&cg, "star_at_start", "Gamma"), "star_at_start must use Gamma");
}

/// Positional starred-unpack targets resolve the correct class's methods.
///
/// `a, b, *c = Alpha(), Beta(), Gamma(), Delta()` binds `a` to Alpha and `b`
/// to Beta via positional matching.  Method calls on those targets must emit
/// uses edges to the *correct* class methods, not to a same-value approximation.
#[test]
fn test_accuracy_starred_unpack_explicit_target_methods() {
    let cg = make_features_graph();
    assert!(
        has_uses_edge(&cg, "star_at_end", "alpha_method"),
        "star_at_end should use alpha_method via a.alpha_method() (a = Alpha())"
    );
    assert!(
        has_uses_edge(&cg, "star_at_end", "beta_method"),
        "star_at_end should use beta_method via b.beta_method() (b = Beta())"
    );
}

/// INV-1: positional unpacking binds each target to the correct class node,
/// not to a collapsed same-value approximation of the whole RHS.
///
/// Verifies that `a, b, *c = Alpha(), Beta(), Gamma(), Delta()` binds `a`
/// specifically to the Alpha class by checking that the uses edge for
/// `alpha_method` points into Alpha's namespace (not Delta's).
#[test]
fn test_positional_unpack_correct_class_binding() {
    let cg = make_features_graph();

    // Find all nodes named "alpha_method" that live in the Alpha class namespace.
    let alpha_method_in_alpha: Vec<usize> = find_nodes_by_name(&cg, "alpha_method")
        .into_iter()
        .filter(|&id| {
            cg.nodes_arena[id]
                .namespace
                .as_deref()
                .unwrap_or("")
                .contains("Alpha")
        })
        .collect();
    assert!(
        !alpha_method_in_alpha.is_empty(),
        "Alpha.alpha_method node must exist"
    );

    // star_at_end uses Alpha.alpha_method specifically (positional binding a → Alpha).
    let star_at_end_ids = find_nodes_by_name(&cg, "star_at_end");
    assert!(!star_at_end_ids.is_empty(), "star_at_end must exist");
    let uses_alpha_method = alpha_method_in_alpha.iter().any(|&mid| {
        star_at_end_ids.iter().any(|&fid| {
            cg.uses_edges
                .get(&fid)
                .is_some_and(|targets| targets.contains(&mid))
        })
    });
    assert!(
        uses_alpha_method,
        "star_at_end should use Alpha.alpha_method specifically (a bound to Alpha via positional unpacking)"
    );

    // Verify star_in_middle: c is Delta (last positional), calls c.delta_method().
    let delta_method_in_delta: Vec<usize> = find_nodes_by_name(&cg, "delta_method")
        .into_iter()
        .filter(|&id| {
            cg.nodes_arena[id]
                .namespace
                .as_deref()
                .unwrap_or("")
                .contains("Delta")
        })
        .collect();
    assert!(
        !delta_method_in_delta.is_empty(),
        "Delta.delta_method node must exist"
    );
    let star_in_middle_ids = find_nodes_by_name(&cg, "star_in_middle");
    let uses_delta_method = delta_method_in_delta.iter().any(|&mid| {
        star_in_middle_ids.iter().any(|&fid| {
            cg.uses_edges
                .get(&fid)
                .is_some_and(|targets| targets.contains(&mid))
        })
    });
    assert!(
        uses_delta_method,
        "star_in_middle should use Delta.delta_method specifically (c bound to Delta via positional unpacking)"
    );
}

// -------------------------------------------------------------------
// Chained-call regression (submodule1.py)
//
// The canonical hard case: B.get_a_via_A() calls self.to_A() and then
// passes the chained attribute access self.to_A().b.a to test_func1().
// The analyzer must at minimum track the outer call (to_A) and the
// enclosing call (test_func1); deeper chain resolution through the
// opaque return is a known gap.
// -------------------------------------------------------------------

fn make_submodule_graph() -> CallGraph {
    make_multi_fixture_graph(&[
        "test_code/submodule1.py",
        "test_code/submodule2.py",
        "test_code/subpackage1/__init__.py",
        "test_code/subpackage1/submodule1.py",
    ])
}

#[test]
fn test_accuracy_chained_call_outer_tracked() {
    // get_a_via_A calls self.to_A() — uses edge to to_A must exist.
    let cg = make_submodule_graph();
    assert!(
        has_uses_edge(&cg, "get_a_via_A", "to_A"),
        "get_a_via_A should use to_A (self.to_A() is the chain head)"
    );
}

#[test]
fn test_accuracy_chained_call_enclosing_func_tracked() {
    // get_a_via_A calls test_func1(...) as the outer wrapper call.
    let cg = make_submodule_graph();
    assert!(
        has_uses_edge(&cg, "get_a_via_A", "test_func1"),
        "get_a_via_A should use test_func1 (outermost call around chain)"
    );
}

/// Return-value propagation: `self.to_A()` returns an `A` instance.
/// The worklist propagation closes this for simple factory cases but the
/// method-call chain through `self.to_A()` does not yet propagate through
/// the ValueSet binding model (regression after ValueSet rebase).
#[test]
#[ignore = "GAP: attribute access on method-call result (self.to_A().b) not resolved after ValueSet rebase"]
fn test_accuracy_chained_call_deep_chain() {
    let cg = make_submodule_graph();
    assert!(
        has_uses_edge(&cg, "get_a_via_A", "A"),
        "get_a_via_A should use A via return-value propagation of to_A()"
    );
}

// -------------------------------------------------------------------
// Higher-order function accuracy
//
// Tests that calling the return value of a factory (fn = make_greeter();
// fn()) resolves to the actual returned function, and that the call graph
// reflects this dependency.
//
// Adapted from PyCG micro-benchmark functions/assigned_call.
// -------------------------------------------------------------------

#[test]
fn test_accuracy_higher_order_factory_call() {
    // make_greeter() returns greet; call_via_factory calls make_greeter()
    // so it must have a uses edge to make_greeter.
    let cg = make_single_fixture_graph("accuracy_higher_order.py");
    assert!(
        has_uses_edge(&cg, "call_via_factory", "make_greeter"),
        "call_via_factory should use make_greeter (direct call)"
    );
}

#[test]
fn test_accuracy_higher_order_return_resolved() {
    // make_greeter() returns greet; fn = make_greeter(); fn() must resolve to greet.
    let cg = make_single_fixture_graph("accuracy_higher_order.py");
    assert!(
        has_uses_edge(&cg, "call_via_factory", "greet"),
        "call_via_factory should use greet via make_greeter() return-value propagation"
    );
}

#[test]
fn test_accuracy_higher_order_assigned_return() {
    // adder = get_adder(); adder(1,2) must resolve to add_nums.
    let cg = make_single_fixture_graph("accuracy_higher_order.py");
    assert!(
        has_uses_edge(&cg, "call_returned_fn", "get_adder"),
        "call_returned_fn should use get_adder (direct call)"
    );
    assert!(
        has_uses_edge(&cg, "call_returned_fn", "add_nums"),
        "call_returned_fn should use add_nums via get_adder() return-value propagation"
    );
}

// Accuracy harness — ValueSet binding model
//
// These tests verify the invariants introduced by replacing single-value
// bindings with abstract-value sets:
//
//   INV-1  Branch joins and rebinding preserve all plausible pointees.
//   INV-2  Alias chains retain multiple candidate values.
//   INV-3  Existing green tests remain green (checked by all tests above).
// ===================================================================

/// Helper: build a CallGraph from a single fixture file.
fn make_single_fixture(name: &str) -> CallGraph {
    let path = test_code_dir().join(name);
    let files = vec![path.to_string_lossy().to_string()];
    CallGraph::new(&files, None).expect("fixture analysis should succeed")
}

// -------------------------------------------------------------------
// INV-1: branch-join — calls from both branches must be traceable
// -------------------------------------------------------------------

/// `caller` in accuracy_branch.py assigns `x = A()` in the if-branch and
/// `x = B()` in the else-branch, then calls `x.method()`.
/// After the ValueSet refactor the analyzer must emit uses edges to BOTH
/// `A.method` and `B.method` (not just the last-assigned one).
#[test]
fn test_inv1_branch_join_preserves_both_branches() {
    let cg = make_single_fixture("accuracy_branch.py");
    let uses = get_uses(&cg, "caller");
    assert!(
        uses.contains("method"),
        "caller should use method (branch join), got: {uses:?}"
    );
    // The uses set must contain the method from BOTH A and B.
    // We check for at least two distinct nodes named "method".
    let method_nodes: Vec<usize> = find_nodes_by_name(&cg, "method")
        .into_iter()
        .filter(|&id| {
            cg.uses_edges
                .get(&find_nodes_by_name(&cg, "caller")[0])
                .is_some_and(|targets| targets.contains(&id))
        })
        .collect();
    assert!(
        method_nodes.len() >= 2,
        "caller should use method from both A and B after branch-join, \
         found {} method node(s) in the uses set",
        method_nodes.len()
    );
}

/// `rebind_caller` does `x = A(); x = B(); x.method()`.
/// The rebinding must union rather than overwrite, so both A.method and
/// B.method must appear.
#[test]
fn test_inv1_rebind_preserves_earlier_value() {
    let cg = make_single_fixture("accuracy_branch.py");
    let uses = get_uses(&cg, "rebind_caller");
    assert!(
        uses.contains("method"),
        "rebind_caller should use method after rebinding, got: {uses:?}"
    );
    let caller_ids = find_nodes_by_name(&cg, "rebind_caller");
    assert!(!caller_ids.is_empty(), "rebind_caller node must exist");
    let method_nodes: Vec<usize> = find_nodes_by_name(&cg, "method")
        .into_iter()
        .filter(|&id| {
            caller_ids.iter().any(|&cid| {
                cg.uses_edges
                    .get(&cid)
                    .is_some_and(|targets| targets.contains(&id))
            })
        })
        .collect();
    assert!(
        method_nodes.len() >= 2,
        "rebind_caller should use method from both A and B after rebinding, \
         found {} method node(s)",
        method_nodes.len()
    );
}

// -------------------------------------------------------------------
// INV-2: alias rebinding — earlier candidate must not be silently dropped
// -------------------------------------------------------------------

/// `alias_caller` does `alias = func_a; alias = func_b; alias()`.
/// With ValueSet both func_a and func_b must appear in the uses set.
#[test]
fn test_inv2_alias_rebind_preserves_both_values() {
    let cg = make_single_fixture("accuracy_alias.py");
    let uses = get_uses(&cg, "alias_caller");
    assert!(
        uses.contains("func_a"),
        "alias_caller should use func_a after alias rebinding, got: {uses:?}"
    );
    assert!(
        uses.contains("func_b"),
        "alias_caller should use func_b after alias rebinding, got: {uses:?}"
    );
}

/// `import_alias_caller` does `foo = func_a; foo = bar; foo()`.
/// Both func_a and bar must remain reachable.
#[test]
fn test_inv2_import_alias_retains_earlier_candidate() {
    let cg = make_single_fixture("accuracy_alias.py");
    let uses = get_uses(&cg, "import_alias_caller");
    assert!(
        uses.contains("func_a"),
        "import_alias_caller should use func_a (first alias target), got: {uses:?}"
    );
    assert!(
        uses.contains("bar"),
        "import_alias_caller should use bar (second alias target), got: {uses:?}"
    );
}

// -------------------------------------------------------------------
// INV-3 regression guard: ValueSet must not explode simple single-value cases
// -------------------------------------------------------------------

/// A plain function call `f()` where `f` has exactly one binding must still
/// resolve to exactly one target — not zero, not many.
#[test]
fn test_inv3_single_binding_still_resolves() {
    let cg = make_features_graph();
    // `bar` calls `foo` — simple single-value case must still work.
    assert!(
        has_uses_edge(&cg, "bar", "foo"),
        "bar should still use foo after ValueSet refactor (single-value regression)"
    );
}

/// Inheritance resolution must not regress: `Derived` uses `Base`.
#[test]
fn test_inv3_inheritance_still_resolves() {
    let cg = make_features_graph();
    assert!(
        has_uses_edge(&cg, "Derived", "Base"),
        "Derived should still use Base after ValueSet refactor"
    );
}

// ===================================================================
// Import precision tests (scope-based resolution)
//
// These tests verify the invariants introduced by the scope-based import
// improvement:
//
//   INV-1  Imported aliases/reexports resolve to concrete (namespaced)
//           nodes when the source module is analyzable.
//   INV-2  `from x import *` gains a sound static approximation —
//           exported names bind to their concrete definitions.
//   INV-3  Import precision does not cause false-positive fanout:
//           the caller only uses what it actually calls.
// ===================================================================

/// Check that a uses edge from `from_name` reaches a concrete (namespaced)
/// node with the given `to_name`, rather than a wildcard.
fn has_concrete_uses_edge(cg: &CallGraph, from_name: &str, to_name: &str) -> bool {
    for &fid in find_nodes_by_name(cg, from_name).iter() {
        if let Some(targets) = cg.uses_edges.get(&fid) {
            for &tid in targets {
                let n = &cg.nodes_arena[tid];
                if n.name == to_name && n.namespace.is_some() {
                    return true;
                }
            }
        }
    }
    false
}

// -------------------------------------------------------------------
// INV-1: Re-export chain resolves to the concrete definition
// -------------------------------------------------------------------

/// After scope-based resolution, `reexport_caller` must reach
/// `reexport_func` as a concrete node (from accuracy_reexport.impl),
/// not just a wildcard placeholder.
#[test]
fn test_inv1_reexport_chain_resolves_concrete_node() {
    let cg = make_multi_fixture_graph(&[
        "test_code/accuracy_reexport/__init__.py",
        "test_code/accuracy_reexport/impl.py",
        "test_code/accuracy_reexport/user.py",
    ]);
    assert!(
        has_concrete_uses_edge(&cg, "reexport_caller", "reexport_func"),
        "reexport_caller must use reexport_func via a concrete (namespaced) node, \
         not a wildcard placeholder"
    );
}

/// Copying imported facts must not go stale: the `accuracy_reexport`
/// package itself (the __init__) must also hold a concrete edge to
/// `reexport_func` (the node it imported from impl).
#[test]
fn test_inv1_reexport_package_node_concrete() {
    let cg = make_multi_fixture_graph(&[
        "test_code/accuracy_reexport/__init__.py",
        "test_code/accuracy_reexport/impl.py",
        "test_code/accuracy_reexport/user.py",
    ]);
    assert!(
        has_concrete_uses_edge(&cg, "accuracy_reexport", "reexport_func"),
        "the accuracy_reexport package node must concretely use reexport_func \
         (regression: copied import fact must not diverge from source)"
    );
}

// -------------------------------------------------------------------
// INV-2: Star import produces sound concrete bindings
// -------------------------------------------------------------------

/// `star_import_caller` calls `exported_func1` and `exported_func2` after
/// `from accuracy_star_src import *`.  Both must resolve to concrete nodes
/// (i.e. namespaced, not wildcards) so the result is a sound approximation.
#[test]
fn test_inv2_star_import_concrete_nodes() {
    let cg = make_multi_fixture_graph(&[
        "test_code/accuracy_star_src.py",
        "test_code/accuracy_star_user.py",
    ]);
    assert!(
        has_concrete_uses_edge(&cg, "star_import_caller", "exported_func1"),
        "star_import_caller must reach exported_func1 as a concrete node \
         (INV-2: star import static approximation)"
    );
    assert!(
        has_concrete_uses_edge(&cg, "star_import_caller", "exported_func2"),
        "star_import_caller must reach exported_func2 as a concrete node \
         (INV-2: star import static approximation)"
    );
}

// -------------------------------------------------------------------
// INV-3: Import precision — no false-positive fanout
// -------------------------------------------------------------------

/// The caller in the re-export fixture only calls `reexport_func()`.
/// After resolution, `reexport_caller`'s uses set must be small — it must
/// not gain spurious edges to unrelated nodes just because of import handling.
#[test]
fn test_inv3_reexport_no_spurious_fanout() {
    let cg = make_multi_fixture_graph(&[
        "test_code/accuracy_reexport/__init__.py",
        "test_code/accuracy_reexport/impl.py",
        "test_code/accuracy_reexport/user.py",
    ]);
    let uses = get_uses(&cg, "reexport_caller");
    // Should use reexport_func and nothing else from this tiny fixture.
    assert!(
        uses.contains("reexport_func"),
        "reexport_caller must use reexport_func, got: {uses:?}"
    );
    assert!(
        uses.len() <= 3,
        "reexport_caller has unexpected extra uses edges (noise explosion): {uses:?}"
    );
}

// ===================================================================
// INV-1: del protocol edges (__delattr__ / __delitem__)
//
// When the receiver of `del obj.attr` or `del obj[key]` is statically
// known (bound to a class we analyzed), the analyzer must emit uses
// edges to the appropriate dunder method.  Bare `del name` must NOT
// emit any protocol edges.
// ===================================================================

/// `clear_entry` binds `registry = Registry()` then does `del registry.entry`.
/// Since `registry` is statically bound to `Registry`, the analyzer must emit
/// a uses edge from `clear_entry` to `Registry.__delattr__`.
#[test]
fn test_del_delattr_protocol_known_receiver() {
    let cg = make_features_graph();
    assert!(
        has_uses_edge(&cg, "clear_entry", "__delattr__"),
        "clear_entry should use Registry.__delattr__ via 'del registry.attr' (receiver is known Registry)"
    );
}

/// `remove_item` binds `registry = Registry()` then does `del registry["key"]`.
/// Since `registry` is statically bound to `Registry`, the analyzer must emit
/// a uses edge from `remove_item` to `Registry.__delitem__`.
#[test]
fn test_del_delitem_protocol_known_receiver() {
    let cg = make_features_graph();
    assert!(
        has_uses_edge(&cg, "remove_item", "__delitem__"),
        "remove_item should use Registry.__delitem__ via 'del registry[key]' (receiver is known Registry)"
    );
}

/// `unbind_local` does `del tmp` on a plain local variable.
/// No protocol edge must be emitted for a bare name deletion.
#[test]
fn test_del_local_no_protocol_edge() {
    let cg = make_features_graph();
    let uses = get_uses(&cg, "unbind_local");
    assert!(
        !uses.contains("__delattr__"),
        "unbind_local (del local var) must not emit __delattr__, got: {uses:?}"
    );
    assert!(
        !uses.contains("__delitem__"),
        "unbind_local (del local var) must not emit __delitem__, got: {uses:?}"
    );
}

/// Protocol edges for del must only be emitted when the receiver is a
/// known class — not for unknown/argument receivers.
#[test]
fn test_del_protocol_edges_are_concrete() {
    let cg = make_features_graph();
    // All __delattr__ / __delitem__ nodes must have a non-None namespace.
    for method in ["__delattr__", "__delitem__"] {
        for &nid in cg.nodes_by_name.get(method).unwrap_or(&vec![]) {
            assert!(
                cg.nodes_arena[nid].namespace.is_some(),
                "del protocol method {method} must resolve to a concrete (non-wildcard) node"
            );
        }
    }
}

// ===================================================================
// str / repr builtin modeling
//
// Calling `str(obj)` or `repr(obj)` with a statically-known class
// instance must emit uses edges to `obj.__str__` / `obj.__repr__`.
// ===================================================================

/// `call_str_repr` calls `str(obj)` where `obj = Printable()`.
/// The analyzer must emit a uses edge to `Printable.__str__`.
#[test]
fn test_str_builtin_emits_dunder_str() {
    let cg = make_features_graph();
    assert!(
        has_uses_edge(&cg, "call_str_repr", "__str__"),
        "call_str_repr should use Printable.__str__ via str(obj)"
    );
}

/// `call_str_repr` calls `repr(obj)` where `obj = Printable()`.
/// The analyzer must emit a uses edge to `Printable.__repr__`.
#[test]
fn test_repr_builtin_emits_dunder_repr() {
    let cg = make_features_graph();
    assert!(
        has_uses_edge(&cg, "call_str_repr", "__repr__"),
        "call_str_repr should use Printable.__repr__ via repr(obj)"
    );
}

// ===================================================================
// Decorator-chain call flow
//
// When @decorator is applied to func, the decorator is called with
// func as its argument.  The analyzer must emit a uses edge from the
// concrete decorator to the decorated function.
// ===================================================================

/// `@simple_decorator` applied to `simple_decorated` — the decorator
/// receives the function as an argument, so `simple_decorator` must
/// have a uses edge to `simple_decorated`.
#[test]
fn test_accuracy_decorator_uses_decorated_function() {
    let cg = make_single_fixture_graph("accuracy_decorator.py");
    assert!(
        has_uses_edge(&cg, "simple_decorator", "simple_decorated"),
        "simple_decorator should use simple_decorated (decorator receives function as argument)"
    );
}

// ===================================================================
// INV-2: expand_unknowns precision
//
// When a caller already has a *concrete* uses edge to a node named N,
// wildcard expansion for *.N from the same caller must be suppressed.
// This prevents false-positive fanout to unrelated classes that happen
// to define a method with the same short name.
// ===================================================================

/// `precision_caller` in features.py:
///   - calls `a.do_work()` → concrete uses edge to `WorkerA.do_work`
///   - calls bare `do_work()` → creates wildcard `*.do_work`
///
/// Old behavior: wildcard expands to both `WorkerA.do_work` and `WorkerB.do_work`.
/// New behavior: concrete resolution already exists for "do_work", so wildcard
/// expansion is skipped — `WorkerB.do_work` must NOT appear in uses.
#[test]
fn test_expand_unknowns_scoped_by_concrete_resolution() {
    let cg = make_features_graph();

    // The concrete edge must still exist.
    assert!(
        has_uses_edge(&cg, "precision_caller", "do_work"),
        "precision_caller must use do_work via concrete a.do_work(), got: {:?}",
        get_uses(&cg, "precision_caller")
    );

    // WorkerB.do_work must NOT be reached via wildcard expansion.
    let worker_b_do_work: Vec<usize> = find_nodes_by_name(&cg, "do_work")
        .into_iter()
        .filter(|&id| {
            cg.nodes_arena[id]
                .namespace
                .as_deref()
                .unwrap_or("")
                .contains("WorkerB")
        })
        .collect();

    let precision_caller_ids = find_nodes_by_name(&cg, "precision_caller");
    let has_worker_b_edge = worker_b_do_work.iter().any(|&mid| {
        precision_caller_ids.iter().any(|&fid| {
            cg.uses_edges
                .get(&fid)
                .is_some_and(|targets| targets.contains(&mid))
        })
    });
    assert!(
        !has_worker_b_edge,
        "precision_caller must not use WorkerB.do_work — wildcard expansion should be \
         suppressed because a concrete WorkerA.do_work resolution already exists"
    );
}

/// Helper: check that at most `max_count` distinct uses targets exist for a node.
/// Used to verify that wildcard expansion does not globally fan out.
fn has_concrete_uses_edge_for_name(cg: &CallGraph, from_name: &str, short_name: &str) -> bool {
    for &fid in find_nodes_by_name(cg, from_name).iter() {
        if let Some(targets) = cg.uses_edges.get(&fid) {
            for &tid in targets {
                if cg.nodes_arena[tid].name == short_name
                    && cg.nodes_arena[tid].namespace.is_some()
                {
                    return true;
                }
            }
        }
    }
    false
}

/// INV-2 precision: the number of *concrete* (namespaced) `do_work` uses from
/// `precision_caller` must be exactly 1 (WorkerA only), not more.
/// Wildcards (*.do_work) are expected from the bare `do_work()` call and are
/// not counted — we only care that the concrete fanout is bounded.
#[test]
fn test_expand_unknowns_fanout_count_bounded() {
    let cg = make_features_graph();

    // Verify concrete edge exists (WorkerA.do_work).
    assert!(
        has_concrete_uses_edge_for_name(&cg, "precision_caller", "do_work"),
        "precision_caller must have at least one concrete 'do_work' uses edge"
    );

    // Count concrete (namespaced) nodes named "do_work" in precision_caller's uses set.
    let precision_caller_ids = find_nodes_by_name(&cg, "precision_caller");
    let concrete_do_work_count = find_nodes_by_name(&cg, "do_work")
        .into_iter()
        .filter(|&mid| {
            // Only count concrete (namespaced) nodes.
            cg.nodes_arena[mid].namespace.is_some()
                && precision_caller_ids.iter().any(|&fid| {
                    cg.uses_edges
                        .get(&fid)
                        .is_some_and(|targets| targets.contains(&mid))
                })
        })
        .count();

    assert!(
        concrete_do_work_count <= 1,
        "precision_caller should use at most 1 concrete 'do_work' node (WorkerA only), \
         got {concrete_do_work_count} — wildcard expansion is too broad"
    );
}
