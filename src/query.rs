use crate::analyzer::{CallGraph, ExternalReferenceKind};
use crate::intern::Interner;
use crate::node::{Flavor, Node, NodeId};
use serde::Serialize;
use crate::{FxHashMap, FxHashSet};
use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchMode {
    Exact,
    Suffix,
}

impl MatchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Suffix => "suffix",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryGraphMode {
    Symbol,
    Module,
}

impl QueryGraphMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Symbol => "symbol",
            Self::Module => "module",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetKind {
    Path,
    Module,
}

impl TargetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::Module => "module",
        }
    }
}

pub struct QueryRenderOptions<'a> {
    pub analysis_root: Option<&'a str>,
    pub inputs: &'a [String],
}

struct PathFormatter {
    root: Option<PathBuf>,
    cwd: PathBuf,
}

impl PathFormatter {
    fn new(root: Option<&str>) -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let root = root.map(|value| Self::resolve_path(&cwd, value));
        Self { root, cwd }
    }

    fn resolve_path(base: &Path, path: &str) -> PathBuf {
        let path = Path::new(path);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            base.join(path)
        }
    }

    fn normalize_for_match(&self, path: &str) -> PathBuf {
        Self::resolve_path(&self.cwd, path)
    }

    fn format_location(&self, path: &str) -> String {
        let path = Self::resolve_path(&self.cwd, path);
        if let Some(root) = &self.root
            && let Ok(relative) = path.strip_prefix(root)
        {
            return Self::path_to_string(relative);
        }

        if let Ok(relative) = path.strip_prefix(&self.cwd) {
            if relative.as_os_str().is_empty() {
                ".".to_string()
            } else {
                Self::path_to_string(relative)
            }
        } else {
            Self::path_to_string(&path)
        }
    }

    fn path_to_string(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "/")
    }
}

#[derive(Clone, Serialize)]
struct Location {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
}

#[derive(Clone, Serialize)]
struct SymbolRef {
    canonical_name: String,
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    namespace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<Location>,
}

#[derive(Clone, Serialize)]
struct DiagnosticSummary {
    warnings: usize,
    unresolved_references: usize,
    ambiguous_resolutions: usize,
    external_references: usize,
    approximations: usize,
}

#[derive(Clone, Serialize, Default)]
struct Warning {
    code: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
}

#[derive(Clone, Serialize)]
struct UnresolvedReference {
    kind: String,
    source: String,
    symbol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
}

#[derive(Clone, Serialize)]
struct AmbiguousResolution {
    kind: String,
    source: String,
    symbol: String,
    candidate_targets: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
}

#[derive(Clone, Serialize)]
struct ExternalReference {
    kind: String,
    source: String,
    canonical_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
}

#[derive(Clone, Serialize)]
struct Approximation {
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

#[derive(Clone, Serialize)]
struct QueryDiagnostics {
    summary: DiagnosticSummary,
    warnings: Vec<Warning>,
    unresolved_references: Vec<UnresolvedReference>,
    ambiguous_resolutions: Vec<AmbiguousResolution>,
    external_references: Vec<ExternalReference>,
    approximations: Vec<Approximation>,
}

#[derive(Clone, Serialize)]
struct QueryError {
    code: String,
    message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    matches: Vec<String>,
}

#[derive(Clone, Serialize)]
struct TargetQuery {
    target: String,
    target_kind: String,
    graph_mode: String,
}

#[derive(Clone, Serialize)]
struct SymbolQuery {
    symbol: String,
    match_mode: String,
    graph_mode: String,
}

#[derive(Clone, Serialize)]
struct PathQuery {
    source: String,
    target: String,
    match_mode: String,
    graph_mode: String,
}

#[derive(Clone, Serialize)]
struct OutgoingEdge {
    kind: String,
    target: SymbolRef,
}

#[derive(Clone, Serialize)]
struct IncomingEdge {
    kind: String,
    source: SymbolRef,
}

#[derive(Clone, Serialize)]
struct SymbolStat {
    canonical_name: String,
    kind: String,
    caller_count: usize,
    callee_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<Location>,
}

#[derive(Clone, Serialize)]
struct SummaryPayload {
    file_count: usize,
    symbol_counts: BTreeMap<String, usize>,
    edge_counts: BTreeMap<String, usize>,
    top_level_symbols: Vec<SymbolRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    symbol_stats: Option<Vec<SymbolStat>>,
}

#[derive(Clone, Serialize)]
struct PathEdge {
    kind: String,
    source: String,
    target: String,
}

#[derive(Clone, Serialize)]
struct PathResult {
    nodes: Vec<SymbolRef>,
    edges: Vec<PathEdge>,
}

enum QueryDocument<Q, P> {
    Ok {
        query: Q,
        payload: P,
        diagnostics: QueryDiagnostics,
    },
    Error {
        query: Q,
        error: QueryError,
    },
}

enum QueryResponseInner {
    SymbolsIn(QueryDocument<TargetQuery, Vec<SymbolRef>>),
    Summary(QueryDocument<TargetQuery, SummaryPayload>),
    Callees(QueryDocument<SymbolQuery, (SymbolRef, Vec<OutgoingEdge>)>),
    Callers(QueryDocument<SymbolQuery, (SymbolRef, Vec<IncomingEdge>)>),
    Neighbors(QueryDocument<SymbolQuery, (SymbolRef, Vec<IncomingEdge>, Vec<OutgoingEdge>)>),
    Path(QueryDocument<PathQuery, Vec<PathResult>>),
}

pub struct QueryResponse(QueryResponseInner);

impl QueryResponse {
    pub fn is_error(&self) -> bool {
        match &self.0 {
            QueryResponseInner::SymbolsIn(doc) => matches!(doc, QueryDocument::Error { .. }),
            QueryResponseInner::Summary(doc) => matches!(doc, QueryDocument::Error { .. }),
            QueryResponseInner::Callees(doc) => matches!(doc, QueryDocument::Error { .. }),
            QueryResponseInner::Callers(doc) => matches!(doc, QueryDocument::Error { .. }),
            QueryResponseInner::Neighbors(doc) => matches!(doc, QueryDocument::Error { .. }),
            QueryResponseInner::Path(doc) => matches!(doc, QueryDocument::Error { .. }),
        }
    }

    pub fn render_json(&self) -> String {
        match &self.0 {
            QueryResponseInner::SymbolsIn(doc) => {
                render_json_document("symbols_in", doc, |query, symbols, diagnostics| {
                    serde_json::json!({
                        "schema_version": "1",
                        "query_kind": "symbols_in",
                        "status": "ok",
                        "query": query,
                        "symbols": symbols,
                        "diagnostics": diagnostics,
                    })
                })
            }
            QueryResponseInner::Summary(doc) => {
                render_json_document("summary", doc, |query, summary, diagnostics| {
                    serde_json::json!({
                        "schema_version": "1",
                        "query_kind": "summary",
                        "status": "ok",
                        "query": query,
                        "summary": summary,
                        "diagnostics": diagnostics,
                    })
                })
            }
            QueryResponseInner::Callees(doc) => {
                render_json_document("callees", doc, |query, (node, edges), diagnostics| {
                    serde_json::json!({
                        "schema_version": "1",
                        "query_kind": "callees",
                        "status": "ok",
                        "query": query,
                        "node": node,
                        "edges": edges,
                        "diagnostics": diagnostics,
                    })
                })
            }
            QueryResponseInner::Callers(doc) => {
                render_json_document("callers", doc, |query, (node, edges), diagnostics| {
                    serde_json::json!({
                        "schema_version": "1",
                        "query_kind": "callers",
                        "status": "ok",
                        "query": query,
                        "node": node,
                        "edges": edges,
                        "diagnostics": diagnostics,
                    })
                })
            }
            QueryResponseInner::Neighbors(doc) => render_json_document(
                "neighbors",
                doc,
                |query, (node, incoming, outgoing), diagnostics| {
                    serde_json::json!({
                        "schema_version": "1",
                        "query_kind": "neighbors",
                        "status": "ok",
                        "query": query,
                        "node": node,
                        "incoming": incoming,
                        "outgoing": outgoing,
                        "diagnostics": diagnostics,
                    })
                },
            ),
            QueryResponseInner::Path(doc) => {
                render_json_document("path", doc, |query, paths, diagnostics| {
                    serde_json::json!({
                        "schema_version": "1",
                        "query_kind": "path",
                        "status": "ok",
                        "query": query,
                        "paths": paths,
                        "diagnostics": diagnostics,
                    })
                })
            }
        }
    }

    pub fn render_text(&self) -> String {
        match &self.0 {
            QueryResponseInner::SymbolsIn(doc) => match doc {
                QueryDocument::Ok { payload, .. } => {
                    let mut out = String::new();
                    for symbol in payload {
                        writeln!(out, "{}", symbol.canonical_name).unwrap();
                    }
                    out
                }
                QueryDocument::Error { error, .. } => format!("error: {}\n", error.message),
            },
            QueryResponseInner::Summary(doc) => match doc {
                QueryDocument::Ok { payload, .. } => {
                    let mut out = String::new();
                    writeln!(out, "files: {}", payload.file_count).unwrap();
                    writeln!(out, "symbols:").unwrap();
                    for (kind, count) in &payload.symbol_counts {
                        writeln!(out, "  {kind}: {count}").unwrap();
                    }
                    if !payload.edge_counts.is_empty() {
                        writeln!(out, "edges:").unwrap();
                        for (kind, count) in &payload.edge_counts {
                            writeln!(out, "  {kind}: {count}").unwrap();
                        }
                    }
                    if !payload.top_level_symbols.is_empty() {
                        writeln!(out, "top-level:").unwrap();
                        for symbol in &payload.top_level_symbols {
                            writeln!(out, "  {}", symbol.canonical_name).unwrap();
                        }
                    }
                    if let Some(stats) = &payload.symbol_stats {
                        let max_name = stats
                            .iter()
                            .map(|s| s.canonical_name.len())
                            .max()
                            .unwrap_or(0);
                        let max_kind = stats.iter().map(|s| s.kind.len()).max().unwrap_or(0);
                        writeln!(out, "symbol stats (by caller count):").unwrap();
                        for stat in stats {
                            writeln!(
                                out,
                                "  {:<name_w$}  {:<kind_w$}  callers:{}  callees:{}",
                                stat.canonical_name,
                                stat.kind,
                                stat.caller_count,
                                stat.callee_count,
                                name_w = max_name,
                                kind_w = max_kind,
                            )
                            .unwrap();
                        }
                    }
                    out
                }
                QueryDocument::Error { error, .. } => format!("error: {}\n", error.message),
            },
            QueryResponseInner::Callees(doc) => match doc {
                QueryDocument::Ok {
                    payload: (node, edges),
                    ..
                } => {
                    let mut out = String::new();
                    writeln!(out, "{}", node.canonical_name).unwrap();
                    for edge in edges {
                        writeln!(out, "  [{}] {}", edge.kind, edge.target.canonical_name).unwrap();
                    }
                    out
                }
                QueryDocument::Error { error, .. } => format!("error: {}\n", error.message),
            },
            QueryResponseInner::Callers(doc) => match doc {
                QueryDocument::Ok {
                    payload: (node, edges),
                    ..
                } => {
                    let mut out = String::new();
                    writeln!(out, "{}", node.canonical_name).unwrap();
                    for edge in edges {
                        writeln!(out, "  [{}] {}", edge.kind, edge.source.canonical_name).unwrap();
                    }
                    out
                }
                QueryDocument::Error { error, .. } => format!("error: {}\n", error.message),
            },
            QueryResponseInner::Neighbors(doc) => match doc {
                QueryDocument::Ok {
                    payload: (node, incoming, outgoing),
                    ..
                } => {
                    let mut out = String::new();
                    writeln!(out, "{}", node.canonical_name).unwrap();
                    writeln!(out, "incoming:").unwrap();
                    for edge in incoming {
                        writeln!(out, "  [{}] {}", edge.kind, edge.source.canonical_name).unwrap();
                    }
                    writeln!(out, "outgoing:").unwrap();
                    for edge in outgoing {
                        writeln!(out, "  [{}] {}", edge.kind, edge.target.canonical_name).unwrap();
                    }
                    out
                }
                QueryDocument::Error { error, .. } => format!("error: {}\n", error.message),
            },
            QueryResponseInner::Path(doc) => match doc {
                QueryDocument::Ok { payload, .. } => {
                    let mut out = String::new();
                    for (index, path) in payload.iter().enumerate() {
                        if index > 0 {
                            writeln!(out).unwrap();
                        }
                        let names: Vec<&str> = path
                            .nodes
                            .iter()
                            .map(|node| node.canonical_name.as_str())
                            .collect();
                        writeln!(out, "{}", names.join(" -> ")).unwrap();
                    }
                    out
                }
                QueryDocument::Error { error, .. } => format!("error: {}\n", error.message),
            },
        }
    }
}

fn render_json_document<Q: Serialize, P, F>(
    query_kind: &str,
    doc: &QueryDocument<Q, P>,
    ok_builder: F,
) -> String
where
    F: FnOnce(&Q, &P, &QueryDiagnostics) -> serde_json::Value,
{
    let value = match doc {
        QueryDocument::Ok {
            query,
            payload,
            diagnostics,
            ..
        } => ok_builder(query, payload, diagnostics),
        QueryDocument::Error { query, error } => serde_json::json!({
            "schema_version": "1",
            "query_kind": query_kind,
            "status": "error",
            "query": query,
            "error": error,
        }),
    };
    serde_json::to_string_pretty(&value).expect("query JSON serialization should succeed")
}

fn public_kind(node: &Node) -> Option<&'static str> {
    match node.flavor {
        Flavor::Module => Some("module"),
        Flavor::Class => Some("class"),
        Flavor::Function => Some("function"),
        Flavor::Method => Some("method"),
        Flavor::StaticMethod => Some("static_method"),
        Flavor::ClassMethod => Some("class_method"),
        _ => None,
    }
}

fn node_name_and_namespace(node: &Node, canonical_name: &str, interner: &Interner) -> (Option<String>, Option<String>) {
    if let Some(ns_id) = node.namespace {
        let ns_str = interner.resolve(ns_id);
        if !ns_str.is_empty() {
            return (Some(interner.resolve(node.name).to_owned()), Some(ns_str.to_owned()));
        }
    }

    if let Some((namespace, name)) = canonical_name.rsplit_once('.') {
        return (Some(name.to_string()), Some(namespace.to_string()));
    }

    (Some(canonical_name.to_string()), None)
}

fn symbol_ref(node: &Node, formatter: &PathFormatter, interner: &Interner) -> Option<SymbolRef> {
    let canonical_name = node.get_name(interner);
    let kind = public_kind(node)?.to_string();
    let (name, namespace) = node_name_and_namespace(node, &canonical_name, interner);
    Some(SymbolRef {
        canonical_name,
        kind,
        name,
        namespace,
        location: node.filename.as_ref().map(|path| Location {
            path: formatter.format_location(path),
            line: node.line,
        }),
    })
}

fn defined_public_node_ids(cg: &CallGraph) -> Vec<NodeId> {
    let mut ids: Vec<NodeId> = cg
        .defined
        .iter()
        .copied()
        .filter(|id| public_kind(&cg.nodes_arena[*id]).is_some())
        .collect();
    ids.sort_by(|a, b| {
        cg.nodes_arena[*a]
            .get_name(&cg.interner)
            .cmp(&cg.nodes_arena[*b].get_name(&cg.interner))
    });
    ids
}

fn resolve_symbol_matches(cg: &CallGraph, symbol: &str, match_mode: MatchMode) -> Vec<NodeId> {
    let mut ids: Vec<NodeId> = defined_public_node_ids(cg)
        .into_iter()
        .filter(|id| {
            let canonical_name = cg.nodes_arena[*id].get_name(&cg.interner);
            match match_mode {
                MatchMode::Exact => canonical_name == symbol,
                MatchMode::Suffix => {
                    canonical_name == symbol || canonical_name.ends_with(&format!(".{symbol}"))
                }
            }
        })
        .collect();
    ids.sort_by(|a, b| {
        cg.nodes_arena[*a]
            .get_name(&cg.interner)
            .cmp(&cg.nodes_arena[*b].get_name(&cg.interner))
    });
    ids
}

fn query_error(code: &str, message: impl Into<String>, matches: Vec<String>) -> QueryError {
    QueryError {
        code: code.to_string(),
        message: message.into(),
        matches,
    }
}

fn resolve_single_symbol(
    cg: &CallGraph,
    symbol: &str,
    match_mode: MatchMode,
) -> Result<NodeId, QueryError> {
    let matches = resolve_symbol_matches(cg, symbol, match_mode);
    match matches.as_slice() {
        [] => Err(query_error(
            "symbol_not_found",
            format!("No symbol matched query '{symbol}'"),
            Vec::new(),
        )),
        [id] => Ok(*id),
        many => Err(query_error(
            "ambiguous_query",
            format!("Query '{symbol}' matched multiple symbols"),
            many.iter()
                .map(|id| cg.nodes_arena[*id].get_name(&cg.interner))
                .collect(),
        )),
    }
}

fn path_matches_target(path: &str, target: &str, formatter: &PathFormatter) -> bool {
    let node_path = formatter.normalize_for_match(path);
    let target_path = formatter.normalize_for_match(target);
    node_path == target_path || node_path.starts_with(&target_path)
}

fn module_matches_target(module_name: &str, target: &str) -> bool {
    module_name == target || module_name.starts_with(&format!("{target}."))
}

fn target_query(target: &str, target_kind: TargetKind, graph_mode: QueryGraphMode) -> TargetQuery {
    TargetQuery {
        target: target.to_string(),
        target_kind: target_kind.as_str().to_string(),
        graph_mode: graph_mode.as_str().to_string(),
    }
}

fn symbol_query(symbol: &str, match_mode: MatchMode) -> SymbolQuery {
    SymbolQuery {
        symbol: symbol.to_string(),
        match_mode: match_mode.as_str().to_string(),
        graph_mode: QueryGraphMode::Symbol.as_str().to_string(),
    }
}

fn path_query(source: &str, target: &str, match_mode: MatchMode) -> PathQuery {
    PathQuery {
        source: source.to_string(),
        target: target.to_string(),
        match_mode: match_mode.as_str().to_string(),
        graph_mode: QueryGraphMode::Symbol.as_str().to_string(),
    }
}

fn collect_query_diagnostics(
    cg: &CallGraph,
    relevant_source_ids: &FxHashSet<NodeId>,
    relevant_paths: &FxHashSet<String>,
    formatter: &PathFormatter,
) -> QueryDiagnostics {
    let mut warnings: Vec<Warning> = Vec::new();
    let mut unresolved_references: Vec<UnresolvedReference> = Vec::new();
    let mut ambiguous_resolutions: Vec<AmbiguousResolution> = Vec::new();
    let mut external_references: Vec<ExternalReference> = Vec::new();
    let mut approximations: Vec<Approximation> = Vec::new();

    let relevant_source_names: FxHashSet<String> = relevant_source_ids
        .iter()
        .map(|id| cg.nodes_arena[*id].get_name(&cg.interner))
        .collect();

    let mut unresolved_suppressions: FxHashSet<(String, String)> = FxHashSet::default();
    for diagnostic in &cg.diagnostics.external_references {
        let formatted_path = diagnostic
            .source_filename
            .as_ref()
            .map(|path| formatter.format_location(path));
        if !relevant_source_names.contains(&diagnostic.source_canonical_name)
            && !formatted_path
                .as_ref()
                .is_some_and(|path| relevant_paths.contains(path))
        {
            continue;
        }

        unresolved_suppressions.insert((
            diagnostic.source_canonical_name.clone(),
            diagnostic.canonical_name.clone(),
        ));
        if matches!(diagnostic.kind, ExternalReferenceKind::Import)
            && let Some((_, short_name)) = diagnostic.canonical_name.rsplit_once('.')
        {
            unresolved_suppressions.insert((
                diagnostic.source_canonical_name.clone(),
                short_name.to_string(),
            ));
        }

        external_references.push(ExternalReference {
            kind: diagnostic.kind.as_str().to_string(),
            source: diagnostic.source_canonical_name.clone(),
            canonical_name: diagnostic.canonical_name.clone(),
            path: formatted_path,
            line: diagnostic.source_line,
        });
    }

    let mut source_ids: Vec<NodeId> = relevant_source_ids.iter().copied().collect();
    source_ids.sort_unstable();

    for source_id in source_ids {
        let source_node = &cg.nodes_arena[source_id];
        let source_name = source_node.get_name(&cg.interner);
        let path = source_node
            .filename
            .as_ref()
            .map(|filename| formatter.format_location(filename));
        let line = source_node.line;
        let mut targets: Vec<NodeId> = cg
            .uses_edges
            .get(&source_id)
            .map(|targets| targets.iter().copied().collect())
            .unwrap_or_default();
        targets.sort_unstable_by(|a, b| {
            let left = &cg.nodes_arena[*a];
            let right = &cg.nodes_arena[*b];
            (&left.namespace, &left.name, left.flavor.specificity()).cmp(&(
                &right.namespace,
                &right.name,
                right.flavor.specificity(),
            ))
        });

        let mut concrete_groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut unresolved_seen: FxHashSet<String> = FxHashSet::default();

        for target_id in targets {
            let target = &cg.nodes_arena[target_id];
            if target.namespace.is_none() {
                let target_name_str = cg.interner.resolve(target.name).to_owned();
                if target_name_str == "__init__"
                    || target_name_str.starts_with("^^^")
                    || unresolved_suppressions.contains(&(source_name.clone(), target_name_str.clone()))
                {
                    continue;
                }
                if unresolved_seen.insert(target_name_str.clone()) {
                    unresolved_references.push(UnresolvedReference {
                        kind: "use".to_string(),
                        source: source_name.clone(),
                        symbol: target_name_str,
                        path: path.clone(),
                        line,
                    });
                }
                continue;
            }

            if cg.defined.contains(&target_id) {
                let target_name_str = cg.interner.resolve(target.name).to_owned();
                concrete_groups
                    .entry(target_name_str)
                    .or_default()
                    .push(target.get_name(&cg.interner));
            }
        }

        for (symbol, mut candidate_targets) in concrete_groups {
            candidate_targets.sort();
            candidate_targets.dedup();
            if candidate_targets.len() < 2 {
                continue;
            }

            ambiguous_resolutions.push(AmbiguousResolution {
                kind: "use".to_string(),
                source: source_name.clone(),
                symbol: symbol.clone(),
                candidate_targets: candidate_targets.clone(),
                path: path.clone(),
                line,
            });
            approximations.push(Approximation {
                kind: "resolution_widening".to_string(),
                source: source_name.clone(),
                symbol: Some(symbol),
                reason: "multiple_candidate_targets".to_string(),
                candidate_targets,
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

    QueryDiagnostics {
        summary: DiagnosticSummary {
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

pub fn symbols_in(
    cg: &mut CallGraph,
    target: &str,
    target_kind: TargetKind,
    graph_mode: QueryGraphMode,
    render_options: &QueryRenderOptions<'_>,
) -> QueryResponse {
    let formatter = PathFormatter::new(render_options.analysis_root);
    match graph_mode {
        QueryGraphMode::Symbol => {
            let node_ids: Vec<NodeId> = defined_public_node_ids(cg)
                .into_iter()
                .filter(|id| match target_kind {
                    TargetKind::Path => cg.nodes_arena[*id]
                        .filename
                        .as_ref()
                        .is_some_and(|path| path_matches_target(path, target, &formatter)),
                    TargetKind::Module => {
                        module_matches_target(&cg.nodes_arena[*id].get_name(&cg.interner), target)
                    }
                })
                .collect();

            if node_ids.is_empty() {
                return QueryResponse(QueryResponseInner::SymbolsIn(QueryDocument::Error {
                    query: target_query(target, target_kind, graph_mode),
                    error: query_error(
                        "target_not_found",
                        format!("No symbols matched target '{target}'"),
                        Vec::new(),
                    ),
                }));
            }

            let symbols: Vec<SymbolRef> = node_ids
                .iter()
                .filter_map(|id| symbol_ref(&cg.nodes_arena[*id], &formatter, &cg.interner))
                .collect();
            let relevant_paths: FxHashSet<String> = symbols
                .iter()
                .filter_map(|symbol| {
                    symbol
                        .location
                        .as_ref()
                        .map(|location| location.path.clone())
                })
                .collect();
            let diagnostics = collect_query_diagnostics(
                cg,
                &node_ids.iter().copied().collect(),
                &relevant_paths,
                &formatter,
            );
            QueryResponse(QueryResponseInner::SymbolsIn(QueryDocument::Ok {
                query: target_query(target, target_kind, graph_mode),
                payload: symbols,
                diagnostics,
            }))
        }
        QueryGraphMode::Module => {
            let (nodes, _uses, defined) = cg.derive_module_graph();
            let mut relevant_ids = FxHashSet::default();
            let mut symbols = Vec::new();
            for id in defined {
                let node = &nodes[id];
                let matches = match target_kind {
                    TargetKind::Path => node
                        .filename
                        .as_ref()
                        .is_some_and(|path| path_matches_target(path, target, &formatter)),
                    TargetKind::Module => module_matches_target(&node.get_name(&cg.interner), target),
                };
                if matches {
                    relevant_ids.insert(id);
                    if let Some(symbol) = symbol_ref(node, &formatter, &cg.interner) {
                        symbols.push(symbol);
                    }
                }
            }
            symbols.sort_by(|a, b| a.canonical_name.cmp(&b.canonical_name));
            if symbols.is_empty() {
                return QueryResponse(QueryResponseInner::SymbolsIn(QueryDocument::Error {
                    query: target_query(target, target_kind, graph_mode),
                    error: query_error(
                        "target_not_found",
                        format!("No modules matched target '{target}'"),
                        Vec::new(),
                    ),
                }));
            }

            let relevant_paths: FxHashSet<String> = symbols
                .iter()
                .filter_map(|symbol| {
                    symbol
                        .location
                        .as_ref()
                        .map(|location| location.path.clone())
                })
                .collect();
            let diagnostics =
                collect_query_diagnostics(cg, &FxHashSet::default(), &relevant_paths, &formatter);
            QueryResponse(QueryResponseInner::SymbolsIn(QueryDocument::Ok {
                query: target_query(target, target_kind, graph_mode),
                payload: symbols,
                diagnostics,
            }))
        }
    }
}

pub fn summary(
    cg: &mut CallGraph,
    target: &str,
    target_kind: TargetKind,
    graph_mode: QueryGraphMode,
    render_options: &QueryRenderOptions<'_>,
    include_stats: bool,
) -> QueryResponse {
    match symbols_in(cg, target, target_kind, graph_mode, render_options) {
        QueryResponse(QueryResponseInner::SymbolsIn(QueryDocument::Ok {
            query,
            payload,
            diagnostics,
        })) => {
            let cg = &*cg; // reborrow as shared after symbols_in is done
            let mut symbol_counts = BTreeMap::new();
            let mut file_count = FxHashSet::default();
            for symbol in &payload {
                *symbol_counts.entry(symbol.kind.clone()).or_insert(0) += 1;
                if let Some(location) = &symbol.location {
                    file_count.insert(location.path.clone());
                }
            }

            let symbol_names: FxHashSet<String> = payload
                .iter()
                .map(|symbol| symbol.canonical_name.clone())
                .collect();

            let mut edge_counts = BTreeMap::new();
            if graph_mode == QueryGraphMode::Symbol {
                let incoming_uses = cg
                    .uses_edges
                    .iter()
                    .flat_map(|(source, targets)| {
                        let symbol_names = symbol_names.clone();
                        targets.iter().filter(move |target| {
                            symbol_names.contains(&cg.nodes_arena[**target].get_name(&cg.interner))
                                && cg.defined.contains(source)
                        })
                    })
                    .count();
                let outgoing_uses = cg
                    .uses_edges
                    .iter()
                    .filter(|(source, _)| {
                        symbol_names.contains(&cg.nodes_arena[**source].get_name(&cg.interner))
                    })
                    .map(|(_, targets)| {
                        targets
                            .iter()
                            .filter(|target| cg.defined.contains(target))
                            .count()
                    })
                    .sum::<usize>();
                edge_counts.insert("incoming_uses".to_string(), incoming_uses);
                edge_counts.insert("outgoing_uses".to_string(), outgoing_uses);
            }

            let symbol_stats = if include_stats && graph_mode == QueryGraphMode::Symbol {
                let mut caller_counts: FxHashMap<String, usize> = FxHashMap::default();
                let mut callee_counts: FxHashMap<String, usize> = FxHashMap::default();

                for (source, targets) in &cg.uses_edges {
                    let source_name = cg.nodes_arena[*source].get_name(&cg.interner);
                    let source_in_scope = symbol_names.contains(&source_name);
                    for target_id in targets {
                        if !cg.defined.contains(target_id) {
                            continue;
                        }
                        let target_name = cg.nodes_arena[*target_id].get_name(&cg.interner);
                        if symbol_names.contains(&target_name) && cg.defined.contains(source) {
                            *caller_counts.entry(target_name.clone()).or_insert(0) += 1;
                        }
                        if source_in_scope {
                            *callee_counts.entry(source_name.clone()).or_insert(0) += 1;
                        }
                    }
                }

                let mut stats: Vec<SymbolStat> = payload
                    .iter()
                    .map(|sym| SymbolStat {
                        canonical_name: sym.canonical_name.clone(),
                        kind: sym.kind.clone(),
                        caller_count: caller_counts.get(&sym.canonical_name).copied().unwrap_or(0),
                        callee_count: callee_counts.get(&sym.canonical_name).copied().unwrap_or(0),
                        location: sym.location.clone(),
                    })
                    .collect();
                stats.sort_by(|a, b| {
                    a.caller_count
                        .cmp(&b.caller_count)
                        .then_with(|| a.canonical_name.cmp(&b.canonical_name))
                });
                Some(stats)
            } else {
                None
            };

            let top_level_symbols: Vec<SymbolRef> = payload
                .iter()
                .filter(|symbol| symbol.namespace.as_ref().is_none_or(|ns| !ns.contains('.')))
                .cloned()
                .collect();

            QueryResponse(QueryResponseInner::Summary(QueryDocument::Ok {
                query,
                payload: SummaryPayload {
                    file_count: file_count.len(),
                    symbol_counts,
                    edge_counts,
                    top_level_symbols,
                    symbol_stats,
                },
                diagnostics,
            }))
        }
        QueryResponse(QueryResponseInner::SymbolsIn(QueryDocument::Error { query, error })) => {
            QueryResponse(QueryResponseInner::Summary(QueryDocument::Error {
                query,
                error,
            }))
        }
        _ => unreachable!(),
    }
}

pub fn callees(
    cg: &CallGraph,
    symbol: &str,
    match_mode: MatchMode,
    render_options: &QueryRenderOptions<'_>,
) -> QueryResponse {
    let formatter = PathFormatter::new(render_options.analysis_root);
    match resolve_single_symbol(cg, symbol, match_mode) {
        Ok(source_id) => {
            let node = symbol_ref(&cg.nodes_arena[source_id], &formatter, &cg.interner)
                .expect("resolved node should be public");
            let mut edges: Vec<OutgoingEdge> = cg
                .uses_edges
                .get(&source_id)
                .into_iter()
                .flat_map(|targets| targets.iter())
                .filter_map(|target_id| {
                    if !cg.defined.contains(target_id) {
                        return None;
                    }
                    symbol_ref(&cg.nodes_arena[*target_id], &formatter, &cg.interner).map(|target| OutgoingEdge {
                        kind: "uses".to_string(),
                        target,
                    })
                })
                .collect();
            edges.sort_by(|a, b| a.target.canonical_name.cmp(&b.target.canonical_name));
            let relevant_ids = FxHashSet::from_iter([source_id]);
            let relevant_paths = node
                .location
                .as_ref()
                .map(|location| FxHashSet::from_iter([location.path.clone()]))
                .unwrap_or_default();
            let diagnostics =
                collect_query_diagnostics(cg, &relevant_ids, &relevant_paths, &formatter);
            QueryResponse(QueryResponseInner::Callees(QueryDocument::Ok {
                query: symbol_query(symbol, match_mode),
                payload: (node, edges),
                diagnostics,
            }))
        }
        Err(error) => QueryResponse(QueryResponseInner::Callees(QueryDocument::Error {
            query: symbol_query(symbol, match_mode),
            error,
        })),
    }
}

pub fn callers(
    cg: &CallGraph,
    symbol: &str,
    match_mode: MatchMode,
    render_options: &QueryRenderOptions<'_>,
) -> QueryResponse {
    let formatter = PathFormatter::new(render_options.analysis_root);
    match resolve_single_symbol(cg, symbol, match_mode) {
        Ok(target_id) => {
            let node = symbol_ref(&cg.nodes_arena[target_id], &formatter, &cg.interner)
                .expect("resolved node should be public");
            let mut relevant_ids = FxHashSet::default();
            let mut edges: Vec<IncomingEdge> = cg
                .uses_edges
                .iter()
                .filter_map(|(source_id, targets)| {
                    if !cg.defined.contains(source_id) || !targets.contains(&target_id) {
                        return None;
                    }
                    relevant_ids.insert(*source_id);
                    symbol_ref(&cg.nodes_arena[*source_id], &formatter, &cg.interner).map(|source| IncomingEdge {
                        kind: "uses".to_string(),
                        source,
                    })
                })
                .collect();
            edges.sort_by(|a, b| a.source.canonical_name.cmp(&b.source.canonical_name));
            let relevant_paths: FxHashSet<String> = edges
                .iter()
                .filter_map(|edge| {
                    edge.source
                        .location
                        .as_ref()
                        .map(|location| location.path.clone())
                })
                .collect();
            let diagnostics =
                collect_query_diagnostics(cg, &relevant_ids, &relevant_paths, &formatter);
            QueryResponse(QueryResponseInner::Callers(QueryDocument::Ok {
                query: symbol_query(symbol, match_mode),
                payload: (node, edges),
                diagnostics,
            }))
        }
        Err(error) => QueryResponse(QueryResponseInner::Callers(QueryDocument::Error {
            query: symbol_query(symbol, match_mode),
            error,
        })),
    }
}

pub fn neighbors(
    cg: &CallGraph,
    symbol: &str,
    match_mode: MatchMode,
    render_options: &QueryRenderOptions<'_>,
) -> QueryResponse {
    let formatter = PathFormatter::new(render_options.analysis_root);
    match resolve_single_symbol(cg, symbol, match_mode) {
        Ok(node_id) => {
            let node = symbol_ref(&cg.nodes_arena[node_id], &formatter, &cg.interner)
                .expect("resolved node should be public");
            let mut relevant_ids = FxHashSet::from_iter([node_id]);
            let mut incoming = Vec::new();
            for (source_id, targets) in &cg.uses_edges {
                if !cg.defined.contains(source_id) || !targets.contains(&node_id) {
                    continue;
                }
                relevant_ids.insert(*source_id);
                if let Some(source) = symbol_ref(&cg.nodes_arena[*source_id], &formatter, &cg.interner) {
                    incoming.push(IncomingEdge {
                        kind: "uses".to_string(),
                        source,
                    });
                }
            }
            let mut outgoing = Vec::new();
            for target_id in cg
                .uses_edges
                .get(&node_id)
                .into_iter()
                .flat_map(|targets| targets.iter())
            {
                if !cg.defined.contains(target_id) {
                    continue;
                }
                if let Some(target) = symbol_ref(&cg.nodes_arena[*target_id], &formatter, &cg.interner) {
                    outgoing.push(OutgoingEdge {
                        kind: "uses".to_string(),
                        target,
                    });
                }
            }
            incoming.sort_by(|a, b| a.source.canonical_name.cmp(&b.source.canonical_name));
            outgoing.sort_by(|a, b| a.target.canonical_name.cmp(&b.target.canonical_name));
            let relevant_paths: FxHashSet<String> = incoming
                .iter()
                .filter_map(|edge| {
                    edge.source
                        .location
                        .as_ref()
                        .map(|location| location.path.clone())
                })
                .chain(outgoing.iter().filter_map(|edge| {
                    edge.target
                        .location
                        .as_ref()
                        .map(|location| location.path.clone())
                }))
                .collect();
            let diagnostics =
                collect_query_diagnostics(cg, &relevant_ids, &relevant_paths, &formatter);
            QueryResponse(QueryResponseInner::Neighbors(QueryDocument::Ok {
                query: symbol_query(symbol, match_mode),
                payload: (node, incoming, outgoing),
                diagnostics,
            }))
        }
        Err(error) => QueryResponse(QueryResponseInner::Neighbors(QueryDocument::Error {
            query: symbol_query(symbol, match_mode),
            error,
        })),
    }
}

pub fn path(
    cg: &CallGraph,
    source: &str,
    target: &str,
    match_mode: MatchMode,
    render_options: &QueryRenderOptions<'_>,
) -> QueryResponse {
    let formatter = PathFormatter::new(render_options.analysis_root);
    let source_id = match resolve_single_symbol(cg, source, match_mode) {
        Ok(id) => id,
        Err(error) => {
            return QueryResponse(QueryResponseInner::Path(QueryDocument::Error {
                query: path_query(source, target, match_mode),
                error,
            }));
        }
    };
    let target_id = match resolve_single_symbol(cg, target, match_mode) {
        Ok(id) => id,
        Err(error) => {
            return QueryResponse(QueryResponseInner::Path(QueryDocument::Error {
                query: path_query(source, target, match_mode),
                error,
            }));
        }
    };

    let mut queue = VecDeque::from([source_id]);
    let mut visited = FxHashSet::from_iter([source_id]);
    let mut prev: FxHashMap<NodeId, NodeId> = FxHashMap::default();

    while let Some(current) = queue.pop_front() {
        if current == target_id {
            break;
        }
        if let Some(targets) = cg.uses_edges.get(&current) {
            let mut sorted_targets: Vec<NodeId> = targets
                .iter()
                .copied()
                .filter(|id| cg.defined.contains(id) && public_kind(&cg.nodes_arena[*id]).is_some())
                .collect();
            sorted_targets.sort_by(|a, b| {
                cg.nodes_arena[*a]
                    .get_name(&cg.interner)
                    .cmp(&cg.nodes_arena[*b].get_name(&cg.interner))
            });
            for next in sorted_targets {
                if visited.insert(next) {
                    prev.insert(next, current);
                    queue.push_back(next);
                }
            }
        }
    }

    if !visited.contains(&target_id) {
        return QueryResponse(QueryResponseInner::Path(QueryDocument::Error {
            query: path_query(source, target, match_mode),
            error: query_error(
                "path_not_found",
                format!(
                    "No path connected '{}' to '{}'",
                    cg.nodes_arena[source_id].get_name(&cg.interner),
                    cg.nodes_arena[target_id].get_name(&cg.interner)
                ),
                Vec::new(),
            ),
        }));
    }

    let mut node_ids = vec![target_id];
    let mut current = target_id;
    while current != source_id {
        current = prev[&current];
        node_ids.push(current);
    }
    node_ids.reverse();

    let nodes: Vec<SymbolRef> = node_ids
        .iter()
        .filter_map(|id| symbol_ref(&cg.nodes_arena[*id], &formatter, &cg.interner))
        .collect();
    let edges: Vec<PathEdge> = node_ids
        .windows(2)
        .map(|pair| PathEdge {
            kind: "uses".to_string(),
            source: cg.nodes_arena[pair[0]].get_name(&cg.interner),
            target: cg.nodes_arena[pair[1]].get_name(&cg.interner),
        })
        .collect();
    let relevant_ids: FxHashSet<NodeId> = node_ids.iter().copied().collect();
    let relevant_paths: FxHashSet<String> = nodes
        .iter()
        .filter_map(|node| node.location.as_ref().map(|location| location.path.clone()))
        .collect();
    let diagnostics = collect_query_diagnostics(cg, &relevant_ids, &relevant_paths, &formatter);
    QueryResponse(QueryResponseInner::Path(QueryDocument::Ok {
        query: path_query(source, target, match_mode),
        payload: vec![PathResult { nodes, edges }],
        diagnostics,
    }))
}
