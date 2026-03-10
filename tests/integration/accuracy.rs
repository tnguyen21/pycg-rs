use crate::common::*;

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
    // @factory_decorator("arg") — module must use the outer factory, and the
    // returned inner decorator must be what applies to factory_decorated.
    let cg = make_single_fixture_graph("accuracy_decorator.py");
    assert!(
        has_uses_edge(&cg, "accuracy_decorator", "factory_decorator"),
        "module should use factory_decorator (applied as @factory_decorator(...))"
    );
    assert!(
        has_uses_edge(&cg, "decorator", "factory_decorated"),
        "the inner decorator returned by factory_decorator should use factory_decorated"
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

#[test]
fn test_accuracy_list_subscript_method_resolution() {
    let cg = make_single_fixture_graph("accuracy_container.py");
    assert!(
        has_uses_edge(&cg, "list_subscript_method_caller", "handle_a"),
        "list_subscript_method_caller should use handle_a via handlers[0]"
    );
    assert!(
        has_uses_edge(&cg, "list_subscript_method_caller", "handle_b"),
        "list_subscript_method_caller should use handle_b via handlers[1]"
    );
}

#[test]
fn test_accuracy_dict_subscript_method_resolution() {
    let cg = make_single_fixture_graph("accuracy_container.py");
    assert!(
        has_uses_edge(&cg, "dict_subscript_method_caller", "handle_a"),
        "dict_subscript_method_caller should use handle_a via handlers[\"a\"]"
    );
    assert!(
        has_uses_edge(&cg, "dict_subscript_method_caller", "handle_b"),
        "dict_subscript_method_caller should use handle_b via handlers[\"b\"]"
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
    assert!(
        has_uses_edge(&cg, "star_at_end", "Alpha"),
        "star_at_end must use Alpha"
    );
    assert!(
        has_uses_edge(&cg, "star_at_end", "Beta"),
        "star_at_end must use Beta"
    );
    assert!(
        has_uses_edge(&cg, "star_at_end", "Gamma"),
        "star_at_end must use Gamma"
    );
    assert!(
        has_uses_edge(&cg, "star_at_end", "Delta"),
        "star_at_end must use Delta"
    );
    // star_in_middle: a, *b, c = Alpha(), Beta(), Gamma(), Delta()
    assert!(
        has_uses_edge(&cg, "star_in_middle", "Alpha"),
        "star_in_middle must use Alpha"
    );
    assert!(
        has_uses_edge(&cg, "star_in_middle", "Delta"),
        "star_in_middle must use Delta"
    );
    // star_at_start: *a, b = Alpha(), Beta(), Gamma()
    assert!(
        has_uses_edge(&cg, "star_at_start", "Gamma"),
        "star_at_start must use Gamma"
    );
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
/// `get_a_via_A` calls `test_func1(self.to_A().b.a)` — the chain head
/// `self.to_A()` must resolve to the `A` class so the `A` uses-edge exists.
#[test]
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

// -------------------------------------------------------------------
// Direct call-result attribute resolution (INV-1 for chained calls)
//
// Tests that `make().method()` resolves correctly — no intermediate
// variable binding needed.  Covers the `get_obj_ids_for_expr` fix for
// Expr::Call that propagates return types for non-builtin calls.
// -------------------------------------------------------------------

#[test]
fn test_accuracy_direct_chain_call_tracked() {
    // direct_chain_caller() calls make() — uses edge must exist.
    let cg = make_single_fixture_graph("accuracy_chained_call.py");
    assert!(
        has_uses_edge(&cg, "direct_chain_caller", "make"),
        "direct_chain_caller should use make (direct call)"
    );
}

#[test]
fn test_accuracy_direct_chain_method_resolved() {
    // make().render() — render must be reachable via make()'s return type.
    let cg = make_single_fixture_graph("accuracy_chained_call.py");
    assert!(
        has_uses_edge(&cg, "direct_chain_caller", "render"),
        "direct_chain_caller should use render via make() return-type propagation"
    );
}

// -------------------------------------------------------------------
// Multi-return propagation (INV-2: all candidates preserved at call site)
//
// Tests that `choose(flag)` returning either A() or B() causes BOTH
// A.method and B.method to be reachable at `obj = choose(flag); obj.method()`.
// -------------------------------------------------------------------

#[test]
fn test_accuracy_multi_return_caller_uses_choose() {
    // multi_return_caller calls choose — direct call edge must exist.
    let cg = make_single_fixture_graph("accuracy_multi_return.py");
    assert!(
        has_uses_edge(&cg, "multi_return_caller", "choose"),
        "multi_return_caller should use choose (direct call)"
    );
}

#[test]
fn test_accuracy_multi_return_both_methods_reachable() {
    // choose(flag) returns A() or B(); both A.method and B.method must be used.
    let cg = make_single_fixture_graph("accuracy_multi_return.py");
    let caller_ids = find_nodes_by_name(&cg, "multi_return_caller");
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
        "multi_return_caller should use method from both A and B (multi-return propagation), \
         found {} method node(s)",
        method_nodes.len()
    );
}

#[test]
fn test_accuracy_nested_multi_return_wrapper_preserves_all_candidates() {
    let cg = make_single_fixture_graph("accuracy_multi_return.py");
    assert!(
        has_uses_edge(&cg, "wrapped_multi_return_caller", "wrap"),
        "wrapped_multi_return_caller should use wrap (direct call)"
    );
    assert!(
        has_uses_edge(&cg, "wrap", "choose"),
        "wrap should use choose when returning choose(flag)"
    );
    let caller_ids = find_nodes_by_name(&cg, "wrapped_multi_return_caller");
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
        "wrapped_multi_return_caller should use method from both A and B through wrap(flag), \
         found {} method node(s)",
        method_nodes.len()
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

/// `conditional_rebind_caller` in accuracy_branch.py assigns `x = A()`
/// unconditionally and then `x = B()` only inside an `if flag:` branch.
/// Because the rebind is conditional, both A.method and B.method are genuinely
/// reachable (depending on the runtime value of `flag`).  A sound analysis must
/// retain both at the join point and emit uses edges to both.
#[test]
fn test_inv1_conditional_rebind_both_branches() {
    let cg = make_single_fixture("accuracy_branch.py");
    let uses = get_uses(&cg, "conditional_rebind_caller");
    assert!(
        uses.contains("method"),
        "conditional_rebind_caller should use method (branch join), got: {uses:?}"
    );
    let caller_ids = find_nodes_by_name(&cg, "conditional_rebind_caller");
    assert!(
        !caller_ids.is_empty(),
        "conditional_rebind_caller node must exist"
    );
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
        "conditional_rebind_caller should use method from both A (unconditional) \
         and B (conditional branch) — found {} method node(s)",
        method_nodes.len()
    );
}

// -------------------------------------------------------------------
// INV-2: alias rebinding — earlier candidate must not be silently dropped
// -------------------------------------------------------------------

/// `branch_alias_caller(flag)` assigns `alias = func_a` in the if-branch and
/// `alias = func_b` in the else-branch.  Because the assignment is conditional,
/// both func_a and func_b are genuinely reachable at the `alias()` call site
/// (depending on the runtime value of `flag`).  The analyzer must emit uses
/// edges to both.
#[test]
fn test_inv2_branch_alias_both_values() {
    let cg = make_single_fixture("accuracy_alias.py");
    let uses = get_uses(&cg, "branch_alias_caller");
    assert!(
        uses.contains("func_a"),
        "branch_alias_caller should use func_a (if-branch target), got: {uses:?}"
    );
    assert!(
        uses.contains("func_b"),
        "branch_alias_caller should use func_b (else-branch target), got: {uses:?}"
    );
}

/// `local_rebind_caller(flag)` assigns `foo = func_a` in the if-branch and
/// `foo = bar` in the else-branch.  Both func_a and bar are genuinely reachable
/// at the `foo()` call site depending on `flag`.
///
/// (Renamed from the mislabeled `import_alias_caller` — there is no import
/// alias in this fixture; it is plain local variable rebinding via branches.)
#[test]
fn test_inv2_local_rebind_branch_both_values() {
    let cg = make_single_fixture("accuracy_alias.py");
    let uses = get_uses(&cg, "local_rebind_caller");
    assert!(
        uses.contains("func_a"),
        "local_rebind_caller should use func_a (if-branch target), got: {uses:?}"
    );
    assert!(
        uses.contains("bar"),
        "local_rebind_caller should use bar (else-branch target), got: {uses:?}"
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
    assert_eq!(
        uses,
        HashSet::from([String::from("reexport_func")]),
        "reexport_caller should only use reexport_func in this fixture"
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

// ===================================================================
// INV-1: __all__-aware star-import resolution
//
// When the source module defines `__all__ = [...]` with statically
// known string literals, `from src import *` must inject exactly those
// names and no others, even if some listed names are private (_-prefixed)
// and some unlisted names are public.
// ===================================================================

/// Names listed in `__all__` — including a private one — must resolve
/// after `from star_all_src import *`.
#[test]
fn test_star_import_all_exports_listed_names_resolve() {
    let cg = make_multi_fixture_graph(&["test_code/star_all_src.py", "test_code/star_all_user.py"]);
    // public_exported is in __all__ → must resolve concretely.
    assert!(
        has_concrete_uses_edge(&cg, "all_aware_caller", "public_exported"),
        "all_aware_caller must concretely use public_exported (listed in __all__)"
    );
    // _special_exported is private but explicitly in __all__ → must resolve.
    assert!(
        has_concrete_uses_edge(&cg, "all_aware_caller", "_special_exported"),
        "all_aware_caller must concretely use _special_exported \
         (private but explicitly in __all__ — INV-1)"
    );
}

/// A public name absent from `__all__` must NOT be concretely resolved
/// after `from star_all_src import *`.
#[test]
fn test_star_import_all_excludes_unlisted_public() {
    let cg = make_multi_fixture_graph(&["test_code/star_all_src.py", "test_code/star_all_user.py"]);
    // public_not_exported is public but NOT in __all__ → must not resolve concretely.
    assert!(
        !has_concrete_uses_edge(&cg, "all_aware_caller", "public_not_exported"),
        "all_aware_caller must NOT concretely use public_not_exported \
         (public but not in __all__ — INV-1)"
    );
}

/// A private name absent from `__all__` must NOT be concretely resolved
/// after `from star_all_src import *`, and wildcard expansion must not
/// reattach it (INV-3).
#[test]
fn test_star_import_all_excludes_unlisted_private() {
    let cg = make_multi_fixture_graph(&["test_code/star_all_src.py", "test_code/star_all_user.py"]);
    // _private_not_exported is private and NOT in __all__ → must not resolve.
    assert!(
        !has_concrete_uses_edge(&cg, "all_aware_caller", "_private_not_exported"),
        "all_aware_caller must NOT concretely use _private_not_exported \
         (private, not in __all__, expand_unknowns must not resurrect it — INV-3)"
    );
}

// ===================================================================
// INV-2 / INV-3: Privacy filter without __all__
//
// Without an `__all__`, `from src import *` must inject only public
// names.  Private names must remain unresolved, and `expand_unknowns`
// must not reattach them.
// ===================================================================

/// Public name without `__all__` must resolve via star import.
#[test]
fn test_star_import_no_all_public_resolves() {
    let cg = make_multi_fixture_graph(&[
        "test_code/star_private_src.py",
        "test_code/star_private_user.py",
    ]);
    assert!(
        has_concrete_uses_edge(&cg, "privacy_checker", "public_func"),
        "privacy_checker must concretely use public_func \
         (public name, no __all__ — existing public star-import must keep working)"
    );
}

/// Private name without `__all__` must NOT resolve via star import, and
/// `expand_unknowns` must not resurrect it through global short-name fanout.
#[test]
fn test_star_import_no_all_private_stays_unresolved() {
    let cg = make_multi_fixture_graph(&[
        "test_code/star_private_src.py",
        "test_code/star_private_user.py",
    ]);
    // _private_impl was filtered out during star import (INV-2) and must
    // not be reconnected by expand_unknowns (INV-3).
    assert!(
        !has_concrete_uses_edge(&cg, "privacy_checker", "_private_impl"),
        "privacy_checker must NOT concretely use _private_impl \
         (private name excluded by star-import filter; \
          expand_unknowns must not resurrect it — INV-2 + INV-3)"
    );
}
