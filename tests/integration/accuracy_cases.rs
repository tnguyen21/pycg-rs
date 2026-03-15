use crate::common::*;
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct AccuracyManifest {
    cases: Vec<AccuracyCase>,
}

#[derive(Clone, Debug, Deserialize)]
struct AccuracyCase {
    id: String,
    category: String,
    files: Vec<String>,
    root: Option<String>,
    expectations: Vec<AccuracyExpectation>,
}

#[derive(Clone, Debug, Deserialize)]
struct AccuracyExpectation {
    kind: EdgeKind,
    source: String,
    #[serde(default)]
    source_match: MatchKind,
    target: String,
    #[serde(default)]
    target_match: MatchKind,
    present: bool,
    min_matches: Option<usize>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum EdgeKind {
    Uses,
    Defines,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MatchKind {
    #[default]
    Short,
    Full,
    ConcreteShort,
    ConcreteFull,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct GraphKey {
    files: Vec<String>,
    root: Option<String>,
}

#[derive(Debug)]
struct ExpectationResult {
    matched_targets: usize,
    matched_target_names: Vec<String>,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn manifest_path() -> PathBuf {
    repo_root()
        .join("tests")
        .join("fixtures")
        .join("accuracy_cases.json")
}

fn load_manifest() -> AccuracyManifest {
    let path = manifest_path();
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()))
}

fn build_graph(case: &AccuracyCase) -> CallGraph {
    let root = repo_root();
    let files: Vec<String> = case
        .files
        .iter()
        .map(|file| root.join(file).to_string_lossy().to_string())
        .collect();
    let root_arg = case
        .root
        .as_ref()
        .map(|path| root.join(path).to_string_lossy().to_string());
    CallGraph::new(&files, root_arg.as_deref())
        .unwrap_or_else(|e| panic!("failed to analyze fixture case {}: {e}", case.id))
}

fn matches_node(cg: &CallGraph, node_id: usize, expected: &str, matcher: MatchKind) -> bool {
    let node = &cg.nodes_arena[node_id];
    let full_name = node.get_name(&cg.interner);
    let short_name = full_name.rsplit('.').next().unwrap_or(&full_name);
    match matcher {
        MatchKind::Short => short_name == expected,
        MatchKind::Full => full_name == expected,
        MatchKind::ConcreteShort => short_name == expected && node.namespace.is_some(),
        MatchKind::ConcreteFull => full_name == expected && node.namespace.is_some(),
    }
}

fn resolve_matching_nodes(cg: &CallGraph, expected: &str, matcher: MatchKind) -> Vec<usize> {
    let mut matches = Vec::new();
    for (node_id, _) in cg.nodes_arena.iter().enumerate() {
        if cg.defined.contains(&node_id) && matches_node(cg, node_id, expected, matcher) {
            matches.push(node_id);
        }
    }
    matches
}

fn evaluate_expectation(cg: &CallGraph, expectation: &AccuracyExpectation) -> ExpectationResult {
    let edge_map = match expectation.kind {
        EdgeKind::Uses => &cg.uses_edges,
        EdgeKind::Defines => &cg.defines_edges,
    };

    let source_ids = resolve_matching_nodes(cg, &expectation.source, expectation.source_match);
    let target_ids = resolve_matching_nodes(cg, &expectation.target, expectation.target_match);

    let mut matched_target_ids = HashSet::new();
    for source_id in source_ids {
        if let Some(targets) = edge_map.get(&source_id) {
            for target_id in &target_ids {
                if targets.contains(target_id) {
                    matched_target_ids.insert(*target_id);
                }
            }
        }
    }

    let mut matched_target_names: Vec<String> = matched_target_ids
        .into_iter()
        .map(|target_id| cg.nodes_arena[target_id].get_name(&cg.interner).to_string())
        .collect();
    matched_target_names.sort();

    ExpectationResult {
        matched_targets: matched_target_names.len(),
        matched_target_names,
    }
}

#[test]
fn accuracy_manifest_cases_pass() {
    let manifest = load_manifest();
    let mut graph_cache: HashMap<GraphKey, CallGraph> = HashMap::new();
    let mut failures = Vec::new();
    let mut total_expectations = 0usize;

    for case in &manifest.cases {
        let key = GraphKey {
            files: case.files.clone(),
            root: case.root.clone(),
        };
        let cg = graph_cache.entry(key).or_insert_with(|| build_graph(case));

        for expectation in &case.expectations {
            total_expectations += 1;
            let result = evaluate_expectation(cg, expectation);
            let required_matches = expectation.min_matches.unwrap_or(1);
            let ok = if expectation.present {
                result.matched_targets >= required_matches
            } else {
                result.matched_targets == 0
            };

            if !ok {
                let expectation_type = if expectation.present {
                    format!("expected at least {required_matches} match(es)")
                } else {
                    "expected no matches".to_string()
                };
                failures.push(format!(
                    "{} [{}]: {:?} edge {} -> {} ({:?}/{:?}) failed: {}; matched {} target(s): {}",
                    case.id,
                    case.category,
                    expectation.kind,
                    expectation.source,
                    expectation.target,
                    expectation.source_match,
                    expectation.target_match,
                    expectation_type,
                    result.matched_targets,
                    if result.matched_target_names.is_empty() {
                        "<none>".to_string()
                    } else {
                        result.matched_target_names.join(", ")
                    }
                ));
            }
        }
    }

    if !failures.is_empty() {
        let mut message = String::new();
        let _ = writeln!(
            message,
            "{} of {} semantic accuracy expectation(s) failed:",
            failures.len(),
            total_expectations
        );
        for failure in failures {
            let _ = writeln!(message, "- {failure}");
        }
        panic!("{message}");
    }
}
