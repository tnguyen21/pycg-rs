use crate::common::*;

// Core analysis tests
// ===================================================================

#[test]
fn test_modules_found() {
    let cg = make_call_graph(&test_code_dir());
    let module_names: Vec<_> = cg
        .nodes_arena
        .iter()
        .filter(|n| n.flavor == pycg_rs::node::Flavor::Module)
        .map(|n| n.get_name())
        .collect();
    assert!(
        module_names.iter().any(|n| n.contains("submodule1")),
        "submodule1 not found"
    );
    assert!(
        module_names.iter().any(|n| n.contains("submodule2")),
        "submodule2 not found"
    );
}

#[test]
fn test_class_found() {
    let cg = make_call_graph(&test_code_dir());
    let classes: Vec<_> = cg
        .nodes_arena
        .iter()
        .filter(|n| n.flavor == pycg_rs::node::Flavor::Class)
        .map(|n| n.name.clone())
        .collect();
    assert!(
        classes.contains(&"A".to_string()),
        "Class A not found, got: {:?}",
        classes
    );
}

#[test]
fn test_function_found() {
    let cg = make_call_graph(&test_code_dir());
    let functions: Vec<_> = cg
        .nodes_arena
        .iter()
        .filter(|n| {
            matches!(
                n.flavor,
                pycg_rs::node::Flavor::Function | pycg_rs::node::Flavor::Method
            )
        })
        .map(|n| n.name.clone())
        .collect();
    assert!(
        functions.contains(&"test_func1".to_string()),
        "test_func1 not found, got: {:?}",
        functions
    );
}

#[test]
fn test_submodule_defines() {
    let cg = make_call_graph(&test_code_dir());
    let defs = get_defines(&cg, "submodule2");
    assert!(
        defs.contains("test_2"),
        "submodule2 should define test_2, got: {:?}",
        defs
    );
}

#[test]
fn test_uses_edge_exists() {
    let cg = make_call_graph(&test_code_dir());
    let uses = get_uses(&cg, "test_2");
    assert!(
        uses.contains("test_func1") || uses.contains("test_func2"),
        "test_2 should use test_func1 or test_func2, got: {:?}",
        uses
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
        &cg.nodes_arena,
        &cg.defined,
        &cg.defines_edges,
        &cg.uses_edges,
        &opts,
    );
    let dot = writer::write_dot(&vg, &["rankdir=TB".to_string()]);
    assert!(
        dot.starts_with("digraph G {"),
        "DOT output should start with 'digraph G {{'"
    );
    assert!(dot.trim().ends_with('}'), "DOT output should end with '}}'");
    assert!(dot.contains("->"), "DOT output should contain edges");
    assert!(
        dot.contains("style=\"dashed\""),
        "DOT output should have defines edges (dashed)"
    );
    assert!(
        dot.contains("style=\"solid\""),
        "DOT output should have uses edges (solid)"
    );
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
        &cg.nodes_arena,
        &cg.defined,
        &cg.defines_edges,
        &cg.uses_edges,
        &opts,
    );
    let dot = writer::write_dot(&vg, &["rankdir=TB".to_string()]);
    assert!(
        dot.contains("subgraph cluster_"),
        "Grouped DOT should have subgraphs"
    );
}

// ===================================================================
// Module-level dependency graph
// ===================================================================

#[test]
fn test_module_graph() {
    let cg = make_call_graph(&test_code_dir());
    let (mod_nodes, mod_uses, mod_defined) = cg.derive_module_graph();

    // Should have module nodes for analyzed files.
    assert!(
        mod_nodes.len() >= 5,
        "module graph should have several modules, got {}",
        mod_nodes.len()
    );
    assert_eq!(mod_defined.len(), mod_nodes.len());

    // Should have cross-module edges (submodule1 imports from subpackage1, etc.)
    let total_edges: usize = mod_uses.values().map(|s| s.len()).sum();
    assert!(
        total_edges >= 3,
        "module graph should have cross-module edges, got {total_edges}"
    );

    // Render through the full pipeline — should produce valid DOT.
    let opts = VisualOptions {
        draw_defines: false,
        draw_uses: true,
        colored: false,
        grouped: false,
        annotated: false,
    };
    let vg = VisualGraph::from_call_graph(
        &mod_nodes,
        &mod_defined,
        &std::collections::HashMap::new(),
        &mod_uses,
        &opts,
    );
    let dot = writer::write_dot(&vg, &["rankdir=TB".to_string()]);
    assert!(dot.starts_with("digraph G {"));
    assert!(dot.contains("->"), "module DOT should contain edges");
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
        &cg.nodes_arena,
        &cg.defined,
        &cg.defines_edges,
        &cg.uses_edges,
        &opts,
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
        &cg.nodes_arena,
        &cg.defined,
        &cg.defines_edges,
        &cg.uses_edges,
        &opts,
    );
    let text = writer::write_text(&vg);
    assert!(
        text.contains("[D]") || text.contains("[U]"),
        "Text should have tagged edges"
    );
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
/// crash the analyzer.  Beyond not crashing, verify the structure is correct.
#[test]
fn test_regression_annotated_assignments() {
    let cg = make_fixture_graph("regression_issue2.py");

    // Module defines both the function and the class
    assert!(
        has_defines_edge(&cg, "regression_issue2", "annotated_fn"),
        "issue2: module should define annotated_fn"
    );
    assert!(
        has_defines_edge(&cg, "regression_issue2", "Container"),
        "issue2: module should define Container"
    );
    // Container defines set_value method
    assert!(
        has_defines_edge(&cg, "Container", "set_value"),
        "issue2: Container should define set_value"
    );
}

/// Issue #3: complex / nested comprehensions (list-inside-list, dict-in-list,
/// generator-as-iterable) must not crash the analyzer.
#[test]
fn test_regression_comprehensions() {
    let cg = make_fixture_graph("regression_issue3.py");

    // Module defines all five functions
    let defs = get_defines(&cg, "regression_issue3");
    for name in &["f", "g", "h", "set_comp", "dict_comp"] {
        assert!(
            defs.contains(*name),
            "issue3: module should define {name}, got: {defs:?}"
        );
    }
}

/// Issue #5: files that reference external / uninstalled packages (numpy,
/// pandas) and relative imports whose targets don't exist must not crash.
/// The analyzer should still produce correct defines edges for local code.
#[test]
fn test_regression_external_deps() {
    let cg = make_fixture_graph("regression_issue5.py");

    // Module defines the class
    assert!(
        has_defines_edge(&cg, "regression_issue5", "MyProcessor"),
        "issue5: module should define MyProcessor"
    );
    // Class defines __init__ and process methods
    assert!(
        has_defines_edge(&cg, "MyProcessor", "__init__"),
        "issue5: MyProcessor should define __init__"
    );
    assert!(
        has_defines_edge(&cg, "MyProcessor", "process"),
        "issue5: MyProcessor should define process"
    );
}

// ===================================================================
