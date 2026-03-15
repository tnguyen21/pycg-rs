//! Integration tests for pyan-rs.
//!
//! Uses Python test fixtures in tests/test_code/.

pub use std::collections::HashSet;
pub use std::path::PathBuf;

pub use pycg_rs::analyzer::CallGraph;
#[allow(unused_imports)]
pub use pycg_rs::intern::Interner;
pub use pycg_rs::visgraph::{VisualGraph, VisualOptions};
pub use pycg_rs::writer;

pub(crate) fn test_code_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("test_code")
}

pub(crate) fn collect_py_files(dir: &std::path::Path) -> Vec<String> {
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

pub(crate) fn make_call_graph(dir: &std::path::Path) -> CallGraph {
    let files = collect_py_files(dir);
    let root = dir.parent().unwrap().to_string_lossy().to_string();
    CallGraph::new(&files, Some(&root)).expect("analysis should succeed")
}

/// Find all nodes with the given short name, or whose fully qualified name ends with the given name.
pub(crate) fn find_nodes_by_name(cg: &CallGraph, name: &str) -> Vec<usize> {
    let mut result: Vec<usize> = cg
        .interner
        .lookup(name)
        .and_then(|sym| cg.nodes_by_name.get(&sym))
        .cloned()
        .unwrap_or_default();
    for (idx, node) in cg.nodes_arena.iter().enumerate() {
        let full = node.get_name(&cg.interner);
        if (full == name || full.ends_with(&format!(".{name}"))) && !result.contains(&idx) {
            result.push(idx);
        }
    }
    result
}

/// Get the set of short names that `source_name` defines.
pub(crate) fn get_defines(cg: &CallGraph, source_name: &str) -> HashSet<String> {
    let mut result = HashSet::new();
    for &nid in find_nodes_by_name(cg, source_name).iter() {
        if let Some(targets) = cg.defines_edges.get(&nid) {
            for &tid in targets {
                result.insert(cg.interner.resolve(cg.nodes_arena[tid].name).to_owned());
            }
        }
    }
    result
}

/// Get the set of short names that `source_name` uses.
pub(crate) fn get_uses(cg: &CallGraph, source_name: &str) -> HashSet<String> {
    let mut result = HashSet::new();
    for &nid in find_nodes_by_name(cg, source_name).iter() {
        if let Some(targets) = cg.uses_edges.get(&nid) {
            for &tid in targets {
                result.insert(cg.interner.resolve(cg.nodes_arena[tid].name).to_owned());
            }
        }
    }
    result
}

/// Get the set of fully-qualified names that `source_name` uses.
pub(crate) fn get_full_uses(cg: &CallGraph, source_name: &str) -> HashSet<String> {
    let mut result = HashSet::new();
    for &nid in find_nodes_by_name(cg, source_name).iter() {
        if let Some(targets) = cg.uses_edges.get(&nid) {
            for &tid in targets {
                result.insert(cg.nodes_arena[tid].get_name(&cg.interner));
            }
        }
    }
    result
}

/// Check if there is a defines edge from a node matching `from_name` to one matching `to_name`.
pub(crate) fn has_defines_edge(cg: &CallGraph, from_name: &str, to_name: &str) -> bool {
    for &fid in find_nodes_by_name(cg, from_name).iter() {
        if let Some(targets) = cg.defines_edges.get(&fid) {
            for &tid in targets {
                if cg.interner.resolve(cg.nodes_arena[tid].name) == to_name {
                    return true;
                }
            }
        }
    }
    false
}

/// Check if there is a uses edge from a node matching `from_name` to one matching `to_name`.
pub(crate) fn has_uses_edge(cg: &CallGraph, from_name: &str, to_name: &str) -> bool {
    for &fid in find_nodes_by_name(cg, from_name).iter() {
        if let Some(targets) = cg.uses_edges.get(&fid) {
            for &tid in targets {
                if cg.interner.resolve(cg.nodes_arena[tid].name) == to_name {
                    return true;
                }
            }
        }
    }
    false
}

/// Check if there is a uses edge from an exact fully-qualified source node to an exact
/// fully-qualified target node.
pub(crate) fn has_uses_edge_full(cg: &CallGraph, from_fqn: &str, to_fqn: &str) -> bool {
    let from_ids: Vec<usize> = cg
        .nodes_arena
        .iter()
        .enumerate()
        .filter_map(|(idx, node)| (node.get_name(&cg.interner) == from_fqn).then_some(idx))
        .collect();
    let to_ids: Vec<usize> = cg
        .nodes_arena
        .iter()
        .enumerate()
        .filter_map(|(idx, node)| (node.get_name(&cg.interner) == to_fqn).then_some(idx))
        .collect();

    for fid in from_ids {
        if let Some(targets) = cg.uses_edges.get(&fid) {
            for tid in &to_ids {
                if targets.contains(tid) {
                    return true;
                }
            }
        }
    }
    false
}

pub(crate) fn make_features_graph() -> CallGraph {
    let features_file = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("test_code")
        .join("features.py");
    let files = vec![features_file.to_string_lossy().to_string()];
    CallGraph::new(&files, None).expect("should parse features.py")
}

pub(crate) fn make_fixture_graph(fixture: &str) -> CallGraph {
    let file = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("test_code")
        .join(fixture);
    let files = vec![file.to_string_lossy().to_string()];
    CallGraph::new(&files, None).unwrap_or_else(|_| panic!("should parse {fixture}"))
}

pub(crate) fn make_fixture_dir_graph(subdir: &str) -> CallGraph {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("test_code")
        .join(subdir);
    let files = collect_py_files(&dir);
    let root = dir.parent().unwrap().to_string_lossy().to_string();
    CallGraph::new(&files, Some(&root)).unwrap_or_else(|_| panic!("should parse {subdir}"))
}

pub(crate) fn make_single_fixture_graph(fixture_name: &str) -> CallGraph {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("test_code")
        .join(fixture_name);
    let files = vec![path.to_string_lossy().to_string()];
    CallGraph::new(&files, None).unwrap_or_else(|e| panic!("failed to parse {fixture_name}: {e}"))
}

/// Build a CallGraph from multiple accuracy fixture files with `tests/` as root.
pub(crate) fn make_multi_fixture_graph(fixture_relative_paths: &[&str]) -> CallGraph {
    let test_code = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests");
    let files: Vec<String> = fixture_relative_paths
        .iter()
        .map(|p| test_code.join(p).to_string_lossy().to_string())
        .collect();
    let root = test_code.to_string_lossy().to_string();
    CallGraph::new(&files, Some(&root))
        .unwrap_or_else(|e| panic!("failed to parse multi-file fixture: {e}"))
}

pub(crate) fn has_concrete_uses_edge_for_name(
    cg: &CallGraph,
    from_name: &str,
    short_name: &str,
) -> bool {
    for &fid in find_nodes_by_name(cg, from_name).iter() {
        if let Some(targets) = cg.uses_edges.get(&fid) {
            for &tid in targets {
                if cg.interner.resolve(cg.nodes_arena[tid].name) == short_name
                    && cg.nodes_arena[tid].namespace.is_some()
                {
                    return true;
                }
            }
        }
    }
    false
}

pub(crate) fn has_concrete_uses_edge_full(cg: &CallGraph, from_fqn: &str, to_fqn: &str) -> bool {
    let from_ids: Vec<usize> = cg
        .nodes_arena
        .iter()
        .enumerate()
        .filter_map(|(idx, node)| (node.get_name(&cg.interner) == from_fqn).then_some(idx))
        .collect();
    let to_ids: Vec<usize> = cg
        .nodes_arena
        .iter()
        .enumerate()
        .filter_map(|(idx, node)| {
            (node.get_name(&cg.interner) == to_fqn && node.namespace.is_some()).then_some(idx)
        })
        .collect();

    for fid in from_ids {
        if let Some(targets) = cg.uses_edges.get(&fid) {
            for tid in &to_ids {
                if targets.contains(tid) {
                    return true;
                }
            }
        }
    }
    false
}
