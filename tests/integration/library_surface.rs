use crate::common::*;

// get_module_name unit tests
// ===================================================================

#[test]
fn test_get_module_name_with_root() {
    use pycg_rs::analyzer::get_module_name;
    let tc = test_code_dir();
    let root = tc.parent().unwrap().to_string_lossy().to_string();
    let file = tc.join("features.py").to_string_lossy().to_string();
    let name = get_module_name(&file, Some(&root));
    assert_eq!(name, "test_code.features", "module name with root: {name}");
}

#[test]
fn test_get_module_name_init_file() {
    use pycg_rs::analyzer::get_module_name;
    let tc = test_code_dir();
    let root = tc.parent().unwrap().to_string_lossy().to_string();
    let file = tc
        .join("subpackage1")
        .join("__init__.py")
        .to_string_lossy()
        .to_string();
    let name = get_module_name(&file, Some(&root));
    assert_eq!(
        name, "test_code.subpackage1",
        "init file should map to package: {name}"
    );
}

#[test]
fn test_get_module_name_nested() {
    use pycg_rs::analyzer::get_module_name;
    let tc = test_code_dir();
    let root = tc.parent().unwrap().to_string_lossy().to_string();
    let file = tc
        .join("subpackage1")
        .join("submodule1.py")
        .to_string_lossy()
        .to_string();
    let name = get_module_name(&file, Some(&root));
    assert_eq!(
        name, "test_code.subpackage1.submodule1",
        "nested module: {name}"
    );
}

#[test]
fn test_get_module_name_no_root() {
    use pycg_rs::analyzer::get_module_name;
    let tc = test_code_dir();
    let file = tc
        .join("subpackage1")
        .join("submodule1.py")
        .to_string_lossy()
        .to_string();
    let name = get_module_name(&file, None);
    // Without root, walk-up should still find test_code package
    assert!(
        name.contains("subpackage1"),
        "should contain package: {name}"
    );
    assert!(
        name.ends_with("submodule1"),
        "should end with module: {name}"
    );
}

// ===================================================================
// Node, visgraph, writer targeted tests
// ===================================================================

#[test]
fn test_node_equality_and_hash() {
    use pycg_rs::node::{Flavor, Node};
    use std::collections::HashSet;

    let a = Node::new(Some("pkg"), "Foo", Flavor::Class);
    let b = Node::new(Some("pkg"), "Foo", Flavor::Function); // same ns+name, different flavor
    let c = Node::new(Some("other"), "Foo", Flavor::Class); // different namespace

    // PartialEq only checks namespace + name
    assert_eq!(
        a, b,
        "same namespace+name should be equal regardless of flavor"
    );
    assert_ne!(a, c, "different namespace should not be equal");

    // Hash consistency
    let mut set = HashSet::new();
    set.insert(a.clone());
    assert!(set.contains(&b), "equal nodes must have same hash");
    assert!(!set.contains(&c), "different nodes must not collide");
}

#[test]
fn test_node_display_and_short_name() {
    use pycg_rs::node::{Flavor, Node};

    let n = Node::new(Some("pkg.sub"), "func", Flavor::Function);
    assert_eq!(format!("{n}"), "pkg.sub.func");
    assert_eq!(n.get_short_name(), "func");
    assert_eq!(n.get_name(), "pkg.sub.func");

    let root = Node::new(Some(""), "mod", Flavor::Module);
    assert_eq!(root.get_name(), "mod");
    assert_eq!(root.get_short_name(), "mod");
}

#[test]
fn test_node_specificity_ordering() {
    use pycg_rs::node::Flavor;

    // More specific flavors must have higher specificity
    assert!(Flavor::Function.specificity() > Flavor::Module.specificity());
    assert!(Flavor::Method.specificity() > Flavor::Function.specificity());
    assert!(Flavor::Module.specificity() > Flavor::ImportedItem.specificity());
    assert!(Flavor::ImportedItem.specificity() > Flavor::Name.specificity());
    assert!(Flavor::Unknown.specificity() > Flavor::Unspecified.specificity());
}

#[test]
fn test_flavor_display() {
    use pycg_rs::node::Flavor;

    assert_eq!(format!("{}", Flavor::Module), "module");
    assert_eq!(format!("{}", Flavor::Function), "function");
    assert_eq!(format!("{}", Flavor::Class), "class");
    assert_eq!(format!("{}", Flavor::Method), "method");
}

#[test]
fn test_dot_output_indent_and_edges() {
    // Test that DOT output has proper structure: indented nodes, styled edges.
    let cg = make_fixture_graph("stmt_coverage.py");
    let opts = VisualOptions {
        draw_defines: true,
        draw_uses: true,
        colored: true,
        grouped: false,
        annotated: true,
    };
    let vg = VisualGraph::from_call_graph(
        &cg.nodes_arena,
        &cg.defined,
        &cg.defines_edges,
        &cg.uses_edges,
        &opts,
    );
    let dot = writer::write_dot(&vg, &["rankdir=TB".to_string()]);

    // Nodes should be indented with spaces
    assert!(
        dot.lines()
            .any(|l| l.starts_with("    ") && l.contains("label=")),
        "nodes should be indented"
    );
    // Uses edges should be solid
    assert!(
        dot.contains("style=\"solid\""),
        "uses edges should be solid"
    );
    // Defines edges should be dashed
    assert!(
        dot.contains("style=\"dashed\""),
        "defines edges should be dashed"
    );
    // Annotated labels should contain file info
    assert!(
        dot.contains("stmt_coverage"),
        "annotated labels should contain filename"
    );
}

#[test]
fn test_tgf_output_structure() {
    let cg = make_fixture_graph("stmt_coverage.py");
    let opts = VisualOptions {
        draw_defines: false,
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
    let parts: Vec<&str> = tgf.splitn(2, '#').collect();
    assert_eq!(parts.len(), 2, "TGF must have # separator");
    // Node section: each line has "index label"
    let node_lines: Vec<&str> = parts[0].trim().lines().collect();
    assert!(!node_lines.is_empty(), "TGF should have nodes");
    // Edge section
    let edge_lines: Vec<&str> = parts[1].trim().lines().collect();
    assert!(!edge_lines.is_empty(), "TGF should have edges");
    // Each edge line should have "src tgt label"
    for line in &edge_lines {
        let fields: Vec<&str> = line.split_whitespace().collect();
        assert!(
            fields.len() >= 3,
            "edge line should have src, tgt, label: {line}"
        );
    }
}

#[test]
fn test_text_output_structure() {
    let cg = make_fixture_graph("stmt_coverage.py");
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
    // Should have [U] and [D] tags
    assert!(text.contains("[U]"), "text output should have uses edges");
    assert!(
        text.contains("[D]"),
        "text output should have defines edges"
    );
    // Indented edges
    assert!(
        text.lines()
            .any(|l| l.starts_with("    [U]") || l.starts_with("    [D]")),
        "edges should be indented under their source node"
    );
}

#[test]
fn test_dot_grouped_subgraph_indent() {
    let cg = make_fixture_graph("stmt_coverage.py");
    let opts = VisualOptions {
        draw_defines: false,
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
        "grouped output must have subgraphs"
    );
    // Subgraph nodes should be doubly indented
    assert!(
        dot.lines()
            .any(|l| l.starts_with("        ") && l.contains("label=")),
        "subgraph nodes should be more deeply indented"
    );
}

// ===================================================================
// Visgraph color math
// ===================================================================

#[test]
fn test_hls_to_rgb_varied() {
    use pycg_rs::visgraph::{hls_to_rgb, rgb_hex, rgba_hex};

    // Green (h=0.333)
    let (r, g, b) = hls_to_rgb(0.333, 0.5, 1.0);
    assert!(
        g > r && g > b,
        "green hue should have highest green channel: ({r}, {g}, {b})"
    );

    // Blue (h=0.667)
    let (r, g, b) = hls_to_rgb(0.667, 0.5, 1.0);
    assert!(
        b > r && b > g,
        "blue hue should have highest blue channel: ({r}, {g}, {b})"
    );

    // Low saturation should be grayish
    let (r, g, b) = hls_to_rgb(0.0, 0.5, 0.1);
    assert!(
        (r - g).abs() < 0.15 && (g - b).abs() < 0.15,
        "low saturation should be near-gray: ({r}, {g}, {b})"
    );

    // High lightness should be near-white
    let (r, g, b) = hls_to_rgb(0.5, 0.95, 1.0);
    assert!(
        r > 0.85 && g > 0.85 && b > 0.85,
        "high lightness should be near-white: ({r}, {g}, {b})"
    );

    // rgb_hex: verify format
    let hex = rgb_hex(1.0, 0.0, 0.0);
    assert_eq!(hex, "#ff0000", "pure red hex");

    let hex = rgb_hex(0.0, 1.0, 0.0);
    assert_eq!(hex, "#00ff00", "pure green hex");

    // rgba_hex: verify alpha channel
    let hex = rgba_hex(1.0, 0.0, 0.0, 0.5);
    assert!(
        hex.starts_with("#ff0000"),
        "rgba red should start with #ff0000: {hex}"
    );
    assert_eq!(hex.len(), 9, "rgba hex should be 9 chars: {hex}");
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
    eprintln!(
        "Average analysis time: {:?} (100 runs over {} files)",
        per_run,
        files.len()
    );
    assert!(
        per_run.as_millis() < 200,
        "Analysis too slow: {:?}",
        per_run
    );
}

// ===================================================================
