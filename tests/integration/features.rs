use crate::common::*;

#[test]
fn test_features_classes_found() {
    let cg = make_features_graph();
    let class_names: HashSet<_> = cg
        .nodes_arena
        .iter()
        .filter(|n| n.flavor == pycg_rs::node::Flavor::Class)
        .map(|n| cg.interner.resolve(n.name))
        .collect();
    for expected in [
        "Decorated",
        "Base",
        "Derived",
        "MixinA",
        "MixinB",
        "Combined",
    ] {
        assert!(
            class_names.contains(expected),
            "Class {expected} not found, got: {class_names:?}"
        );
    }
}

#[test]
fn test_features_decorators() {
    let cg = make_features_graph();
    assert!(has_defines_edge(&cg, "Decorated", "static_method"));
    assert!(has_defines_edge(&cg, "Decorated", "class_method"));
    assert!(has_defines_edge(&cg, "Decorated", "my_prop"));
    assert!(has_defines_edge(&cg, "Decorated", "regular"));

    let sm: Vec<_> = find_nodes_by_name(&cg, "static_method")
        .into_iter()
        .filter(|&id| cg.nodes_arena[id].flavor == pycg_rs::node::Flavor::StaticMethod)
        .collect();
    assert!(
        !sm.is_empty(),
        "static_method should have StaticMethod flavor"
    );

    let cm: Vec<_> = find_nodes_by_name(&cg, "class_method")
        .into_iter()
        .filter(|&id| cg.nodes_arena[id].flavor == pycg_rs::node::Flavor::ClassMethod)
        .collect();
    assert!(
        !cm.is_empty(),
        "class_method should have ClassMethod flavor"
    );
}

#[test]
fn test_features_inheritance() {
    let cg = make_features_graph();
    assert!(
        has_uses_edge(&cg, "Derived", "Base"),
        "Derived should use Base (inheritance)"
    );
    assert!(has_uses_edge(&cg, "bar", "foo"), "bar should use foo");
}

#[test]
fn test_features_multiple_inheritance() {
    let cg = make_features_graph();
    assert!(
        has_uses_edge(&cg, "Combined", "MixinA"),
        "Combined should use MixinA"
    );
    assert!(
        has_uses_edge(&cg, "Combined", "MixinB"),
        "Combined should use MixinB"
    );
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

#[test]
fn test_iterator_protocol_set_comprehension() {
    let cg = make_fixture_graph("comprehension_coverage.py");
    assert!(
        has_uses_edge(&cg, "set_comp_protocol", "__iter__"),
        "set_comp_protocol should use Sequence.__iter__"
    );
    assert!(
        has_uses_edge(&cg, "set_comp_protocol", "__next__"),
        "set_comp_protocol should use Sequence.__next__"
    );
}

#[test]
fn test_iterator_protocol_dict_comprehension() {
    let cg = make_fixture_graph("comprehension_coverage.py");
    assert!(
        has_uses_edge(&cg, "dict_comp_protocol", "__iter__"),
        "dict_comp_protocol should use Sequence.__iter__"
    );
    assert!(
        has_uses_edge(&cg, "dict_comp_protocol", "__next__"),
        "dict_comp_protocol should use Sequence.__next__"
    );
}

#[test]
fn test_iterator_protocol_generator_expression() {
    let cg = make_fixture_graph("comprehension_coverage.py");
    assert!(
        has_uses_edge(&cg, "genexpr_protocol", "__iter__"),
        "genexpr_protocol should use Sequence.__iter__"
    );
    assert!(
        has_uses_edge(&cg, "genexpr_protocol", "__next__"),
        "genexpr_protocol should use Sequence.__next__"
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
        let empty = vec![];
        let nids = cg
            .interner
            .lookup(method)
            .and_then(|sym| cg.nodes_by_name.get(&sym))
            .unwrap_or(&empty);
        for &nid in nids {
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
