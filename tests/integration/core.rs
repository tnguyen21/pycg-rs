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
/// crash the analyzer.
#[test]
fn test_regression_annotated_assignments() {
    let fixture = test_code_dir().join("regression_issue2.py");
    let files = vec![fixture.to_string_lossy().to_string()];
    let cg = CallGraph::new(&files, None)
        .expect("issue2: annotated assignment must not crash the analyzer");
    // The file defines annotated_fn and Container – verify we produced nodes.
    assert!(
        !cg.nodes_arena.is_empty(),
        "issue2: graph should not be empty"
    );
    let fn_names: Vec<_> = cg
        .nodes_arena
        .iter()
        .filter(|n| {
            matches!(
                n.flavor,
                pycg_rs::node::Flavor::Function | pycg_rs::node::Flavor::Method
            )
        })
        .map(|n| n.name.as_str())
        .collect();
    assert!(
        fn_names.contains(&"annotated_fn"),
        "issue2: annotated_fn not found, got: {fn_names:?}"
    );
}

/// Issue #3: complex / nested comprehensions (list-inside-list, dict-in-list,
/// generator-as-iterable) must not crash the analyzer.
#[test]
fn test_regression_comprehensions() {
    let fixture = test_code_dir().join("regression_issue3.py");
    let files = vec![fixture.to_string_lossy().to_string()];
    let cg =
        CallGraph::new(&files, None).expect("issue3: comprehensions must not crash the analyzer");
    let fn_names: Vec<_> = cg
        .nodes_arena
        .iter()
        .filter(|n| {
            matches!(
                n.flavor,
                pycg_rs::node::Flavor::Function | pycg_rs::node::Flavor::Method
            )
        })
        .map(|n| n.name.as_str())
        .collect();
    assert!(
        fn_names.contains(&"f"),
        "issue3: function f not found, got: {fn_names:?}"
    );
    assert!(
        fn_names.contains(&"g"),
        "issue3: function g not found, got: {fn_names:?}"
    );
    assert!(
        fn_names.contains(&"h"),
        "issue3: function h not found, got: {fn_names:?}"
    );
}

/// Issue #5: files that reference external / uninstalled packages (numpy,
/// pandas) and relative imports whose targets don't exist must not crash.
#[test]
fn test_regression_external_deps() {
    let fixture = test_code_dir().join("regression_issue5.py");
    let files = vec![fixture.to_string_lossy().to_string()];
    let cg = CallGraph::new(&files, None)
        .expect("issue5: external-dep imports must not crash the analyzer");
    let class_names: Vec<_> = cg
        .nodes_arena
        .iter()
        .filter(|n| n.flavor == pycg_rs::node::Flavor::Class)
        .map(|n| n.name.as_str())
        .collect();
    assert!(
        class_names.contains(&"MyProcessor"),
        "issue5: MyProcessor not found, got: {class_names:?}"
    );
}

// ===================================================================
