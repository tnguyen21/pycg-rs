use crate::common::*;

// Corpus-scale integration smoke tests
//
// Run the analyzer against real-world vendored Python packages from
// benchmarks/corpora/ and assert the resulting graph is non-degenerate.
//
// These tests are marked `#[ignore]` so they don't silently pass when
// the corpus directories are absent.  Run them explicitly with:
//
//   cargo test -- --ignored          # all ignored tests
//   cargo test corpus -- --ignored   # just corpus tests
//
// CI clones the corpora via scripts/bootstrap-corpora.sh before running.
// ===================================================================

/// Resolve the path to a specific package subdirectory inside the vendored
/// corpora.  Panics if the directory does not exist.
fn corpus_dir(package: &str, subpath: &str) -> std::path::PathBuf {
    let candidate = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("benchmarks")
        .join("corpora")
        .join(package)
        .join(subpath);
    assert!(
        candidate.is_dir(),
        "Corpus directory not found: {candidate:?}\n\
         Run `scripts/bootstrap-corpora.sh --only-corpora` to download corpora."
    );
    candidate
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
        .filter(|n| n.flavor == pycg_rs::node::Flavor::Module)
        .count();
    let classes = cg
        .nodes_arena
        .iter()
        .filter(|n| n.flavor == pycg_rs::node::Flavor::Class)
        .count();
    let functions = cg
        .nodes_arena
        .iter()
        .filter(|n| {
            matches!(
                n.flavor,
                pycg_rs::node::Flavor::Function
                    | pycg_rs::node::Flavor::Method
                    | pycg_rs::node::Flavor::StaticMethod
                    | pycg_rs::node::Flavor::ClassMethod
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

    (
        cg,
        CorpusStats {
            modules,
            classes,
            functions,
            uses_edge_count,
        },
    )
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

/// Analyze the `requests` package (~18 files).
///
/// Beyond the smoke-test thresholds, asserts structurally stable cross-module
/// edges that would break only if the analyzer regresses on import resolution,
/// class instantiation, or attribute access.
#[test]
#[ignore] // requires corpora: run with `cargo test -- --ignored`
fn test_corpus_requests() {
    let dir = corpus_dir("requests", "src/requests");

    let (cg, stats) = analyze_corpus(&dir);

    assert_corpus_healthy("requests", &stats, 10, 5, 20, 15);

    // Module-level __init__ calls internal helpers
    assert!(
        has_uses_edge(&cg, "requests", "check_compatibility"),
        "requests.__init__ should call check_compatibility"
    );
    assert!(
        has_uses_edge(&cg, "requests", "_check_cryptography"),
        "requests.__init__ should call _check_cryptography"
    );

    // Cross-module: adapters uses models, auth, cookies, exceptions
    assert!(
        has_uses_edge(&cg, "adapters", "Response"),
        "adapters should reference models.Response"
    );
    assert!(
        has_uses_edge(&cg, "adapters", "_basic_auth_str"),
        "adapters should call auth._basic_auth_str"
    );
    assert!(
        has_uses_edge(&cg, "adapters", "extract_cookies_to_jar"),
        "adapters should call cookies.extract_cookies_to_jar"
    );

    // Exception references from adapters
    assert!(
        has_uses_edge(&cg, "adapters", "ConnectionError"),
        "adapters should reference exceptions.ConnectionError"
    );
}

/// Analyze the `rich` package (~78 files).
#[test]
#[ignore] // requires corpora: run with `cargo test -- --ignored`
fn test_corpus_rich() {
    let dir = corpus_dir("rich", "rich");

    let (cg, stats) = analyze_corpus(&dir);

    assert_corpus_healthy("rich", &stats, 40, 30, 80, 60);

    // __main__.make_test_card uses Console, Table, Panel, Syntax, etc.
    assert!(
        has_uses_edge(&cg, "__main__", "Console"),
        "rich.__main__ should reference console.Console"
    );
    assert!(
        has_uses_edge(&cg, "__main__", "Table"),
        "rich.__main__ should reference table.Table"
    );
    assert!(
        has_uses_edge(&cg, "__main__", "Panel"),
        "rich.__main__ should reference panel.Panel"
    );
    assert!(
        has_uses_edge(&cg, "__main__", "Syntax"),
        "rich.__main__ should reference syntax.Syntax"
    );
    assert!(
        has_uses_edge(&cg, "__main__", "Style"),
        "rich.__main__ should reference style.Style"
    );
}

/// Analyze the `flask` package (~18 files).
#[test]
#[ignore] // requires corpora: run with `cargo test -- --ignored`
fn test_corpus_flask() {
    let dir = corpus_dir("flask", "src/flask");

    let (cg, stats) = analyze_corpus(&dir);

    assert_corpus_healthy("flask", &stats, 8, 5, 20, 15);

    // Flask App class uses Scaffold (its base class)
    assert!(
        has_uses_edge(&cg, "App", "Scaffold"),
        "App should reference scaffold.Scaffold (base class)"
    );

    // Blueprint also inherits from Scaffold
    assert!(
        has_uses_edge(&cg, "Blueprint", "Scaffold"),
        "Blueprint should reference scaffold.Scaffold (base class)"
    );

    // App.__init__ references Blueprint and calls super().__init__
    let init_uses = get_uses(&cg, "__init__");
    assert!(
        init_uses.contains("Blueprint"),
        "App.__init__ should reference Blueprint, got: {init_uses:?}"
    );

    // App.add_url_rule uses scaffold helper
    assert!(
        has_uses_edge(&cg, "add_url_rule", "_endpoint_from_view_func"),
        "add_url_rule should call scaffold._endpoint_from_view_func"
    );
}

// ===================================================================
