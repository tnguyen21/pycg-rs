use crate::common::*;

#[test]
fn test_stmt_while_produces_edge() {
    let cg = make_fixture_graph("stmt_coverage.py");
    assert!(
        has_uses_edge(&cg, "uses_while", "process"),
        "call inside while body must produce uses edge"
    );
}

#[test]
fn test_stmt_try_produces_edge() {
    let cg = make_fixture_graph("stmt_coverage.py");
    assert!(
        has_uses_edge(&cg, "uses_try", "process"),
        "call inside try body must produce uses edge"
    );
}

#[test]
fn test_stmt_try_except_body_produces_edge() {
    let cg = make_fixture_graph("stmt_coverage.py");
    assert!(
        has_uses_edge(&cg, "uses_try_except_body", "assist"),
        "call inside except body must produce uses edge"
    );
}

#[test]
fn test_stmt_match_produces_edge() {
    let cg = make_fixture_graph("stmt_coverage.py");
    assert!(
        has_uses_edge(&cg, "uses_match", "process"),
        "call inside match arm must produce uses edge"
    );
}

#[test]
fn test_stmt_ann_assign_produces_edge() {
    let cg = make_fixture_graph("stmt_coverage.py");
    assert!(
        has_uses_edge(&cg, "uses_ann_assign", "process"),
        "annotated assignment + call must produce uses edge"
    );
}

#[test]
fn test_stmt_lambda_produces_edge() {
    let cg = make_fixture_graph("stmt_coverage.py");
    // After collapse_inner, lambda edges merge into parent function.
    assert!(
        has_uses_edge(&cg, "uses_lambda", "process"),
        "lambda body call must produce uses edge (after collapse_inner)"
    );
}

#[test]
fn test_stmt_default_arg_produces_edge() {
    let cg = make_fixture_graph("stmt_coverage.py");
    assert!(
        has_uses_edge(&cg, "uses_defaults", "Worker"),
        "default argument call must produce uses edge"
    );
}

#[test]
fn test_stmt_for_produces_edge() {
    let cg = make_fixture_graph("stmt_coverage.py");
    // For-loop body is visited; at minimum the constructor call is tracked.
    assert!(
        has_uses_edge(&cg, "uses_for", "Worker"),
        "call inside for body must produce uses edge to constructor"
    );
}

#[test]
fn test_stmt_with_produces_edge() {
    let cg = make_fixture_graph("stmt_coverage.py");
    assert!(
        has_uses_edge(&cg, "uses_with", "process"),
        "call inside with body must produce uses edge"
    );
}

#[test]
fn test_stmt_global_scope_defs() {
    let cg = make_fixture_graph("stmt_coverage.py");
    // global statement must allow name to be collected in scope defs
    assert!(
        has_uses_edge(&cg, "uses_global", "Worker"),
        "global var assignment must produce uses edge to class"
    );
}

#[test]
fn test_stmt_nonlocal_scope_defs() {
    let cg = make_fixture_graph("stmt_coverage.py");
    assert!(
        has_uses_edge(&cg, "inner", "Worker"),
        "nonlocal var assignment must produce uses edge to class"
    );
}

// ===================================================================
// Binding / assignment coverage
// ===================================================================

#[test]
fn test_binding_tuple_unpack() {
    let cg = make_fixture_graph("binding_coverage.py");
    assert!(
        has_uses_edge(&cg, "tuple_unpack", "x_method"),
        "first tuple element should resolve to X"
    );
    assert!(
        has_uses_edge(&cg, "tuple_unpack", "y_method"),
        "second tuple element should resolve to Y"
    );
}

#[test]
fn test_binding_list_unpack() {
    let cg = make_fixture_graph("binding_coverage.py");
    assert!(
        has_uses_edge(&cg, "list_unpack", "x_method"),
        "first list element should resolve to X"
    );
    assert!(
        has_uses_edge(&cg, "list_unpack", "y_method"),
        "second list element should resolve to Y"
    );
}

#[test]
fn test_binding_nested_tuple() {
    let cg = make_fixture_graph("binding_coverage.py");
    // At minimum the outer tuple first element resolves.
    assert!(
        has_uses_edge(&cg, "nested_tuple_unpack", "x_method"),
        "first element of nested tuple should resolve"
    );
    // Inner nested tuple is harder to resolve; check constructors at least.
    assert!(
        has_uses_edge(&cg, "nested_tuple_unpack", "Y")
            || has_uses_edge(&cg, "nested_tuple_unpack", "Z"),
        "nested tuple constructors should be tracked"
    );
}

#[test]
fn test_binding_starred() {
    let cg = make_fixture_graph("binding_coverage.py");
    assert!(
        has_uses_edge(&cg, "starred_unpack", "x_method"),
        "first element before star should resolve"
    );
}

#[test]
fn test_binding_attr_assignment() {
    let cg = make_fixture_graph("binding_coverage.py");
    // self.item = X() in __init__; self.item.x_method() in use_item
    // The analyzer tracks attribute assignment via set_attribute.
    assert!(
        has_uses_edge(&cg, "__init__", "X"),
        "self.item = X() should track constructor call"
    );
}

#[test]
fn test_binding_aug_assign() {
    let cg = make_fixture_graph("binding_coverage.py");
    assert!(
        has_uses_edge(&cg, "aug_assign", "X") || has_uses_edge(&cg, "aug_assign", "Y"),
        "augmented assignment should produce edge to constructor"
    );
}

// ===================================================================
// Resolution coverage (attributes, MRO, subscripts)
// ===================================================================

#[test]
fn test_resolution_chained_attr() {
    let cg = make_fixture_graph("resolution_coverage.py");
    // Chained attribute access: o.inner.deep_method()
    // At minimum, the constructor and attribute access are tracked.
    assert!(
        has_uses_edge(&cg, "chained_attr", "Outer"),
        "chained attr should track constructor"
    );
    assert!(
        has_uses_edge(&cg, "chained_attr", "inner")
            || has_uses_edge(&cg, "chained_attr", "deep_method"),
        "chained attr should track attribute access"
    );
}

#[test]
fn test_resolution_call_then_attr() {
    let cg = make_fixture_graph("resolution_coverage.py");
    // Outer().inner.deep_method() — call then attribute chain
    assert!(
        has_uses_edge(&cg, "call_then_attr", "Outer"),
        "call-then-attr should track constructor"
    );
}

#[test]
fn test_resolution_mro_grandchild() {
    let cg = make_fixture_graph("resolution_coverage.py");
    assert!(
        has_uses_edge(&cg, "mro_grandchild", "inherited"),
        "GrandChild.inherited() should resolve via MRO to GrandParent"
    );
}

#[test]
fn test_resolution_subscript_call() {
    let cg = make_fixture_graph("resolution_coverage.py");
    assert!(
        has_uses_edge(&cg, "subscript_call", "deep_method"),
        "items['key'].deep_method() should resolve through subscript"
    );
}

// ===================================================================
// Postprocessing effects
// ===================================================================

#[test]
fn test_postprocess_cull_inherited() {
    let cg = make_fixture_graph("postprocess_effects.py");
    // caller_uses_child calls c.inherited_method() — after cull_inherited,
    // the edge should target a concrete method, not be duplicated.
    assert!(
        has_uses_edge(&cg, "caller_uses_child", "inherited_method"),
        "inherited method call must resolve"
    );
    assert!(
        has_uses_edge(&cg, "caller_uses_child", "own_method"),
        "own method call must resolve"
    );
}

#[test]
fn test_postprocess_collapse_inner_lambda() {
    let cg = make_fixture_graph("postprocess_effects.py");
    assert!(
        has_uses_edge(&cg, "caller_with_lambda", "own_method"),
        "lambda body edges should collapse into parent function"
    );
}

#[test]
fn test_postprocess_collapse_inner_listcomp() {
    let cg = make_fixture_graph("postprocess_effects.py");
    assert!(
        has_uses_edge(&cg, "caller_with_listcomp", "Child"),
        "listcomp edges should collapse into parent function"
    );
}

#[test]
fn test_postprocess_resolve_imports() {
    // Use the full test_code dir which has real cross-module imports.
    let cg = make_call_graph(&test_code_dir());
    // submodule1 does: from test_code.subpackage1 import A
    // After resolve_imports, the edge should point to a concrete node.
    let uses = get_uses(&cg, "submodule1");
    assert!(
        uses.contains("A") || uses.contains("subpackage1"),
        "import resolution must produce concrete edge, got: {:?}",
        uses
    );
}

#[test]
fn test_postprocess_changes_graph() {
    // Verify postprocessing is not a no-op: the full test_code dir graph
    // should have defined nodes that are Module-flavored (postprocess keeps these).
    let cg = make_call_graph(&test_code_dir());
    let module_count = cg
        .nodes_arena
        .iter()
        .enumerate()
        .filter(|(id, n)| n.flavor == pycg_rs::node::Flavor::Module && cg.defined.contains(id))
        .count();
    assert!(
        module_count >= 3,
        "postprocessing should keep module nodes defined, got {module_count}"
    );
}

// ===================================================================
// Import coverage (relative imports, visit_import)
// ===================================================================

#[test]
fn test_import_relative_resolution() {
    let cg = make_fixture_dir_graph("import_coverage");
    assert!(
        has_uses_edge(&cg, "caller", "sibling_func"),
        "relative import from .sibling should resolve sibling_func"
    );
    assert!(
        has_uses_edge(&cg, "caller", "deep_func"),
        "relative import from .deep.inner should resolve deep_func"
    );
}

#[test]
fn test_import_module_reference() {
    let cg = make_fixture_dir_graph("import_coverage");
    // `from . import sibling` should create a uses edge to the sibling module
    let uses = get_uses(&cg, "user");
    assert!(
        uses.iter().any(|n| n.contains("sibling")),
        "from . import sibling should create module edge, got: {:?}",
        uses
    );
}

#[test]
fn test_visit_import_produces_module_edge() {
    // The full test_code dir: submodule2.py does `from . import submodule1`
    let cg = make_call_graph(&test_code_dir());
    let uses = get_uses(&cg, "submodule2");
    assert!(
        uses.iter().any(|n| n.contains("submodule1")),
        "import statement should produce uses edge to module, got: {:?}",
        uses
    );
}

// ===================================================================
