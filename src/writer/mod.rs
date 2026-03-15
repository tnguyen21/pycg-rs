//! Output writers for the visual call graph.
//!
//! Provides functions to serialize a [`VisualGraph`] into DOT (GraphViz),
//! TGF (Trivial Graph Format), plain text, and JSON.

use crate::analyzer::AnalysisDiagnostics;
use crate::intern::Interner;
use crate::node::{Node, NodeId};
use crate::visgraph::{VisualGraph, VisualNode};
use serde::Serialize;
use crate::{FxHashMap, FxHashSet};
use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// DOT writer
// ---------------------------------------------------------------------------

/// Render the visual graph in GraphViz DOT format.
///
/// `options` is a list of extra top-level graph attributes (e.g.
/// `rankdir=LR`).  When the graph is grouped, `clusterrank="local"` is
/// appended automatically.
pub fn write_dot(graph: &VisualGraph, options: &[String]) -> String {
    let mut out = String::new();

    // Collect graph-level options.
    let mut opts: Vec<String> = options.to_vec();
    if graph.grouped {
        opts.push("clusterrank=\"local\"".to_string());
    }
    let opts_str = opts.join(", ");

    writeln!(out, "digraph G {{").unwrap();
    writeln!(out, "    graph [{opts_str}];").unwrap();

    if graph.grouped && !graph.subgraphs.is_empty() {
        for sg in &graph.subgraphs {
            write_dot_subgraph(&mut out, sg, 1);
        }
    } else {
        // No subgraphs – emit all nodes at root level.
        for node in &graph.nodes {
            write_dot_node(&mut out, node, 1);
        }
    }

    // Edges (always at root level).
    for edge in &graph.edges {
        let src = &graph.nodes[edge.source_idx];
        let tgt = &graph.nodes[edge.target_idx];
        let style = if edge.flavor == "defines" {
            "dashed"
        } else {
            "solid"
        };
        let color = &edge.color;
        writeln!(
            out,
            "    {} -> {} [style=\"{style}\", color=\"{color}\"];",
            src.id, tgt.id
        )
        .unwrap();
    }

    writeln!(out, "}}").unwrap();
    out
}

fn indent(level: usize) -> String {
    "    ".repeat(level)
}

fn write_dot_node(out: &mut String, node: &VisualNode, level: usize) {
    let pad = indent(level);
    writeln!(
        out,
        "{pad}{id} [label=\"{label}\", style=\"filled\", fillcolor=\"{fill}\", fontcolor=\"{text}\", group=\"{group}\"];",
        id = node.id,
        label = node.label,
        fill = node.fill_color,
        text = node.text_color,
        group = node.group,
    )
    .unwrap();
}

fn write_dot_subgraph(out: &mut String, sg: &VisualGraph, level: usize) {
    let pad = indent(level);
    writeln!(out, "{pad}subgraph cluster_{id} {{", id = sg.id).unwrap();

    let inner = indent(level + 1);
    writeln!(
        out,
        "{inner}graph [style=\"filled,rounded\", fillcolor=\"#80808018\", label=\"{label}\"];",
        label = sg.label,
    )
    .unwrap();

    for node in &sg.nodes {
        write_dot_node(out, node, level + 1);
    }

    for child in &sg.subgraphs {
        write_dot_subgraph(out, child, level + 1);
    }

    writeln!(out, "{pad}}}").unwrap();
}

// ---------------------------------------------------------------------------
// TGF writer
// ---------------------------------------------------------------------------

/// Render the visual graph in Trivial Graph Format.
///
/// Nodes are numbered sequentially starting at 1.
pub fn write_tgf(graph: &VisualGraph) -> String {
    let mut out = String::new();

    // Assign sequential 1-based IDs.
    for (i, node) in graph.nodes.iter().enumerate() {
        writeln!(out, "{} {}", i + 1, node.label).unwrap();
    }

    writeln!(out, "#").unwrap();

    for edge in &graph.edges {
        let tag = if edge.flavor == "uses" { "U" } else { "D" };
        writeln!(out, "{} {} {tag}", edge.source_idx + 1, edge.target_idx + 1).unwrap();
    }

    out
}

// ---------------------------------------------------------------------------
// Text writer
// ---------------------------------------------------------------------------

/// Render the visual graph as a plain-text dependency list.
///
/// Each source node is printed on its own line, followed by its outgoing
/// edges indented with `[D]` (defines) or `[U]` (uses) tags.  Output is
/// sorted alphabetically by source label, then by (tag, target label).
pub fn write_text(graph: &VisualGraph) -> String {
    use std::collections::BTreeMap;

    // Build adjacency: source label → sorted Vec<(tag, target label)>.
    let mut adj: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();

    for edge in &graph.edges {
        let src_label = graph.nodes[edge.source_idx].label.as_str();
        let tgt_label = graph.nodes[edge.target_idx].label.as_str();
        let tag = if edge.flavor == "defines" { "D" } else { "U" };
        adj.entry(src_label).or_default().push((tag, tgt_label));
    }

    let mut out = String::new();
    for (src, targets) in &mut adj {
        targets.sort();
        writeln!(out, "{src}").unwrap();
        for (tag, tgt) in targets {
            writeln!(out, "    [{tag}] {tgt}").unwrap();
        }
    }

    out
}

// ---------------------------------------------------------------------------
// JSON writer
// ---------------------------------------------------------------------------

pub enum JsonGraphMode {
    Symbol,
    Module,
}

pub struct JsonOutputOptions<'a> {
    pub graph_mode: JsonGraphMode,
    pub analysis_root: Option<&'a str>,
    pub inputs: &'a [String],
}

struct PathFormatter {
    root: Option<PathBuf>,
    cwd: PathBuf,
    path_kind: &'static str,
}

impl PathFormatter {
    fn new(root: Option<&str>, inputs: &[String]) -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let root = root.map(|value| Self::resolve_path(&cwd, value));
        let path_kind = if root.is_some() {
            "root_relative"
        } else if inputs.iter().all(|value| !Path::new(value).is_absolute()) {
            "input_relative"
        } else {
            "absolute"
        };
        Self {
            root,
            cwd,
            path_kind,
        }
    }

    fn resolve_path(base: &Path, path: &str) -> PathBuf {
        let path = Path::new(path);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            base.join(path)
        }
    }

    fn format_analysis_root(&self, root: &str) -> String {
        let path = Self::resolve_path(&self.cwd, root);
        self.display_path(&path)
    }

    fn format_input(&self, input: &str) -> String {
        let path = Self::resolve_path(&self.cwd, input);
        self.format_graph_path(&path)
    }

    fn format_location(&self, path: &str) -> String {
        self.format_graph_path(&Self::resolve_path(&self.cwd, path))
    }

    fn format_graph_path(&self, path: &Path) -> String {
        if let Some(root) = &self.root
            && let Ok(relative) = path.strip_prefix(root)
        {
            return Self::path_to_string(relative);
        }

        match self.path_kind {
            "input_relative" => self.display_path(path),
            _ => Self::path_to_string(path),
        }
    }

    fn display_path(&self, path: &Path) -> String {
        if let Ok(relative) = path.strip_prefix(&self.cwd) {
            if relative.as_os_str().is_empty() {
                ".".to_string()
            } else {
                Self::path_to_string(relative)
            }
        } else {
            Self::path_to_string(path)
        }
    }

    fn path_to_string(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "/")
    }
}

#[derive(Serialize)]
struct JsonTool {
    name: &'static str,
    version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    commit: Option<&'static str>,
}

#[derive(Serialize)]
struct JsonAnalysis {
    #[serde(skip_serializing_if = "Option::is_none")]
    root: Option<String>,
    inputs: Vec<String>,
    node_inclusion_policy: &'static str,
    path_kind: &'static str,
}

#[derive(Serialize)]
struct JsonStats {
    nodes: usize,
    edges: usize,
    files_analyzed: usize,
    by_node_kind: BTreeMap<String, usize>,
    by_edge_kind: BTreeMap<String, usize>,
}

#[derive(Serialize)]
struct JsonLocation {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
}

#[derive(Serialize)]
struct JsonNode {
    id: String,
    kind: String,
    canonical_name: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    namespace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<JsonLocation>,
}

#[derive(Serialize)]
struct JsonEdge {
    kind: &'static str,
    source: String,
    target: String,
}

#[derive(Serialize)]
struct JsonDiagnosticSummary {
    warnings: usize,
    unresolved_references: usize,
    ambiguous_resolutions: usize,
    external_references: usize,
    approximations: usize,
}

#[derive(Serialize)]
struct JsonWarning {
    code: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
}

#[derive(Serialize)]
struct JsonUnresolvedReference {
    kind: String,
    source: String,
    symbol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
}

#[derive(Serialize)]
struct JsonAmbiguousResolution {
    kind: String,
    source: String,
    symbol: String,
    candidate_targets: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
}

#[derive(Serialize)]
struct JsonExternalReference {
    kind: String,
    source: String,
    canonical_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
}

#[derive(Serialize)]
struct JsonApproximation {
    kind: String,
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    symbol: Option<String>,
    reason: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    candidate_targets: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
}

#[derive(Serialize)]
struct JsonDiagnostics {
    summary: JsonDiagnosticSummary,
    warnings: Vec<JsonWarning>,
    unresolved_references: Vec<JsonUnresolvedReference>,
    ambiguous_resolutions: Vec<JsonAmbiguousResolution>,
    external_references: Vec<JsonExternalReference>,
    approximations: Vec<JsonApproximation>,
}

#[derive(Serialize)]
struct JsonGraph {
    schema_version: &'static str,
    tool: JsonTool,
    graph_mode: &'static str,
    analysis: JsonAnalysis,
    stats: JsonStats,
    nodes: Vec<JsonNode>,
    edges: Vec<JsonEdge>,
    diagnostics: JsonDiagnostics,
}

fn graph_mode_label(mode: &JsonGraphMode) -> &'static str {
    match mode {
        JsonGraphMode::Symbol => "symbol",
        JsonGraphMode::Module => "module",
    }
}

fn public_kind(node: &Node) -> Option<String> {
    use crate::node::Flavor;

    match node.flavor {
        Flavor::Module => Some("module".to_string()),
        Flavor::Class => Some("class".to_string()),
        Flavor::Function => Some("function".to_string()),
        Flavor::Method => Some("method".to_string()),
        Flavor::StaticMethod => Some("static_method".to_string()),
        Flavor::ClassMethod => Some("class_method".to_string()),
        _ => None,
    }
}

fn node_name_and_namespace(node: &Node, canonical_name: &str, interner: &Interner) -> (String, Option<String>) {
    if let Some(ns_id) = node.namespace {
        let ns_str = interner.resolve(ns_id);
        if !ns_str.is_empty() {
            return (interner.resolve(node.name).to_owned(), Some(ns_str.to_owned()));
        }
    }

    if let Some((namespace, name)) = canonical_name.rsplit_once('.') {
        return (name.to_string(), Some(namespace.to_string()));
    }

    (canonical_name.to_string(), None)
}

fn diagnostic_location(
    node: &Node,
    path_formatter: &PathFormatter,
) -> (Option<String>, Option<usize>) {
    (
        node.filename
            .as_ref()
            .map(|filename| path_formatter.format_location(filename)),
        node.line,
    )
}

fn build_json_diagnostics(
    nodes_arena: &[Node],
    defined: &FxHashSet<NodeId>,
    uses_edges: &FxHashMap<NodeId, FxHashSet<NodeId>>,
    node_ids: &FxHashMap<NodeId, String>,
    analyzer_diagnostics: &AnalysisDiagnostics,
    graph_mode: &JsonGraphMode,
    path_formatter: &PathFormatter,
    interner: &Interner,
) -> JsonDiagnostics {
    let mut warnings: Vec<JsonWarning> = Vec::new();
    let mut unresolved_references: Vec<JsonUnresolvedReference> = Vec::new();
    let mut external_references: Vec<JsonExternalReference> = Vec::new();
    let mut ambiguous_resolutions: Vec<JsonAmbiguousResolution> = Vec::new();
    let mut approximations: Vec<JsonApproximation> = Vec::new();

    let mut unresolved_seen: FxHashSet<(NodeId, String)> = FxHashSet::default();

    let canonical_name_to_output_id: FxHashMap<String, String> = node_ids
        .iter()
        .map(|(node_id, output_id)| (nodes_arena[*node_id].get_name(interner), output_id.clone()))
        .collect();
    let path_to_output_id: FxHashMap<String, String> = node_ids
        .iter()
        .filter_map(|(node_id, output_id)| {
            nodes_arena[*node_id]
                .filename
                .as_ref()
                .map(|filename| (path_formatter.format_location(filename), output_id.clone()))
        })
        .collect();
    let mut unresolved_suppressions: FxHashSet<(String, String)> = FxHashSet::default();

    for diagnostic in &analyzer_diagnostics.external_references {
        let source = match graph_mode {
            JsonGraphMode::Symbol => canonical_name_to_output_id
                .get(&diagnostic.source_canonical_name)
                .cloned(),
            JsonGraphMode::Module => diagnostic
                .source_filename
                .as_ref()
                .map(|filename| path_formatter.format_location(filename))
                .and_then(|path| path_to_output_id.get(&path).cloned()),
        };
        let Some(source) = source else {
            continue;
        };

        unresolved_suppressions.insert((source.clone(), diagnostic.canonical_name.clone()));
        if matches!(
            diagnostic.kind,
            crate::analyzer::ExternalReferenceKind::Import
        ) && let Some((_, short_name)) = diagnostic.canonical_name.rsplit_once('.')
        {
            unresolved_suppressions.insert((source.clone(), short_name.to_string()));
        }

        external_references.push(JsonExternalReference {
            kind: diagnostic.kind.as_str().to_string(),
            source,
            canonical_name: diagnostic.canonical_name.clone(),
            path: diagnostic
                .source_filename
                .as_ref()
                .map(|filename| path_formatter.format_location(filename)),
            line: diagnostic.source_line,
        });
    }

    let mut source_ids: Vec<NodeId> = uses_edges.keys().copied().collect();
    source_ids.sort_unstable();

    for source in source_ids {
        if !defined.contains(&source) {
            continue;
        }
        let Some(source_id) = node_ids.get(&source).cloned() else {
            continue;
        };
        let source_node = &nodes_arena[source];
        let (path, line) = diagnostic_location(source_node, path_formatter);

        let mut targets: Vec<NodeId> = uses_edges
            .get(&source)
            .map(|targets| targets.iter().copied().collect())
            .unwrap_or_default();
        targets.sort_unstable_by(|a, b| {
            let left = &nodes_arena[*a];
            let right = &nodes_arena[*b];
            let left_ns = left.namespace.map(|id| interner.resolve(id));
            let right_ns = right.namespace.map(|id| interner.resolve(id));
            let left_name = interner.resolve(left.name);
            let right_name = interner.resolve(right.name);
            (left_ns, left_name, left.flavor.specificity()).cmp(&(
                right_ns,
                right_name,
                right.flavor.specificity(),
            ))
        });

        let mut concrete_groups: BTreeMap<String, Vec<NodeId>> = BTreeMap::new();

        for target in targets {
            let node = &nodes_arena[target];
            if node.namespace.is_none() {
                // Implicit constructor lookup frequently creates a synthetic
                // unresolved `__init__` edge when a class has no explicit
                // initializer. That is implementation noise, not a useful
                // refactor diagnostic.
                let name_str = interner.resolve(node.name);
                if name_str == "__init__"
                    || name_str.starts_with("^^^")
                    || unresolved_suppressions.contains(&(source_id.clone(), name_str.to_owned()))
                {
                    continue;
                }
                let key = (source, name_str.to_owned());
                if unresolved_seen.insert(key) {
                    unresolved_references.push(JsonUnresolvedReference {
                        kind: "use".to_string(),
                        source: source_id.clone(),
                        symbol: name_str.to_owned(),
                        path: path.clone(),
                        line,
                    });
                }
                continue;
            }

            concrete_groups
                .entry(interner.resolve(node.name).to_owned())
                .or_default()
                .push(target);
        }

        for (symbol, mut candidate_targets) in concrete_groups {
            candidate_targets.sort_unstable();
            candidate_targets.dedup();
            if candidate_targets.len() < 2 {
                continue;
            }

            let candidate_target_ids: Vec<String> = candidate_targets
                .iter()
                .filter_map(|target| node_ids.get(target).cloned())
                .collect();
            if candidate_target_ids.len() < 2 {
                continue;
            }

            ambiguous_resolutions.push(JsonAmbiguousResolution {
                kind: "use".to_string(),
                source: source_id.clone(),
                symbol: symbol.clone(),
                candidate_targets: candidate_target_ids.clone(),
                path: path.clone(),
                line,
            });
            approximations.push(JsonApproximation {
                kind: "resolution_widening".to_string(),
                source: source_id.clone(),
                symbol: Some(symbol),
                reason: "multiple_candidate_targets".to_string(),
                candidate_targets: candidate_target_ids,
                path: path.clone(),
                line,
            });
        }
    }

    warnings.sort_by(|a, b| {
        (&a.code, &a.message, &a.path, a.line).cmp(&(&b.code, &b.message, &b.path, b.line))
    });
    unresolved_references.sort_by(|a, b| {
        (&a.source, &a.symbol, &a.path, a.line).cmp(&(&b.source, &b.symbol, &b.path, b.line))
    });
    ambiguous_resolutions.sort_by(|a, b| {
        (&a.source, &a.symbol, &a.path, a.line, &a.candidate_targets).cmp(&(
            &b.source,
            &b.symbol,
            &b.path,
            b.line,
            &b.candidate_targets,
        ))
    });
    external_references.sort_by(|a, b| {
        (&a.source, &a.canonical_name, &a.path, a.line).cmp(&(
            &b.source,
            &b.canonical_name,
            &b.path,
            b.line,
        ))
    });
    approximations.sort_by(|a, b| {
        (
            &a.source,
            &a.reason,
            &a.symbol,
            &a.path,
            a.line,
            &a.candidate_targets,
        )
            .cmp(&(
                &b.source,
                &b.reason,
                &b.symbol,
                &b.path,
                b.line,
                &b.candidate_targets,
            ))
    });

    JsonDiagnostics {
        summary: JsonDiagnosticSummary {
            warnings: warnings.len(),
            unresolved_references: unresolved_references.len(),
            ambiguous_resolutions: ambiguous_resolutions.len(),
            external_references: external_references.len(),
            approximations: approximations.len(),
        },
        warnings,
        unresolved_references,
        ambiguous_resolutions,
        external_references,
        approximations,
    }
}

/// Render the call graph directly as JSON.
///
/// Unlike the other writers which operate on the visual graph, this serializes
/// the raw call graph data for machine consumption.
pub fn write_json(
    nodes_arena: &[Node],
    defined: &FxHashSet<NodeId>,
    defines_edges: &FxHashMap<NodeId, FxHashSet<NodeId>>,
    uses_edges: &FxHashMap<NodeId, FxHashSet<NodeId>>,
    analyzer_diagnostics: &AnalysisDiagnostics,
    options: &JsonOutputOptions<'_>,
    interner: &Interner,
) -> String {
    let path_formatter = PathFormatter::new(options.analysis_root, options.inputs);
    let mut nodes = Vec::new();
    let mut sorted_ids: Vec<NodeId> = defined.iter().copied().collect();
    sorted_ids.sort_by(|&a, &b| {
        let na = &nodes_arena[a];
        let nb = &nodes_arena[b];
        let na_ns = na.namespace.map(|id| interner.resolve(id));
        let nb_ns = nb.namespace.map(|id| interner.resolve(id));
        let na_name = interner.resolve(na.name);
        let nb_name = interner.resolve(nb.name);
        (na_ns, na_name).cmp(&(nb_ns, nb_name))
    });

    let mut files: FxHashSet<&str> = FxHashSet::default();
    let mut node_kind_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut node_ids = FxHashMap::default();

    for (index, &id) in sorted_ids.iter().enumerate() {
        node_ids.insert(id, format!("n{}", index + 1));
    }

    for &id in &sorted_ids {
        let n = &nodes_arena[id];
        let canonical_name = n.get_name(interner);
        let (name, namespace) = node_name_and_namespace(n, &canonical_name, interner);
        if let Some(ref f) = n.filename {
            files.insert(f.as_str());
        }
        let kind = public_kind(n).unwrap_or_else(|| "unknown".to_string());
        *node_kind_counts.entry(kind.clone()).or_insert(0) += 1;
        nodes.push(JsonNode {
            id: node_ids
                .get(&id)
                .expect("sorted node should have an assigned id")
                .clone(),
            kind,
            canonical_name,
            name,
            namespace,
            location: n.filename.as_ref().map(|filename| JsonLocation {
                path: path_formatter.format_location(filename),
                line: n.line,
            }),
        });
    }

    let defined_set: &FxHashSet<NodeId> = defined;
    let mut edges = Vec::new();
    let mut edge_kind_counts: BTreeMap<String, usize> = BTreeMap::new();

    for (&src, targets) in defines_edges {
        if !defined_set.contains(&src) {
            continue;
        }
        for &tgt in targets {
            if !defined_set.contains(&tgt) {
                continue;
            }
            edges.push(JsonEdge {
                kind: "defines",
                source: node_ids
                    .get(&src)
                    .expect("defined source node should have an assigned id")
                    .clone(),
                target: node_ids
                    .get(&tgt)
                    .expect("defined target node should have an assigned id")
                    .clone(),
            });
            *edge_kind_counts.entry("defines".to_string()).or_insert(0) += 1;
        }
    }

    for (&src, targets) in uses_edges {
        if !defined_set.contains(&src) {
            continue;
        }
        for &tgt in targets {
            if !defined_set.contains(&tgt) {
                continue;
            }
            edges.push(JsonEdge {
                kind: "uses",
                source: node_ids
                    .get(&src)
                    .expect("defined source node should have an assigned id")
                    .clone(),
                target: node_ids
                    .get(&tgt)
                    .expect("defined target node should have an assigned id")
                    .clone(),
            });
            *edge_kind_counts.entry("uses".to_string()).or_insert(0) += 1;
        }
    }

    edges.sort_by(|a, b| (&a.source, &a.target, a.kind).cmp(&(&b.source, &b.target, b.kind)));

    let diagnostics = build_json_diagnostics(
        nodes_arena,
        defined_set,
        uses_edges,
        &node_ids,
        analyzer_diagnostics,
        &options.graph_mode,
        &path_formatter,
        interner,
    );

    let graph = JsonGraph {
        schema_version: "1",
        tool: JsonTool {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
            commit: option_env!("PYCG_RS_GIT_COMMIT"),
        },
        graph_mode: graph_mode_label(&options.graph_mode),
        analysis: JsonAnalysis {
            root: options
                .analysis_root
                .map(|root| path_formatter.format_analysis_root(root)),
            inputs: options
                .inputs
                .iter()
                .map(|input| path_formatter.format_input(input))
                .collect(),
            node_inclusion_policy: "defined_only",
            path_kind: path_formatter.path_kind,
        },
        stats: JsonStats {
            nodes: nodes.len(),
            edges: edges.len(),
            files_analyzed: files.len(),
            by_node_kind: node_kind_counts,
            by_edge_kind: edge_kind_counts,
        },
        nodes,
        edges,
        diagnostics,
    };

    serde_json::to_string_pretty(&graph).expect("JSON serialization failed")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intern::Interner;
    use crate::node::{Flavor, Node};
    use crate::visgraph::VisualOptions;
    use crate::{FxHashMap, FxHashSet};

    fn make_test_graph() -> VisualGraph {
        let mut interner = Interner::new();
        let pkg = interner.intern("pkg");
        let other = interner.intern("other");
        let foo = interner.intern("Foo");
        let bar = interner.intern("bar");
        let baz = interner.intern("baz");

        let nodes_arena = vec![
            Node::new(Some(pkg), foo, Flavor::Class).with_location("pkg.py", 1),
            Node::new(Some(pkg), bar, Flavor::Function).with_location("pkg.py", 10),
            Node::new(Some(other), baz, Flavor::Function).with_location("other.py", 5),
        ];
        let mut defined = FxHashSet::default();
        defined.insert(0);
        defined.insert(1);
        defined.insert(2);

        let mut uses = FxHashMap::default();
        uses.entry(0).or_insert_with(FxHashSet::default).insert(1);
        uses.entry(1).or_insert_with(FxHashSet::default).insert(2);

        let mut defines = FxHashMap::default();
        defines.entry(0).or_insert_with(FxHashSet::default).insert(1);

        let options = VisualOptions {
            draw_defines: true,
            draw_uses: true,
            colored: true,
            grouped: false,
            annotated: false,
        };

        VisualGraph::from_call_graph(&nodes_arena, &defined, &defines, &uses, &options, &interner)
    }

    #[test]
    fn test_dot_output_structure() {
        let g = make_test_graph();
        let dot = write_dot(&g, &["rankdir=TB".to_string()]);
        assert!(dot.starts_with("digraph G {"));
        assert!(dot.contains("rankdir=TB"));
        assert!(dot.contains("style=\"filled\""));
        assert!(dot.ends_with("}\n"));
    }

    #[test]
    fn test_dot_grouped() {
        let mut interner = Interner::new();
        let pkg = interner.intern("pkg");
        let other = interner.intern("other");
        let a = interner.intern("A");
        let b = interner.intern("B");

        let nodes_arena = vec![
            Node::new(Some(pkg), a, Flavor::Class).with_location("pkg.py", 1),
            Node::new(Some(other), b, Flavor::Function).with_location("other.py", 5),
        ];
        let mut defined = FxHashSet::default();
        defined.insert(0);
        defined.insert(1);

        let options = VisualOptions {
            draw_defines: false,
            draw_uses: false,
            colored: false,
            grouped: true,
            annotated: false,
        };

        let g = VisualGraph::from_call_graph(
            &nodes_arena,
            &defined,
            &FxHashMap::default(),
            &FxHashMap::default(),
            &options,
            &interner,
        );
        let dot = write_dot(&g, &[]);
        assert!(dot.contains("subgraph cluster_"));
        assert!(dot.contains("clusterrank=\"local\""));
    }

    #[test]
    fn test_tgf_output() {
        let g = make_test_graph();
        let tgf = write_tgf(&g);
        // Should contain node lines, separator, and edge lines.
        assert!(tgf.contains("#\n"));
        // Nodes are 1-indexed.
        assert!(tgf.contains("1 "));
    }

    #[test]
    fn test_text_output() {
        let g = make_test_graph();
        let text = write_text(&g);
        // Should contain [U] and [D] tags.
        assert!(text.contains("[U]") || text.contains("[D]"));
    }
}
