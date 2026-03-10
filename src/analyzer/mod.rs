//! Core call-graph analyzer.
//!
//! Walks Python ASTs (parsed via `ruff_python_parser`) and builds defines/uses
//! edge maps between [`Node`]s.  Follows a two-pass strategy:
//!
//! 1. **Pass 1** – collect all definitions and initial edges.
//! 2. **Between passes** – resolve base classes, compute MRO.
//! 3. **Pass 2** – re-analyze with full inheritance info.
//! 4. **Postprocess** – expand unknowns, contract non-existents, cull
//!    inherited edges, collapse inner scopes, resolve imports.

mod mro;
mod postprocess;
mod util;

use mro::resolve_mro;
pub use util::get_module_name;
use util::{collect_target_names_from_expr, get_ast_node_name, literal_key_from_expr};

use std::collections::{HashMap, HashSet};
use std::ops::{Deref, DerefMut};

use anyhow::{Context, Result};
use log::{debug, info};
use ruff_python_ast::*;
use ruff_python_parser::{self, Mode, ParseOptions};
use ruff_source_file::LineIndex;
use ruff_text_size::Ranged;

use crate::node::{Flavor, Node, NodeId};
use crate::scope::ValueSet;

// ---------------------------------------------------------------------------
// Scope info gathered from AST (replaces Python's `symtable`)
// ---------------------------------------------------------------------------

/// Lightweight scope info extracted from the AST in a pre-pass.
///
/// `defs` maps each locally-declared name to the set of NodeIds it may point
/// to.  An empty `ValueSet` means the name is declared but unresolved.
/// Bindings are unioned on rebind (no last-writer-wins).
#[derive(Debug, Clone)]
pub(super) struct ScopeInfo {
    defs: HashMap<String, ValueSet>,
    /// Shallow container facts for locally-bound names/attributes.
    ///
    /// This tracks statically-known list/tuple/dict literal contents so that
    /// later `x[i]` / `x["k"]` expressions can resolve through the retrieved
    /// value instead of collapsing back to the container object.
    containers: HashMap<String, ContainerFacts>,
    locals: HashSet<String>,
    /// Statically-known `__all__` exports for this module scope.
    ///
    /// `Some(names)` when the module contains a top-level `__all__ = [...]`
    /// whose elements are all string literals; `None` otherwise (either
    /// because `__all__` is absent or because it is not statically analyzable).
    ///
    /// When `Some`, `handle_star_import` uses this as the definitive filter
    /// for `from mod import *`, allowing private names that are explicitly
    /// listed and excluding public names that are not.
    all_exports: Option<HashSet<String>>,
}

/// A shallow abstract value: concrete NodeIds plus any statically-known
/// container literal structure attached to the binding.
#[derive(Debug, Clone, Default)]
struct ShallowValue {
    values: ValueSet,
    containers: ContainerFacts,
}

impl ShallowValue {
    fn union_with(&mut self, other: &ShallowValue) -> bool {
        let values_changed = self.values.union_with(&other.values);
        let containers_changed = self.containers.union_with(&other.containers);
        values_changed || containers_changed
    }

    fn first_value(&self) -> Option<NodeId> {
        self.values.first()
    }
}

/// A literal key we can statically interpret in a subscript expression.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum LiteralKey {
    Int(i64),
    String(String),
}

/// A single shallow container fact.
#[derive(Debug, Clone)]
enum ContainerFact {
    Sequence(Vec<ShallowValue>),
    Mapping(HashMap<LiteralKey, ShallowValue>),
}

/// A small set of shallow container facts for a single binding.
#[derive(Debug, Clone, Default)]
struct ContainerFacts(Vec<ContainerFact>);

impl ContainerFacts {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn push(&mut self, fact: ContainerFact) {
        self.0.push(fact);
    }

    fn union_with(&mut self, other: &ContainerFacts) -> bool {
        if other.0.is_empty() {
            return false;
        }
        let before = self.0.len();
        self.0.extend(other.0.iter().cloned());
        self.0.len() != before
    }

    fn resolve_subscript(&self, key: Option<&LiteralKey>) -> ShallowValue {
        let mut resolved = ShallowValue::default();
        for fact in &self.0 {
            match fact {
                ContainerFact::Sequence(items) => {
                    if let Some(LiteralKey::Int(index)) = key {
                        let idx = if *index >= 0 {
                            usize::try_from(*index).ok()
                        } else {
                            let len = items.len() as i64;
                            usize::try_from(len + index).ok()
                        };
                        if let Some(item) = idx.and_then(|idx| items.get(idx)) {
                            resolved.union_with(item);
                        }
                    } else {
                        for item in items {
                            resolved.union_with(item);
                        }
                    }
                }
                ContainerFact::Mapping(items) => {
                    if let Some(key) = key {
                        if let Some(value) = items.get(key) {
                            resolved.union_with(value);
                        }
                    } else {
                        for value in items.values() {
                            resolved.union_with(value);
                        }
                    }
                }
            }
        }
        resolved
    }
}

impl ScopeInfo {
    fn new(_name: &str) -> Self {
        Self {
            defs: HashMap::new(),
            containers: HashMap::new(),
            locals: HashSet::new(),
            all_exports: None,
        }
    }

    fn from_names(_name: &str, identifiers: &HashSet<String>) -> Self {
        let defs = identifiers
            .iter()
            .map(|id| (id.clone(), ValueSet::empty()))
            .collect();
        let locals = identifiers.clone();
        Self {
            defs,
            containers: HashMap::new(),
            locals,
            all_exports: None,
        }
    }
}

/// Extract the exported names from a `__all__ = [...]` or `__all__ = (...)`
/// literal expression.
///
/// Returns `Some(names)` only when every element is a plain string literal.
/// If any element is not a string literal (e.g. a variable reference or a
/// computed expression) we conservatively return `None` so that callers fall
/// back to the default privacy filter.
fn extract_all_exports(expr: &Expr) -> Option<HashSet<String>> {
    let elts = match expr {
        Expr::List(l) => &l.elts,
        Expr::Tuple(t) => &t.elts,
        _ => return None,
    };
    let mut names = HashSet::new();
    for elt in elts {
        if let Expr::StringLiteral(s) = elt {
            names.insert(s.value.to_str().to_string());
        } else {
            // Non-literal element — not statically analyzable.
            return None;
        }
    }
    Some(names)
}

// ---------------------------------------------------------------------------
// Public call-graph struct
// ---------------------------------------------------------------------------

/// The finished output of the analyzer: a call graph over Python symbols.
#[derive(Debug)]
pub struct CallGraph {
    // Node arena --------------------------------------------------------
    pub nodes_arena: Vec<Node>,
    /// Short name -> list of node IDs (there may be several in different
    /// namespaces).
    pub nodes_by_name: HashMap<String, Vec<NodeId>>,

    // Edges -------------------------------------------------------------
    pub defines_edges: HashMap<NodeId, HashSet<NodeId>>,
    pub uses_edges: HashMap<NodeId, HashSet<NodeId>>,

    /// Which nodes have been marked *defined* (have a defines edge from
    /// them, or were created as wildcard nodes).
    pub defined: HashSet<NodeId>,

    // File mapping ------------------------------------------------------
    pub(super) module_to_filename: HashMap<String, String>,
}

/// Internal mutable analysis session.
///
/// This owns the work-in-progress state needed to build a [`CallGraph`], but
/// that transient state does not leak into the public result type.
#[derive(Debug)]
pub(super) struct AnalysisSession {
    pub(super) graph: CallGraph,

    // Scope tracking (persistent across files/passes) -------------------
    pub(super) scopes: HashMap<String, ScopeInfo>,

    // Class information -------------------------------------------------
    /// Pass 1: class NodeId -> list of base-class AST info (stored as
    /// (namespace, name) pairs extracted from AST nodes).
    pub(super) class_base_ast_info: HashMap<NodeId, Vec<BaseClassRef>>,
    /// Pass 2: class NodeId -> resolved base NodeIds.
    pub(super) class_base_nodes: HashMap<NodeId, Vec<NodeId>>,
    /// MRO for each class.
    pub(super) mro: HashMap<NodeId, Vec<NodeId>>,

    /// Collected return values per function node, used for return-value propagation.
    /// Maps function/method NodeId -> set of NodeIds that the function may return.
    /// Populated during `visit_stmt(Return)` and consumed in `visit_call`.
    pub(super) function_returns: HashMap<NodeId, HashSet<NodeId>>,

    pub(super) filenames: Vec<String>,
    pub(super) root: Option<String>,

    // Transient state (reset per file) ----------------------------------
    pub(super) module_name: String,
    pub(super) filename: String,
    pub(super) name_stack: Vec<String>,
    pub(super) scope_stack: Vec<String>, // keys into self.scopes
    pub(super) class_stack: Vec<NodeId>,
    pub(super) context_stack: Vec<String>,
}

/// Describes how a base class was referenced in the source.
#[derive(Debug, Clone)]
pub(super) enum BaseClassRef {
    Name(String),
    Attribute(Vec<String>),
}

struct CachedFile {
    filename: String,
    module_name: String,
    module: ModModule,
    line_index: LineIndex,
    scopes: HashMap<String, ScopeInfo>,
}

impl Deref for AnalysisSession {
    type Target = CallGraph;

    fn deref(&self) -> &Self::Target {
        &self.graph
    }
}

impl DerefMut for AnalysisSession {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.graph
    }
}

// =========================================================================
// Construction and high-level processing
// =========================================================================

impl CallGraph {
    /// Analyze a set of Python files and return the resulting call graph.
    pub fn new(filenames: &[String], root: Option<&str>) -> Result<Self> {
        let mut session = AnalysisSession::new(filenames, root);
        session.process()?;
        Ok(session.into_call_graph())
    }
}

impl AnalysisSession {
    fn new(filenames: &[String], root: Option<&str>) -> Self {
        let mut module_to_filename = HashMap::new();
        for filename in filenames {
            let mod_name = get_module_name(filename, root);
            module_to_filename.insert(mod_name, filename.clone());
        }

        Self {
            graph: CallGraph {
                nodes_arena: Vec::new(),
                nodes_by_name: HashMap::new(),
                defines_edges: HashMap::new(),
                uses_edges: HashMap::new(),
                defined: HashSet::new(),
                module_to_filename,
            },
            scopes: HashMap::new(),
            function_returns: HashMap::new(),
            class_base_ast_info: HashMap::new(),
            class_base_nodes: HashMap::new(),
            mro: HashMap::new(),
            filenames: filenames.to_vec(),
            root: root.map(|s| s.to_string()),
            module_name: String::new(),
            filename: String::new(),
            name_stack: Vec::new(),
            scope_stack: Vec::new(),
            class_stack: Vec::new(),
            context_stack: Vec::new(),
        }
    }

    fn into_call_graph(self) -> CallGraph {
        self.graph
    }

    /// Two-pass analysis followed by a fixpoint loop for return-value propagation.
    fn process(&mut self) -> Result<()> {
        let cached_files = self.prepare_files()?;
        for cached_file in &cached_files {
            self.merge_scopes(&cached_file.scopes);
        }

        for pass_num in 0..2 {
            for cached_file in &cached_files {
                debug!(
                    "========== pass {}, file '{}' ==========",
                    pass_num + 1,
                    cached_file.filename
                );
                self.process_one(cached_file);
            }
            if pass_num == 0 {
                self.resolve_base_classes();
            }
        }

        // Fixpoint: keep re-analyzing until function_returns stabilises.
        // Each extra pass may propagate return types discovered in the previous
        // pass through call sites, enabling downstream attribute resolution.
        const MAX_PROPAGATION_PASSES: usize = 8;
        for pass_num in 0..MAX_PROPAGATION_PASSES {
            let prev_returns = self.function_returns.clone();
            for cached_file in &cached_files {
                debug!(
                    "========== propagation pass {}, file '{}' ==========",
                    pass_num + 1,
                    cached_file.filename
                );
                self.process_one(cached_file);
            }
            if self.function_returns == prev_returns {
                debug!(
                    "Return propagation converged after {} extra passes",
                    pass_num + 1
                );
                break;
            }
        }

        self.postprocess();
        Ok(())
    }

    fn prepare_files(&self) -> Result<Vec<CachedFile>> {
        let mut cached_files = Vec::with_capacity(self.filenames.len());
        for filename in &self.filenames {
            let content =
                std::fs::read_to_string(filename).with_context(|| format!("reading {filename}"))?;
            let module_name = get_module_name(filename, self.root.as_deref());
            let parsed =
                ruff_python_parser::parse_unchecked(&content, ParseOptions::from(Mode::Module));
            let module = match parsed.into_syntax() {
                Mod::Module(module) => module,
                _ => continue,
            };
            let line_index = LineIndex::from_source_text(&content);
            let scopes = Self::build_scopes(&module, &module_name);
            cached_files.push(CachedFile {
                filename: filename.clone(),
                module_name,
                module,
                line_index,
                scopes,
            });
        }
        Ok(cached_files)
    }

    /// Analyze a single Python source file.
    fn process_one(&mut self, cached_file: &CachedFile) {
        self.filename = cached_file.filename.clone();
        self.module_name = cached_file.module_name.clone();

        self.visit_module(&cached_file.module, &cached_file.line_index);

        self.module_name.clear();
        self.filename.clear();
    }

    // =====================================================================
    // Scope analysis (pre-pass) — replaces Python's `symtable`
    // =====================================================================

    /// Gather scope information by walking the cached AST.
    fn build_scopes(module: &ModModule, module_ns: &str) -> HashMap<String, ScopeInfo> {
        let mut scopes: HashMap<String, ScopeInfo> = HashMap::new();

        // Module-level scope
        let mut module_scope = ScopeInfo::new("");
        Self::collect_scope_defs(&module.body, &mut module_scope);
        scopes.insert(module_ns.to_string(), module_scope);

        // Nested scopes
        Self::collect_nested_scopes(&module.body, module_ns, &mut scopes);

        scopes
    }

    fn merge_scopes(&mut self, scopes: &HashMap<String, ScopeInfo>) {
        // Merge into existing scopes (union values rather than overwrite)
        for (ns, sc) in scopes {
            if let Some(existing) = self.scopes.get_mut(ns.as_str()) {
                for (name, vs) in &sc.defs {
                    existing
                        .defs
                        .entry(name.clone())
                        .or_default()
                        .union_with(vs);
                }
                for (name, facts) in &sc.containers {
                    existing
                        .containers
                        .entry(name.clone())
                        .or_default()
                        .union_with(facts);
                }
                // Propagate __all__ if newly discovered.
                if existing.all_exports.is_none() && sc.all_exports.is_some() {
                    existing.all_exports = sc.all_exports.clone();
                }
            } else {
                self.scopes.insert(ns.clone(), sc.clone());
            }
        }
    }

    /// Collect names defined (bound) at this scope level.
    fn collect_scope_defs(stmts: &[Stmt], scope: &mut ScopeInfo) {
        for stmt in stmts {
            match stmt {
                Stmt::FunctionDef(f) => {
                    let name = f.name.id.to_string();
                    scope.defs.entry(name.clone()).or_default();
                    scope.locals.insert(name);
                }
                Stmt::ClassDef(c) => {
                    let name = c.name.id.to_string();
                    scope.defs.entry(name.clone()).or_default();
                    scope.locals.insert(name);
                }
                Stmt::Import(imp) => {
                    for alias in &imp.names {
                        let name = if let Some(ref asname) = alias.asname {
                            asname.id.to_string()
                        } else {
                            alias.name.id.to_string()
                        };
                        scope.defs.entry(name).or_default();
                    }
                }
                Stmt::ImportFrom(imp) => {
                    for alias in &imp.names {
                        // Skip star imports — names are injected during the visit
                        // phase once the source module's scope is available.
                        if alias.name.id.as_str() == "*" {
                            continue;
                        }
                        let name = if let Some(ref asname) = alias.asname {
                            asname.id.to_string()
                        } else {
                            alias.name.id.to_string()
                        };
                        scope.defs.entry(name).or_default();
                    }
                }
                Stmt::Assign(a) => {
                    for target in &a.targets {
                        // Detect `__all__ = [...]` or `__all__ = (...)` and
                        // record the statically-known export list.
                        if let Expr::Name(n) = target {
                            if n.id.as_str() == "__all__" {
                                if let Some(exports) = extract_all_exports(&a.value) {
                                    scope.all_exports = Some(exports);
                                }
                            }
                        }
                        Self::collect_assign_target_names(target, scope);
                    }
                }
                Stmt::AugAssign(a) => {
                    Self::collect_assign_target_names(&a.target, scope);
                }
                Stmt::AnnAssign(a) => {
                    Self::collect_assign_target_names(&a.target, scope);
                }
                Stmt::For(f) => {
                    Self::collect_assign_target_names(&f.target, scope);
                    // Do NOT recurse into body for scope defs at *this* level;
                    // we only capture the target bindings.
                }
                Stmt::Global(g) => {
                    for name in &g.names {
                        scope.defs.entry(name.id.to_string()).or_default();
                    }
                }
                Stmt::Nonlocal(n) => {
                    for name in &n.names {
                        scope.defs.entry(name.id.to_string()).or_default();
                    }
                }
                // If/While/With/Try — recurse into their bodies at the same
                // scope level (Python does not create new scopes for these).
                Stmt::If(s) => {
                    Self::collect_scope_defs(&s.body, scope);
                    for clause in &s.elif_else_clauses {
                        Self::collect_scope_defs(&clause.body, scope);
                    }
                }
                Stmt::While(s) => {
                    Self::collect_scope_defs(&s.body, scope);
                    Self::collect_scope_defs(&s.orelse, scope);
                }
                Stmt::With(s) => {
                    for item in &s.items {
                        if let Some(ref vars) = item.optional_vars {
                            Self::collect_assign_target_names(vars, scope);
                        }
                    }
                    Self::collect_scope_defs(&s.body, scope);
                }
                Stmt::Try(s) => {
                    Self::collect_scope_defs(&s.body, scope);
                    for handler in &s.handlers {
                        let ExceptHandler::ExceptHandler(h) = handler;
                        if let Some(ref name) = h.name {
                            scope.defs.entry(name.id.to_string()).or_default();
                            scope.locals.insert(name.id.to_string());
                        }
                        Self::collect_scope_defs(&h.body, scope);
                    }
                    Self::collect_scope_defs(&s.orelse, scope);
                    Self::collect_scope_defs(&s.finalbody, scope);
                }
                _ => {}
            }
        }
    }

    /// Recurse into function/class bodies to create child scopes.
    fn collect_nested_scopes(
        stmts: &[Stmt],
        parent_ns: &str,
        scopes: &mut HashMap<String, ScopeInfo>,
    ) {
        for stmt in stmts {
            match stmt {
                Stmt::FunctionDef(f) => {
                    let name = f.name.id.to_string();
                    let ns = format!("{parent_ns}.{name}");
                    let mut scope = ScopeInfo::new(&name);

                    // Add parameter names (declared with empty ValueSet)
                    for param in &f.parameters.args {
                        let pname = param.parameter.name.id.to_string();
                        scope.defs.entry(pname.clone()).or_default();
                        scope.locals.insert(pname);
                    }
                    for param in &f.parameters.posonlyargs {
                        let pname = param.parameter.name.id.to_string();
                        scope.defs.entry(pname.clone()).or_default();
                        scope.locals.insert(pname);
                    }
                    for param in &f.parameters.kwonlyargs {
                        let pname = param.parameter.name.id.to_string();
                        scope.defs.entry(pname.clone()).or_default();
                        scope.locals.insert(pname);
                    }
                    if let Some(ref va) = f.parameters.vararg {
                        let pname = va.name.id.to_string();
                        scope.defs.entry(pname.clone()).or_default();
                        scope.locals.insert(pname);
                    }
                    if let Some(ref kw) = f.parameters.kwarg {
                        let pname = kw.name.id.to_string();
                        scope.defs.entry(pname.clone()).or_default();
                        scope.locals.insert(pname);
                    }

                    Self::collect_scope_defs(&f.body, &mut scope);
                    scopes.insert(ns.clone(), scope);
                    Self::collect_nested_scopes(&f.body, &ns, scopes);
                }
                Stmt::ClassDef(c) => {
                    let name = c.name.id.to_string();
                    let ns = format!("{parent_ns}.{name}");
                    let mut scope = ScopeInfo::new(&name);
                    Self::collect_scope_defs(&c.body, &mut scope);
                    scopes.insert(ns.clone(), scope);
                    Self::collect_nested_scopes(&c.body, &ns, scopes);
                }
                // Recurse into compound statements that don't create new
                // scopes (if/while/with/try/for).
                Stmt::If(s) => {
                    Self::collect_nested_scopes(&s.body, parent_ns, scopes);
                    for clause in &s.elif_else_clauses {
                        Self::collect_nested_scopes(&clause.body, parent_ns, scopes);
                    }
                }
                Stmt::While(s) => {
                    Self::collect_nested_scopes(&s.body, parent_ns, scopes);
                    Self::collect_nested_scopes(&s.orelse, parent_ns, scopes);
                }
                Stmt::For(s) => {
                    Self::collect_nested_scopes(&s.body, parent_ns, scopes);
                    Self::collect_nested_scopes(&s.orelse, parent_ns, scopes);
                }
                Stmt::With(s) => {
                    Self::collect_nested_scopes(&s.body, parent_ns, scopes);
                }
                Stmt::Try(s) => {
                    Self::collect_nested_scopes(&s.body, parent_ns, scopes);
                    for handler in &s.handlers {
                        let ExceptHandler::ExceptHandler(h) = handler;
                        Self::collect_nested_scopes(&h.body, parent_ns, scopes);
                    }
                    Self::collect_nested_scopes(&s.orelse, parent_ns, scopes);
                    Self::collect_nested_scopes(&s.finalbody, parent_ns, scopes);
                }
                _ => {}
            }
        }
    }

    /// Extract names from an assignment target expression.
    fn collect_assign_target_names(target: &Expr, scope: &mut ScopeInfo) {
        match target {
            Expr::Name(n) => {
                let name = n.id.to_string();
                scope.defs.entry(name.clone()).or_default();
                scope.locals.insert(name);
            }
            Expr::Tuple(t) => {
                for elt in &t.elts {
                    Self::collect_assign_target_names(elt, scope);
                }
            }
            Expr::List(l) => {
                for elt in &l.elts {
                    Self::collect_assign_target_names(elt, scope);
                }
            }
            Expr::Starred(s) => {
                Self::collect_assign_target_names(&s.value, scope);
            }
            _ => {} // Attribute, Subscript — not local bindings
        }
    }

    // =====================================================================
    // Node creation and lookup
    // =====================================================================

    /// Get or create the unique node for (namespace, name).
    pub(super) fn get_node(
        &mut self,
        namespace: Option<&str>,
        name: &str,
        flavor: Flavor,
    ) -> NodeId {
        // Check for existing node with matching (namespace, name).
        if let Some(ids) = self.nodes_by_name.get(name) {
            for &id in ids {
                let n = &self.nodes_arena[id];
                if n.namespace.as_deref() == namespace {
                    // Update flavor if strictly more specific
                    if flavor.specificity() > n.flavor.specificity() {
                        self.nodes_arena[id].flavor = flavor;
                    }
                    return id;
                }
            }
        }

        // Determine filename
        let filename = if let Some(ns) = namespace {
            if let Some(f) = self.module_to_filename.get(ns) {
                Some(f.clone())
            } else {
                Some(self.filename.clone())
            }
        } else {
            Some(self.filename.clone())
        };

        let mut node = Node::new(namespace, name, flavor);
        node.filename = filename;
        let id = self.nodes_arena.len();
        // Wildcard nodes (namespace=None) start as defined
        if namespace.is_none() {
            self.defined.insert(id);
        }

        self.nodes_arena.push(node);
        self.nodes_by_name
            .entry(name.to_string())
            .or_default()
            .push(id);
        id
    }

    /// Get the node representing the current namespace.
    fn get_node_of_current_namespace(&mut self) -> NodeId {
        assert!(!self.name_stack.is_empty());
        let namespace = if self.name_stack.len() > 1 {
            self.name_stack[..self.name_stack.len() - 1].join(".")
        } else {
            String::new()
        };
        let name = self
            .name_stack
            .last()
            .expect("name_stack must not be empty during AST walk")
            .clone();
        self.get_node(Some(&namespace), &name, Flavor::Namespace)
    }

    /// Get the parent node of the given node (by splitting its namespace).
    pub(super) fn get_parent_node(&mut self, node_id: NodeId) -> NodeId {
        let node = &self.nodes_arena[node_id];
        let (ns, name) = if let Some(ref namespace) = node.namespace {
            if namespace.contains('.') {
                let (parent_ns, parent_name) = namespace
                    .rsplit_once('.')
                    .expect("namespace contains '.' (checked above)");
                (parent_ns.to_string(), parent_name.to_string())
            } else {
                (String::new(), namespace.clone())
            }
        } else {
            (String::new(), String::new())
        };
        self.get_node(Some(&ns), &name, Flavor::Namespace)
    }

    /// Associate a node with a filename and line number.
    fn associate_node(&mut self, node_id: NodeId, filename: &str, line: usize) {
        self.nodes_arena[node_id].filename = Some(filename.to_string());
        self.nodes_arena[node_id].line = Some(line);
    }

    // =====================================================================
    // Edge management
    // =====================================================================

    pub(super) fn add_defines_edge(&mut self, from_id: NodeId, to_id: Option<NodeId>) -> bool {
        self.defined.insert(from_id);
        if let Some(to) = to_id {
            self.defined.insert(to);
            self.defines_edges.entry(from_id).or_default().insert(to)
        } else {
            false
        }
    }

    pub(super) fn add_uses_edge(&mut self, from_id: NodeId, to_id: NodeId) -> bool {
        let entry = self.uses_edges.entry(from_id).or_default();
        if entry.insert(to_id) {
            // Remove matching wildcard
            let to_ns = self.nodes_arena[to_id].namespace.clone();
            let to_name = self.nodes_arena[to_id].name.clone();
            if to_ns.is_some() {
                self.remove_wild(from_id, to_id, &to_name);
            }
            true
        } else {
            false
        }
    }

    pub(super) fn remove_uses_edge(&mut self, from_id: NodeId, to_id: NodeId) {
        if let Some(edges) = self.uses_edges.get_mut(&from_id) {
            edges.remove(&to_id);
        }
    }

    /// Remove uses edge from `from_id` to wildcard `*.name`.
    fn remove_wild(&mut self, from_id: NodeId, to_id: NodeId, name: &str) {
        if name.is_empty() {
            return;
        }
        let Some(edges) = self.uses_edges.get(&from_id) else {
            return;
        };

        // Don't remove if target is an argument sentinel
        let to_name = &self.nodes_arena[to_id].get_name();
        if to_name.contains("^^^argument^^^") {
            return;
        }

        // Don't remove self-references
        if to_id == from_id {
            return;
        }

        let wild = edges
            .iter()
            .find(|&&eid| {
                let n = &self.nodes_arena[eid];
                n.namespace.is_none() && n.name == name
            })
            .copied();

        if let Some(wild_id) = wild {
            info!(
                "Use from {} to {} resolves {}; removing wildcard",
                self.nodes_arena[from_id].get_name(),
                self.nodes_arena[to_id].get_name(),
                self.nodes_arena[wild_id].get_name()
            );
            self.remove_uses_edge(from_id, wild_id);
        }
    }

    // =====================================================================
    // Value getter/setter (scope-based name resolution)
    // =====================================================================

    /// Get the first (any) value of `name` in the current scope stack.
    ///
    /// This is a backward-compat shim over `get_values`.  Prefer `get_values`
    /// when iterating over all possible pointees.
    fn get_value(&self, name: &str) -> Option<NodeId> {
        self.get_values(name).first()
    }

    /// Get all possible values of `name` in the current scope stack.
    ///
    /// Walks from innermost to outermost scope and returns the `ValueSet` from
    /// the first scope that declares the name.
    fn get_values(&self, name: &str) -> ValueSet {
        for scope_key in self.scope_stack.iter().rev() {
            if let Some(scope) = self.scopes.get(scope_key)
                && let Some(vs) = scope.defs.get(name)
            {
                return vs.clone();
            }
        }
        ValueSet::empty()
    }

    /// Get all shallow container facts of `name` in the current scope stack.
    fn get_containers(&self, name: &str) -> ContainerFacts {
        for scope_key in self.scope_stack.iter().rev() {
            if let Some(scope) = self.scopes.get(scope_key)
                && let Some(facts) = scope.containers.get(name)
            {
                return facts.clone();
            }
        }
        ContainerFacts::default()
    }

    /// Add `value` to the binding set of `name` in the innermost scope that
    /// declares it.  If `value` is `None`, just ensure the name is declared
    /// (creates an empty entry if needed).
    ///
    /// Unlike the old single-value overwrite, this **unions** rather than
    /// replaces, so all plausible pointees from different branches are kept.
    fn set_value(&mut self, name: &str, value: Option<NodeId>) {
        for scope_key in self.scope_stack.iter().rev() {
            if let Some(scope) = self.scopes.get(scope_key)
                && scope.defs.contains_key(name)
            {
                let scope = self
                    .scopes
                    .get_mut(scope_key)
                    .expect("scope confirmed to exist above");
                if let Some(id) = value {
                    scope.defs.entry(name.to_string()).or_default().insert(id);
                }
                // If value is None: name already declared — nothing to add.
                return;
            }
        }
        // Not declared in any enclosing scope — add to current scope.
        if let Some(scope_key) = self.scope_stack.last() {
            let scope_key = scope_key.clone();
            if let Some(scope) = self.scopes.get_mut(&scope_key) {
                if let Some(id) = value {
                    scope.defs.entry(name.to_string()).or_default().insert(id);
                } else {
                    scope.defs.entry(name.to_string()).or_default();
                }
            }
        }
    }

    /// Add shallow container facts to the binding of `name` in the innermost
    /// scope that declares it.
    fn set_containers(&mut self, name: &str, containers: &ContainerFacts) {
        if containers.is_empty() {
            return;
        }

        for scope_key in self.scope_stack.iter().rev() {
            if let Some(scope) = self.scopes.get(scope_key)
                && scope.defs.contains_key(name)
            {
                let scope = self
                    .scopes
                    .get_mut(scope_key)
                    .expect("scope confirmed to exist above");
                scope
                    .containers
                    .entry(name.to_string())
                    .or_default()
                    .union_with(containers);
                return;
            }
        }

        if let Some(scope_key) = self.scope_stack.last() {
            let scope_key = scope_key.clone();
            if let Some(scope) = self.scopes.get_mut(&scope_key) {
                scope
                    .containers
                    .entry(name.to_string())
                    .or_default()
                    .union_with(containers);
            }
        }
    }

    /// Check if a name is a local in the current (innermost) scope.
    fn is_local(&self, name: &str) -> bool {
        if let Some(scope_key) = self.scope_stack.last()
            && let Some(scope) = self.scopes.get(scope_key)
        {
            return scope.locals.contains(name);
        }
        false
    }

    /// Get the current class node, if inside a class definition.
    fn get_current_class(&self) -> Option<NodeId> {
        self.class_stack.last().copied()
    }

    // =====================================================================
    // Attribute access helpers
    // =====================================================================

    /// Resolve an attribute chain: `obj.attr` -> (obj_node_id, attr_name).
    ///
    /// Returns the *first* possible object node for backward compatibility.
    /// Call sites that need all possible objects should use
    /// `get_obj_ids_for_expr` instead.
    fn resolve_attribute(&mut self, expr: &ExprAttribute) -> (Option<NodeId>, String) {
        let attr_name = expr.attr.id.to_string();

        match expr.value.as_ref() {
            Expr::Attribute(inner_attr) => {
                let (obj_node, inner_attr_name) = self.resolve_attribute(inner_attr);

                if let Some(obj_id) = obj_node
                    && self.nodes_arena[obj_id].namespace.is_some()
                {
                    let ns = self.nodes_arena[obj_id].get_name();
                    if let Some(val) = self.lookup_in_scope(&ns, &inner_attr_name) {
                        return (Some(val), attr_name);
                    }
                }
                (None, attr_name)
            }
            Expr::Call(call) => {
                // Try to resolve builtins like super()
                if let Some(result_id) = self.resolve_builtins(call) {
                    (Some(result_id), attr_name)
                } else {
                    (None, attr_name)
                }
            }
            _ => {
                let obj_name = get_ast_node_name(&expr.value);
                if let Some(obj_id) = self.get_value(&obj_name) {
                    (Some(obj_id), attr_name)
                } else {
                    (None, attr_name)
                }
            }
        }
    }

    /// Collect all possible NodeIds that an expression can resolve to.
    ///
    /// For `Name` exprs: returns all values in the binding set.
    /// For `Attribute` exprs: resolves object (multi-value) then looks up attr.
    /// For `Call` exprs: tries builtin resolution.
    /// Returns an empty vec for unresolvable expressions.
    fn get_obj_ids_for_expr(&mut self, expr: &Expr) -> Vec<NodeId> {
        match expr {
            Expr::Name(n) if n.ctx == ExprContext::Load => {
                self.get_values(&n.id.to_string()).iter().collect()
            }
            Expr::Attribute(a) => {
                // Get all possible values for this nested attribute
                let attr_name = a.attr.id.to_string();
                let obj_ids = self.get_obj_ids_for_expr(&a.value);
                let mut results = Vec::new();
                for obj_id in obj_ids {
                    if self.nodes_arena[obj_id].namespace.is_none() {
                        continue;
                    }
                    let ns = self.nodes_arena[obj_id].get_name();
                    let vs = self.lookup_values_in_scope(&ns, &attr_name);
                    if vs.is_empty() {
                        // Try MRO
                        if let Some(mro) = self.mro.get(&obj_id).cloned() {
                            for &base_id in mro.iter().skip(1) {
                                let base_ns = self.nodes_arena[base_id].get_name();
                                let bvs = self.lookup_values_in_scope(&base_ns, &attr_name);
                                for id in bvs.iter() {
                                    if !results.contains(&id) {
                                        results.push(id);
                                    }
                                }
                                if !bvs.is_empty() {
                                    break;
                                }
                            }
                        }
                    } else {
                        for id in vs.iter() {
                            if !results.contains(&id) {
                                results.push(id);
                            }
                        }
                    }
                }
                results
            }
            Expr::Call(c) => {
                if let Some(id) = self.resolve_builtins(c) {
                    return vec![id];
                }
                // For non-builtin calls, resolve the callee and look up return types
                // so that chained attribute access like `make().method()` can proceed.
                let func_ids = self.get_obj_ids_for_expr(&c.func);
                let mut results = Vec::new();
                for &func_id in &func_ids {
                    // Class instantiation: the class node is the "instance type".
                    if self.class_base_ast_info.contains_key(&func_id) {
                        if !results.contains(&func_id) {
                            results.push(func_id);
                        }
                    }
                    // Function call: propagate all statically-known return values.
                    if let Some(ret_ids) = self.function_returns.get(&func_id).cloned() {
                        for ret_id in ret_ids {
                            if !results.contains(&ret_id) {
                                results.push(ret_id);
                            }
                        }
                    }
                }
                results
            }
            Expr::Subscript(s) => {
                let resolved = self.resolve_subscript_value(s);
                if !resolved.values.is_empty() {
                    resolved.values.iter().collect()
                } else {
                    self.get_obj_ids_for_expr(&s.value)
                }
            }
            _ => vec![],
        }
    }

    /// Look up the first value of a name in a specific (named) scope.
    ///
    /// Returns `None` if the scope doesn't exist or the name is unbound.
    /// Use `lookup_values_in_scope` to get all possible values.
    fn lookup_in_scope(&self, ns: &str, name: &str) -> Option<NodeId> {
        self.lookup_values_in_scope(ns, name).first()
    }

    /// Look up all possible values of a name in a specific (named) scope.
    pub(super) fn lookup_values_in_scope(&self, ns: &str, name: &str) -> ValueSet {
        if let Some(scope) = self.scopes.get(ns) {
            if let Some(vs) = scope.defs.get(name) {
                return vs.clone();
            }
        }
        ValueSet::empty()
    }

    /// Look up shallow container facts of a name in a specific (named) scope.
    fn lookup_containers_in_scope(&self, ns: &str, name: &str) -> ContainerFacts {
        if let Some(scope) = self.scopes.get(ns) {
            if let Some(facts) = scope.containers.get(name) {
                return facts.clone();
            }
        }
        ContainerFacts::default()
    }

    /// Add an attribute value to the object's scope (additive — does not
    /// overwrite existing bindings for the same attribute name).
    fn set_attribute(&mut self, expr: &ExprAttribute, value: Option<NodeId>) -> bool {
        let (obj_node, attr_name) = self.resolve_attribute(expr);

        if let Some(obj_id) = obj_node
            && self.nodes_arena[obj_id].namespace.is_some()
        {
            let ns = self.nodes_arena[obj_id].get_name();
            if let Some(scope) = self.scopes.get_mut(&ns) {
                if let Some(id) = value {
                    scope.defs.entry(attr_name).or_default().insert(id);
                } else {
                    scope.defs.entry(attr_name).or_default();
                }
                return true;
            }
        }
        false
    }

    fn set_attribute_shallow_value(&mut self, expr: &ExprAttribute, value: &ShallowValue) -> bool {
        let (obj_node, attr_name) = self.resolve_attribute(expr);

        if let Some(obj_id) = obj_node
            && self.nodes_arena[obj_id].namespace.is_some()
        {
            let ns = self.nodes_arena[obj_id].get_name();
            if let Some(scope) = self.scopes.get_mut(&ns) {
                if !value.values.is_empty() {
                    scope
                        .defs
                        .entry(attr_name.clone())
                        .or_default()
                        .union_with(&value.values);
                } else {
                    scope.defs.entry(attr_name.clone()).or_default();
                }
                if !value.containers.is_empty() {
                    scope
                        .containers
                        .entry(attr_name)
                        .or_default()
                        .union_with(&value.containers);
                }
                return true;
            }
        }
        false
    }

    fn resolve_shallow_value(&mut self, expr: &Expr) -> ShallowValue {
        match expr {
            Expr::Name(node) if node.ctx == ExprContext::Load => ShallowValue {
                values: self.get_values(node.id.as_ref()),
                containers: self.get_containers(node.id.as_ref()),
            },
            Expr::Attribute(node) if node.ctx == ExprContext::Load => {
                let mut resolved = ShallowValue::default();
                let obj_ids = self.get_obj_ids_for_expr(&node.value);
                for obj_id in obj_ids {
                    if self.nodes_arena[obj_id].namespace.is_none() {
                        continue;
                    }

                    let ns = self.nodes_arena[obj_id].get_name();
                    let attr_name = node.attr.id.to_string();
                    let direct_values = self.lookup_values_in_scope(&ns, &attr_name);
                    let direct_containers = self.lookup_containers_in_scope(&ns, &attr_name);

                    if direct_values.is_empty() && direct_containers.is_empty() {
                        if let Some(mro) = self.mro.get(&obj_id).cloned() {
                            for &base_id in mro.iter().skip(1) {
                                let base_ns = self.nodes_arena[base_id].get_name();
                                let base_values = self.lookup_values_in_scope(&base_ns, &attr_name);
                                let base_containers =
                                    self.lookup_containers_in_scope(&base_ns, &attr_name);
                                if !base_values.is_empty() || !base_containers.is_empty() {
                                    resolved.values.union_with(&base_values);
                                    resolved.containers.union_with(&base_containers);
                                    break;
                                }
                            }
                        }
                    } else {
                        resolved.values.union_with(&direct_values);
                        resolved.containers.union_with(&direct_containers);
                    }
                }
                resolved
            }
            Expr::Call(node) => {
                let mut resolved = ShallowValue::default();
                let func_ids = self.get_obj_ids_for_expr(&node.func);
                for func_id in func_ids {
                    if self.class_base_ast_info.contains_key(&func_id) {
                        resolved.values.insert(func_id);
                    }
                    if let Some(ret_ids) = self.function_returns.get(&func_id).cloned() {
                        for ret_id in ret_ids {
                            resolved.values.insert(ret_id);
                        }
                    }
                }
                resolved
            }
            Expr::Tuple(node) => {
                let mut facts = ContainerFacts::default();
                let items = node
                    .elts
                    .iter()
                    .map(|elt| self.resolve_shallow_value(elt))
                    .collect();
                facts.push(ContainerFact::Sequence(items));
                ShallowValue {
                    values: ValueSet::empty(),
                    containers: facts,
                }
            }
            Expr::List(node) => {
                let mut facts = ContainerFacts::default();
                let items = node
                    .elts
                    .iter()
                    .map(|elt| self.resolve_shallow_value(elt))
                    .collect();
                facts.push(ContainerFact::Sequence(items));
                ShallowValue {
                    values: ValueSet::empty(),
                    containers: facts,
                }
            }
            Expr::Dict(node) => {
                let mut items = HashMap::new();
                for item in &node.items {
                    let Some(ref key) = item.key else {
                        continue;
                    };
                    let Some(key) = literal_key_from_expr(key) else {
                        continue;
                    };
                    items
                        .entry(key)
                        .or_insert_with(ShallowValue::default)
                        .union_with(&self.resolve_shallow_value(&item.value));
                }
                let mut facts = ContainerFacts::default();
                if !items.is_empty() {
                    facts.push(ContainerFact::Mapping(items));
                }
                ShallowValue {
                    values: ValueSet::empty(),
                    containers: facts,
                }
            }
            Expr::Subscript(node) => self.resolve_subscript_value(node),
            _ => ShallowValue::default(),
        }
    }

    fn resolve_subscript_value(&mut self, node: &ExprSubscript) -> ShallowValue {
        let container = self.resolve_shallow_value(&node.value);
        let key = literal_key_from_expr(&node.slice);
        container.containers.resolve_subscript(key.as_ref())
    }

    // =====================================================================
    // Visitor methods
    // =====================================================================

    fn visit_module(&mut self, module: &ModModule, line_index: &LineIndex) {
        debug!("Module {}, {}", self.module_name, self.filename);

        let mod_name = self.module_name.clone();
        let fname = self.filename.clone();
        let module_node = self.get_node(Some(""), &mod_name, Flavor::Module);
        let line = line_index.line_index(module.range().start()).get();
        self.associate_node(module_node, &fname, line);

        let ns = self.module_name.clone();
        self.name_stack.push(ns.clone());
        self.scope_stack.push(ns.clone());
        self.context_stack.push(format!("Module {ns}"));

        for stmt in &module.body {
            self.visit_stmt(stmt, line_index);
        }

        self.context_stack.pop();
        self.scope_stack.pop();
        self.name_stack.pop();

        self.add_defines_edge(module_node, None);
    }

    fn visit_stmt(&mut self, stmt: &Stmt, line_index: &LineIndex) {
        match stmt {
            Stmt::ClassDef(node) => self.visit_class_def(node, line_index),
            Stmt::FunctionDef(node) => self.visit_function_def(node, line_index),
            Stmt::Import(node) => self.visit_import(node, line_index),
            Stmt::ImportFrom(node) => self.visit_import_from(node, line_index),
            Stmt::Assign(node) => self.visit_assign(node, line_index),
            Stmt::AugAssign(node) => self.visit_aug_assign(node, line_index),
            Stmt::AnnAssign(node) => self.visit_ann_assign(node, line_index),
            Stmt::For(node) => self.visit_for(node, line_index),
            Stmt::While(node) => self.visit_while(node, line_index),
            Stmt::If(node) => self.visit_if(node, line_index),
            Stmt::With(node) => self.visit_with(node, line_index),
            Stmt::Return(node) => {
                if let Some(ref value) = node.value {
                    let ret_val = self.visit_expr(value, line_index);
                    let mut ret_ids: Vec<NodeId> =
                        self.resolve_shallow_value(value).values.iter().collect();
                    if let Some(ret_id) = ret_val
                        && !ret_ids.contains(&ret_id)
                    {
                        ret_ids.push(ret_id);
                    }
                    // Track return value for the enclosing function/method.
                    // Skip argument sentinels and unknown wildcard nodes —
                    // they don't carry useful type information.
                    for ret_id in ret_ids {
                        let is_sentinel = self.nodes_arena[ret_id].name.contains("^^^argument^^^");
                        let is_unknown = self.nodes_arena[ret_id].namespace.is_none();
                        if !is_sentinel && !is_unknown {
                            let fn_node = self.get_node_of_current_namespace();
                            self.function_returns
                                .entry(fn_node)
                                .or_default()
                                .insert(ret_id);
                        }
                    }
                }
            }
            Stmt::Delete(node) => self.visit_delete(node, line_index),
            Stmt::Expr(node) => {
                self.visit_expr(&node.value, line_index);
            }
            Stmt::Try(node) => self.visit_try(node, line_index),
            Stmt::Raise(node) => {
                if let Some(ref exc) = node.exc {
                    self.visit_expr(exc, line_index);
                }
                if let Some(ref cause) = node.cause {
                    self.visit_expr(cause, line_index);
                }
            }
            Stmt::Assert(node) => {
                self.visit_expr(&node.test, line_index);
                if let Some(ref msg) = node.msg {
                    self.visit_expr(msg, line_index);
                }
            }
            Stmt::Match(node) => self.visit_match(node, line_index),
            Stmt::Global(_)
            | Stmt::Nonlocal(_)
            | Stmt::Pass(_)
            | Stmt::Break(_)
            | Stmt::Continue(_)
            | Stmt::IpyEscapeCommand(_) => {}
            Stmt::TypeAlias(node) => self.visit_type_alias(node, line_index),
        }
    }

    fn visit_class_def(&mut self, node: &StmtClassDef, line_index: &LineIndex) {
        let class_name = node.name.id.to_string();
        debug!(
            "ClassDef {}, {}:{}",
            class_name,
            self.filename,
            line_index.line_index(node.range().start()).get()
        );

        let from_node = self.get_node_of_current_namespace();
        let ns = self.nodes_arena[from_node].get_name();
        let to_node = self.get_node(Some(&ns), &class_name, Flavor::Class);
        if self.add_defines_edge(from_node, Some(to_node)) {
            info!(
                "Def from {} to Class {}",
                self.nodes_arena[from_node].get_name(),
                self.nodes_arena[to_node].get_name()
            );
        }

        let line = line_index.line_index(node.range().start()).get();
        self.associate_node(to_node, &self.filename.clone(), line);
        self.set_value(&class_name, Some(to_node));

        self.class_stack.push(to_node);
        self.name_stack.push(class_name.clone());
        let inner_ns = {
            let inner_node = self.get_node_of_current_namespace();
            self.nodes_arena[inner_node].get_name()
        };
        self.scope_stack.push(inner_ns.clone());
        self.context_stack.push(format!("ClassDef {class_name}"));

        // Gather base class info
        let base_refs = self.class_base_ast_info.entry(to_node).or_default();
        base_refs.clear();

        if let Some(ref arguments) = node.arguments {
            for base in &arguments.args {
                let base_ref = match base {
                    Expr::Name(n) => Some(BaseClassRef::Name(n.id.to_string())),
                    Expr::Attribute(a) => {
                        let parts = self.collect_attr_parts(a);
                        Some(BaseClassRef::Attribute(parts))
                    }
                    _ => None,
                };
                if let Some(br) = base_ref {
                    let refs = self.class_base_ast_info.entry(to_node).or_default();
                    refs.push(br);
                }
                // Visit base to create uses edges
                self.visit_expr(base, line_index);
            }
        }

        // Visit class body
        for stmt in &node.body {
            self.visit_stmt(stmt, line_index);
        }

        self.context_stack.pop();
        self.scope_stack.pop();
        self.name_stack.pop();
        self.class_stack.pop();
    }

    /// Collect the chain of attribute names: a.b.c -> ["a", "b", "c"]
    fn collect_attr_parts(&self, attr: &ExprAttribute) -> Vec<String> {
        let mut parts = match attr.value.as_ref() {
            Expr::Name(n) => vec![n.id.to_string()],
            Expr::Attribute(inner) => self.collect_attr_parts(inner),
            _ => vec![],
        };
        parts.push(attr.attr.id.to_string());
        parts
    }

    fn visit_function_def(&mut self, node: &StmtFunctionDef, line_index: &LineIndex) {
        let func_name = node.name.id.to_string();
        debug!(
            "FunctionDef {}, {}:{}",
            func_name,
            self.filename,
            line_index.line_index(node.range().start()).get()
        );

        // Analyze decorators and determine flavor
        let (self_name, flavor, deco_ids) = self.analyze_function_def(node, line_index);

        let from_node = self.get_node_of_current_namespace();
        let ns = self.nodes_arena[from_node].get_name();
        let to_node = self.get_node(Some(&ns), &func_name, flavor);
        if self.add_defines_edge(from_node, Some(to_node)) {
            info!(
                "Def from {} to Function {}",
                self.nodes_arena[from_node].get_name(),
                self.nodes_arena[to_node].get_name()
            );
        }

        let line = line_index.line_index(node.range().start()).get();
        self.associate_node(to_node, &self.filename.clone(), line);
        self.set_value(&func_name, Some(to_node));

        // Decorator-chain call flow: each concrete (non-wildcard) decorator receives
        // the function as its argument, so emit decorator -> function uses edges.
        for &deco_id in &deco_ids {
            if self.nodes_arena[deco_id].namespace.is_some() {
                if self.add_uses_edge(deco_id, to_node) {
                    info!(
                        "New edge added: decorator {} uses function {}",
                        self.nodes_arena[deco_id].get_name(),
                        self.nodes_arena[to_node].get_name()
                    );
                }
            }
        }

        // Enter function scope
        self.name_stack.push(func_name.clone());
        let inner_ns = {
            let inner_node = self.get_node_of_current_namespace();
            self.nodes_arena[inner_node].get_name()
        };
        self.scope_stack.push(inner_ns.clone());
        self.context_stack.push(format!("FunctionDef {func_name}"));

        // Capture arg names as nonsense nodes
        self.generate_args_nodes(&node.parameters, &inner_ns);

        // Bind self_name to current class (additive insert into ValueSet)
        if let Some(ref sname) = self_name
            && let Some(class_id) = self.get_current_class()
        {
            if let Some(scope) = self.scopes.get_mut(&inner_ns) {
                scope
                    .defs
                    .entry(sname.clone())
                    .or_default()
                    .insert(class_id);
            }
            info!(
                "Method def: setting self name \"{}\" to {}",
                sname,
                self.nodes_arena[class_id].get_name()
            );
        }

        // Analyze default argument values
        self.analyze_arguments(&node.parameters, line_index);

        // Visit type annotations
        if let Some(ref returns) = node.returns {
            self.visit_expr(returns, line_index);
        }
        for arg in node
            .parameters
            .args
            .iter()
            .chain(node.parameters.posonlyargs.iter())
            .chain(node.parameters.kwonlyargs.iter())
        {
            if let Some(ref annotation) = arg.parameter.annotation {
                self.visit_expr(annotation, line_index);
            }
        }
        if let Some(ref va) = node.parameters.vararg
            && let Some(ref annotation) = va.annotation
        {
            self.visit_expr(annotation, line_index);
        }
        if let Some(ref kw) = node.parameters.kwarg
            && let Some(ref annotation) = kw.annotation
        {
            self.visit_expr(annotation, line_index);
        }

        // Visit function body
        for stmt in &node.body {
            self.visit_stmt(stmt, line_index);
        }

        // Exit function scope
        self.context_stack.pop();
        self.scope_stack.pop();
        self.name_stack.pop();
    }

    fn analyze_function_def(
        &mut self,
        node: &StmtFunctionDef,
        line_index: &LineIndex,
    ) -> (Option<String>, Flavor, Vec<NodeId>) {
        // Visit decorators; collect resolved node IDs for decorator-chain flow.
        let mut deco_names = Vec::new();
        let mut deco_ids: Vec<NodeId> = Vec::new();
        for deco in &node.decorator_list {
            let deco_node = self.visit_expr(&deco.expression, line_index);
            if let Some(did) = deco_node {
                deco_names.push(self.nodes_arena[did].name.clone());
                deco_ids.push(did);
            }
        }

        // Determine flavor
        let in_class_ns = self
            .context_stack
            .last()
            .is_some_and(|c| c.starts_with("ClassDef"));

        let flavor = if !in_class_ns {
            Flavor::Function
        } else if deco_names.iter().any(|n| n == "staticmethod") {
            Flavor::StaticMethod
        } else if deco_names.iter().any(|n| n == "classmethod") {
            Flavor::ClassMethod
        } else {
            Flavor::Method
        };

        // Get self_name
        let self_name = if matches!(flavor, Flavor::Method | Flavor::ClassMethod) {
            let posargs = &node.parameters.args;
            if !posargs.is_empty() {
                Some(posargs[0].parameter.name.id.to_string())
            } else {
                None
            }
        } else {
            None
        };

        (self_name, flavor, deco_ids)
    }

    fn generate_args_nodes(&mut self, params: &Parameters, inner_ns: &str) {
        let nonsense_node = self.get_node(Some(inner_ns), "^^^argument^^^", Flavor::Unspecified);
        if let Some(scope) = self.scopes.get_mut(inner_ns) {
            for a in &params.args {
                scope
                    .defs
                    .entry(a.parameter.name.id.to_string())
                    .or_default()
                    .insert(nonsense_node);
            }
            for a in &params.posonlyargs {
                scope
                    .defs
                    .entry(a.parameter.name.id.to_string())
                    .or_default()
                    .insert(nonsense_node);
            }
            if let Some(ref va) = params.vararg {
                scope
                    .defs
                    .entry(va.name.id.to_string())
                    .or_default()
                    .insert(nonsense_node);
            }
            for a in &params.kwonlyargs {
                scope
                    .defs
                    .entry(a.parameter.name.id.to_string())
                    .or_default()
                    .insert(nonsense_node);
            }
            if let Some(ref kw) = params.kwarg {
                scope
                    .defs
                    .entry(kw.name.id.to_string())
                    .or_default()
                    .insert(nonsense_node);
            }
        }
    }

    fn analyze_arguments(&mut self, params: &Parameters, line_index: &LineIndex) {
        // Bind positional args with defaults
        for arg in &params.args {
            if let Some(ref default) = arg.default {
                let val = self.visit_expr(default, line_index);
                self.bind_target_to_value(
                    &Expr::Name(ExprName {
                        node_index: AtomicNodeIndex::default(),
                        range: arg.parameter.name.range(),
                        id: arg.parameter.name.id.clone(),
                        ctx: ExprContext::Store,
                    }),
                    val,
                );
            }
        }

        // Keyword-only args with defaults
        for arg in &params.kwonlyargs {
            if let Some(ref default) = arg.default {
                let val = self.visit_expr(default, line_index);
                self.bind_target_to_value(
                    &Expr::Name(ExprName {
                        node_index: AtomicNodeIndex::default(),
                        range: arg.parameter.name.range(),
                        id: arg.parameter.name.id.clone(),
                        ctx: ExprContext::Store,
                    }),
                    val,
                );
            }
        }
    }

    fn visit_import(&mut self, node: &StmtImport, line_index: &LineIndex) {
        debug!(
            "Import, {}:{}",
            self.filename,
            line_index.line_index(node.range().start()).get()
        );

        for alias in &node.names {
            let src_name = alias.name.id.to_string();
            let from_node = self.get_node_of_current_namespace();
            let mod_node = self.get_node(Some(""), &src_name, Flavor::Module);

            let alias_name = if let Some(ref asname) = alias.asname {
                asname.id.to_string()
            } else {
                // For `import a.b.c`, the bound name is just `a`
                if let Some(first) = src_name.split('.').next() {
                    first.to_string()
                } else {
                    src_name.clone()
                }
            };

            self.add_uses_edge(from_node, mod_node);
            self.set_value(&alias_name, Some(mod_node));
        }
    }

    fn visit_import_from(&mut self, node: &StmtImportFrom, line_index: &LineIndex) {
        debug!(
            "ImportFrom, {}:{}",
            self.filename,
            line_index.line_index(node.range().start()).get()
        );

        let from_node = self.get_node_of_current_namespace();

        // Resolve the target module name
        let tgt_name = if let Some(ref module) = node.module {
            let module_str = module.id.to_string();
            if node.level > 0 {
                // Relative import
                let parts: Vec<&str> = self.module_name.split('.').collect();
                let level = node.level as usize;
                if level <= parts.len() {
                    let base = parts[..parts.len() - level].join(".");
                    if module_str.is_empty() {
                        base
                    } else if base.is_empty() {
                        module_str
                    } else {
                        format!("{base}.{module_str}")
                    }
                } else {
                    module_str
                }
            } else {
                module_str
            }
        } else {
            // `from . import foo` — module is None
            let parts: Vec<&str> = self.module_name.split('.').collect();
            let level = node.level as usize;
            if level <= parts.len() {
                parts[..parts.len() - level].join(".")
            } else {
                String::new()
            }
        };

        // Handle `from mod import *` as a separate path.
        if node.names.len() == 1 && node.names[0].name.id.as_str() == "*" {
            self.handle_star_import(from_node, &tgt_name);
            return;
        }

        for alias in &node.names {
            let item_name = alias.name.id.to_string();
            let alias_name = if let Some(ref asname) = alias.asname {
                asname.id.to_string()
            } else {
                item_name.clone()
            };

            // Check if the import is a sub-module.
            let full_name = format!("{tgt_name}.{item_name}");
            if self.module_to_filename.contains_key(&full_name) {
                let mod_node = self.get_node(Some(""), &full_name, Flavor::Module);
                self.set_value(&alias_name, Some(mod_node));
                self.add_uses_edge(from_node, mod_node);
                continue;
            }

            // Try scope-based resolution: look up the name in the source
            // module's accumulated scope.  This propagates re-exports and
            // alias chains without needing an extra remap pass.
            let resolved = self.lookup_values_in_scope(&tgt_name, &item_name);
            if !resolved.is_empty() {
                for id in resolved.iter() {
                    self.set_value(&alias_name, Some(id));
                    self.add_uses_edge(from_node, id);
                    debug!(
                        "Import scope-resolved: {} -> {}",
                        alias_name,
                        self.nodes_arena[id].get_name()
                    );
                }
                continue;
            }

            // Fall back to an ImportedItem placeholder.  resolve_imports
            // will attempt to chase the chain in postprocess.
            let to_node = self.get_node(Some(&tgt_name), &item_name, Flavor::ImportedItem);
            self.set_value(&alias_name, Some(to_node));
            if self.add_uses_edge(from_node, to_node) {
                info!(
                    "New edge added for Use from {} to ImportFrom {}",
                    self.nodes_arena[from_node].get_name(),
                    self.nodes_arena[to_node].get_name()
                );
            }
        }
    }

    /// Inject exported names from `tgt_module` into the current scope.
    ///
    /// Used by `from mod import *`.  The filter is:
    ///
    /// - If the source module declares `__all__` with statically-knowable
    ///   string literals, only those names are injected (even private ones
    ///   that are explicitly listed, excluding public names that are not).
    /// - Otherwise, all non-private (no leading `_`) names with non-empty
    ///   ValueSets are injected.
    ///
    /// Empty-ValueSet placeholders are always skipped (they produce dead
    /// wildcard references with no useful information).
    fn handle_star_import(&mut self, from_node: NodeId, tgt_module: &str) {
        // Collect the source module's bindings while holding an immutable
        // borrow on self.scopes.
        let bindings: Vec<(String, ValueSet)> = if let Some(scope) = self.scopes.get(tgt_module) {
            let all_exports = scope.all_exports.clone();
            scope
                .defs
                .iter()
                .filter(|(name, vs)| {
                    // Always skip empty placeholders.
                    if vs.is_empty() {
                        return false;
                    }
                    // If __all__ is statically known, use it as the
                    // definitive export list (INV-1).
                    if let Some(ref exports) = all_exports {
                        return exports.contains(name.as_str());
                    }
                    // No __all__: skip private names (INV-2).
                    !name.starts_with('_')
                })
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        } else {
            Vec::new()
        };

        for (name, values) in bindings {
            for id in values.iter() {
                self.set_value(&name, Some(id));
                self.add_uses_edge(from_node, id);
                debug!(
                    "Star-import: {} -> {}",
                    name,
                    self.nodes_arena[id].get_name()
                );
            }
        }
    }

    fn visit_assign(&mut self, node: &StmtAssign, line_index: &LineIndex) {
        for target in &node.targets {
            self.analyze_binding_with_rhs(target, &node.value, line_index);
        }
    }

    /// Bind a target expression using the RHS expression directly.
    ///
    /// When both target and RHS are tuple/list literals, performs positional
    /// matching (including starred unpacking) so each target gets its own
    /// precise value rather than the same collapsed approximation.
    ///
    /// Falls back to the simple one-value binding for non-literal RHS or
    /// when only the LHS is a tuple/list.
    fn analyze_binding_with_rhs(&mut self, target: &Expr, rhs: &Expr, line_index: &LineIndex) {
        let target_elts: Option<Vec<&Expr>> = match target {
            Expr::Tuple(t) => Some(t.elts.iter().collect()),
            Expr::List(t) => Some(t.elts.iter().collect()),
            _ => None,
        };
        let rhs_elts: Option<Vec<&Expr>> = match rhs {
            Expr::Tuple(r) => Some(r.elts.iter().collect()),
            Expr::List(r) => Some(r.elts.iter().collect()),
            _ => None,
        };

        if let (Some(targets), Some(rhs_elts)) = (target_elts, rhs_elts) {
            // Both sides are tuple/list literals: do positional matching.
            self.bind_positional_tuple_elts(&targets, &rhs_elts, line_index);
        } else {
            // Fallback: visit RHS once and bind all targets to that value.
            let value_node = self.visit_expr(rhs, line_index);
            self.analyze_binding_simple(target, value_node, line_index);
            let shallow = self.resolve_shallow_value(rhs);
            self.bind_target_to_shallow_value(target, &shallow);

            // For Call RHS, visit_expr collapses multiple return candidates to
            // a single `first_ret`.  Propagate all remaining return types into
            // the target binding so multi-return functions keep every plausible
            // type in the variable's ValueSet (INV-2).
            if let Expr::Call(call) = rhs {
                let extra_rets: Vec<NodeId> = {
                    let func_cands = self.get_obj_ids_for_expr(&call.func);
                    let mut all_rets = Vec::new();
                    for &fid in &func_cands {
                        if let Some(rets) = self.function_returns.get(&fid).cloned() {
                            for ret_id in rets {
                                if Some(ret_id) != value_node && !all_rets.contains(&ret_id) {
                                    all_rets.push(ret_id);
                                }
                            }
                        }
                    }
                    all_rets
                };
                for ret_id in extra_rets {
                    self.analyze_binding_simple(target, Some(ret_id), line_index);
                }
            }
        }
    }

    /// Positional matching of target slots to RHS elements, handling starred (`*`) targets.
    ///
    /// Algorithm:
    /// - Non-starred targets before `*` → match RHS from the left.
    /// - Non-starred targets after  `*` → match RHS from the right.
    /// - Starred target              `*` → union of all middle RHS elements.
    /// - Without any starred target    → plain one-to-one positional match.
    ///
    /// Each matched RHS element is individually visited (to produce uses edges)
    /// and then bound to the corresponding target slot.
    fn bind_positional_tuple_elts(
        &mut self,
        targets: &[&Expr],
        rhs_elts: &[&Expr],
        line_index: &LineIndex,
    ) {
        let n_rhs = rhs_elts.len();
        let star_idx = targets.iter().position(|e| matches!(*e, Expr::Starred(_)));

        if let Some(star_pos) = star_idx {
            let n_pre = star_pos;
            let n_post = targets.len() - star_pos - 1;

            // Pre-starred slots: positional from left.
            for i in 0..n_pre {
                if i < n_rhs {
                    self.visit_expr(rhs_elts[i], line_index);
                    let shallow = self.resolve_shallow_value(rhs_elts[i]);
                    self.bind_target_to_shallow_value(targets[i], &shallow);
                }
            }

            // Post-starred slots: positional from right.
            for j in 0..n_post {
                let t_idx = star_pos + 1 + j;
                let r_idx = n_rhs.saturating_sub(n_post) + j;
                if r_idx < n_rhs && r_idx >= n_pre {
                    self.visit_expr(rhs_elts[r_idx], line_index);
                    let shallow = self.resolve_shallow_value(rhs_elts[r_idx]);
                    self.bind_target_to_shallow_value(targets[t_idx], &shallow);
                }
            }

            // Starred slot: union of all middle elements.
            if let Expr::Starred(starred) = targets[star_pos] {
                let middle_start = n_pre;
                let middle_end = n_rhs.saturating_sub(n_post);
                let inner = &*starred.value;
                for i in middle_start..middle_end {
                    self.visit_expr(rhs_elts[i], line_index);
                    let shallow = self.resolve_shallow_value(rhs_elts[i]);
                    self.bind_target_to_shallow_value(inner, &shallow);
                }
            }
        } else {
            // No starred: simple one-to-one positional matching.
            for (i, &tgt) in targets.iter().enumerate() {
                if i < n_rhs {
                    self.visit_expr(rhs_elts[i], line_index);
                    let shallow = self.resolve_shallow_value(rhs_elts[i]);
                    self.bind_target_to_shallow_value(tgt, &shallow);
                }
            }
        }
    }

    fn visit_aug_assign(&mut self, node: &StmtAugAssign, line_index: &LineIndex) {
        let value_node = self.visit_expr(&node.value, line_index);
        self.analyze_binding_simple(&node.target, value_node, line_index);
    }

    fn visit_ann_assign(&mut self, node: &StmtAnnAssign, line_index: &LineIndex) {
        if let Some(ref value) = node.value {
            let value_node = self.visit_expr(value, line_index);
            self.analyze_binding_simple(&node.target, value_node, line_index);
            let shallow = self.resolve_shallow_value(value);
            self.bind_target_to_shallow_value(&node.target, &shallow);
        } else {
            // Just a type declaration
            self.bind_target_to_value(&node.target, None);
        }
        // Visit annotation for uses edges
        self.visit_expr(&node.annotation, line_index);
    }

    fn visit_for(&mut self, node: &StmtFor, line_index: &LineIndex) {
        debug!(
            "For-loop, {}:{}",
            self.filename,
            line_index.line_index(node.range().start()).get()
        );

        let iter_node = self.visit_expr(&node.iter, line_index);

        // Emit iterator protocol edges when iterating over a known class instance.
        if let Some(obj_id) = iter_node
            && self.class_base_ast_info.contains_key(&obj_id)
        {
            let methods: &[&str] = if node.is_async {
                &["__aiter__", "__anext__"]
            } else {
                &["__iter__", "__next__"]
            };
            self.emit_protocol_edges(obj_id, methods);
        }

        // Bind target to iter value
        self.analyze_binding_simple(&node.target, iter_node, line_index);

        for stmt in &node.body {
            self.visit_stmt(stmt, line_index);
        }
        for stmt in &node.orelse {
            self.visit_stmt(stmt, line_index);
        }
    }

    fn visit_while(&mut self, node: &StmtWhile, line_index: &LineIndex) {
        self.visit_expr(&node.test, line_index);
        for stmt in &node.body {
            self.visit_stmt(stmt, line_index);
        }
        for stmt in &node.orelse {
            self.visit_stmt(stmt, line_index);
        }
    }

    fn visit_if(&mut self, node: &StmtIf, line_index: &LineIndex) {
        self.visit_expr(&node.test, line_index);
        for stmt in &node.body {
            self.visit_stmt(stmt, line_index);
        }
        for clause in &node.elif_else_clauses {
            if let Some(ref test) = clause.test {
                self.visit_expr(test, line_index);
            }
            for stmt in &clause.body {
                self.visit_stmt(stmt, line_index);
            }
        }
    }

    fn visit_with(&mut self, node: &StmtWith, line_index: &LineIndex) {
        for item in &node.items {
            let cm_node = self.visit_expr(&item.context_expr, line_index);

            // Emit context-manager protocol edges when the CM is a known class instance.
            if let Some(obj_id) = cm_node
                && self.class_base_ast_info.contains_key(&obj_id)
            {
                let methods: &[&str] = if node.is_async {
                    &["__aenter__", "__aexit__"]
                } else {
                    &["__enter__", "__exit__"]
                };
                self.emit_protocol_edges(obj_id, methods);
            }

            if let Some(ref vars) = item.optional_vars {
                if let Expr::Name(_) = vars.as_ref() {
                    self.analyze_binding_simple(vars, cm_node, line_index);
                } else {
                    self.visit_expr(vars, line_index);
                }
            }
        }
        for stmt in &node.body {
            self.visit_stmt(stmt, line_index);
        }
    }

    fn visit_delete(&mut self, node: &StmtDelete, line_index: &LineIndex) {
        for target in &node.targets {
            match target {
                Expr::Attribute(a) => {
                    // `del obj.attr` → emit __delattr__ protocol edge when receiver is known.
                    let obj_ids = self.get_obj_ids_for_expr(&a.value);
                    for obj_id in obj_ids {
                        if self.class_base_ast_info.contains_key(&obj_id) {
                            self.emit_protocol_edges(obj_id, &["__delattr__"]);
                        }
                    }
                }
                Expr::Subscript(s) => {
                    // `del obj[key]` → emit __delitem__ protocol edge when receiver is known.
                    let obj_ids = self.get_obj_ids_for_expr(&s.value);
                    for obj_id in obj_ids {
                        if self.class_base_ast_info.contains_key(&obj_id) {
                            self.emit_protocol_edges(obj_id, &["__delitem__"]);
                        }
                    }
                    // Visit the subscript key for any side-effect calls.
                    self.visit_expr(&s.slice, line_index);
                }
                _ => {
                    // `del name` — local unbind only, no protocol edge.
                    self.visit_expr(target, line_index);
                }
            }
        }
    }

    fn visit_try(&mut self, node: &StmtTry, line_index: &LineIndex) {
        for stmt in &node.body {
            self.visit_stmt(stmt, line_index);
        }
        for handler in &node.handlers {
            let ExceptHandler::ExceptHandler(h) = handler;
            if let Some(ref type_expr) = h.type_ {
                self.visit_expr(type_expr, line_index);
            }
            for stmt in &h.body {
                self.visit_stmt(stmt, line_index);
            }
        }
        for stmt in &node.orelse {
            self.visit_stmt(stmt, line_index);
        }
        for stmt in &node.finalbody {
            self.visit_stmt(stmt, line_index);
        }
    }

    fn visit_match(&mut self, node: &StmtMatch, line_index: &LineIndex) {
        self.visit_expr(&node.subject, line_index);
        for case in &node.cases {
            self.visit_pattern(&case.pattern, line_index);
            if let Some(ref guard) = case.guard {
                self.visit_expr(guard, line_index);
            }
            for stmt in &case.body {
                self.visit_stmt(stmt, line_index);
            }
        }
    }

    fn visit_pattern(&mut self, pattern: &Pattern, line_index: &LineIndex) {
        match pattern {
            Pattern::MatchValue(p) => {
                self.visit_expr(&p.value, line_index);
            }
            Pattern::MatchSingleton(_) => {}
            Pattern::MatchSequence(p) => {
                for pat in &p.patterns {
                    self.visit_pattern(pat, line_index);
                }
            }
            Pattern::MatchMapping(p) => {
                for key in &p.keys {
                    self.visit_expr(key, line_index);
                }
                for pat in &p.patterns {
                    self.visit_pattern(pat, line_index);
                }
                if let Some(ref rest) = p.rest {
                    self.set_value(rest.id.as_ref(), None);
                }
            }
            Pattern::MatchClass(p) => {
                self.visit_expr(&p.cls, line_index);
                for pat in &p.arguments.patterns {
                    self.visit_pattern(pat, line_index);
                }
                for kw in &p.arguments.keywords {
                    self.visit_pattern(&kw.pattern, line_index);
                }
            }
            Pattern::MatchStar(p) => {
                if let Some(ref name) = p.name {
                    self.set_value(name.id.as_ref(), None);
                }
            }
            Pattern::MatchAs(p) => {
                if let Some(ref pattern) = p.pattern {
                    self.visit_pattern(pattern, line_index);
                }
                if let Some(ref name) = p.name {
                    self.set_value(name.id.as_ref(), None);
                }
            }
            Pattern::MatchOr(p) => {
                for pat in &p.patterns {
                    self.visit_pattern(pat, line_index);
                }
            }
        }
    }

    fn visit_type_alias(&mut self, node: &StmtTypeAlias, line_index: &LineIndex) {
        if let Expr::Name(ref name_expr) = *node.name {
            let alias_name = name_expr.id.to_string();
            let from_node = self.get_node_of_current_namespace();
            let ns = self.nodes_arena[from_node].get_name();
            let to_node = self.get_node(Some(&ns), &alias_name, Flavor::Name);
            self.add_defines_edge(from_node, Some(to_node));
            let line = line_index.line_index(node.range().start()).get();
            self.associate_node(to_node, &self.filename.clone(), line);
            self.set_value(&alias_name, Some(to_node));
        }
        self.visit_expr(&node.value, line_index);
    }

    // =====================================================================
    // Expression visitors
    // =====================================================================

    /// Visit an expression and return the NodeId it resolves to, if any.
    fn visit_expr(&mut self, expr: &Expr, line_index: &LineIndex) -> Option<NodeId> {
        match expr {
            Expr::Name(node) => self.visit_name(node, line_index),
            Expr::Attribute(node) => self.visit_attribute(node, line_index),
            Expr::Call(node) => self.visit_call(node, line_index),
            Expr::Lambda(node) => self.visit_lambda(node, line_index),
            Expr::ListComp(node) => self.visit_list_comp(node, line_index),
            Expr::SetComp(node) => self.visit_set_comp(node, line_index),
            Expr::DictComp(node) => self.visit_dict_comp(node, line_index),
            Expr::Generator(node) => self.visit_generator(node, line_index),
            Expr::BoolOp(node) => {
                let mut last = None;
                for val in &node.values {
                    last = self.visit_expr(val, line_index);
                }
                last
            }
            Expr::BinOp(node) => {
                self.visit_expr(&node.left, line_index);
                self.visit_expr(&node.right, line_index)
            }
            Expr::UnaryOp(node) => self.visit_expr(&node.operand, line_index),
            Expr::If(node) => {
                self.visit_expr(&node.test, line_index);
                let body_val = self.visit_expr(&node.body, line_index);
                self.visit_expr(&node.orelse, line_index);
                body_val
            }
            Expr::Dict(node) => {
                for item in &node.items {
                    if let Some(ref key) = item.key {
                        self.visit_expr(key, line_index);
                    }
                    self.visit_expr(&item.value, line_index);
                }
                None
            }
            Expr::Set(node) => {
                for elt in &node.elts {
                    self.visit_expr(elt, line_index);
                }
                None
            }
            Expr::Tuple(node) => {
                let mut last = None;
                for elt in &node.elts {
                    last = self.visit_expr(elt, line_index);
                }
                last
            }
            Expr::List(node) => {
                for elt in &node.elts {
                    self.visit_expr(elt, line_index);
                }
                None
            }
            Expr::Subscript(node) => {
                let resolved = self.resolve_subscript_value(node);
                if !resolved.values.is_empty() {
                    let from_node = self.get_node_of_current_namespace();
                    for to in resolved.values.iter() {
                        self.add_uses_edge(from_node, to);
                    }
                }
                let val = self.visit_expr(&node.value, line_index);
                self.visit_expr(&node.slice, line_index);
                resolved.first_value().or(val)
            }
            Expr::Starred(node) => self.visit_expr(&node.value, line_index),
            Expr::Await(node) => self.visit_expr(&node.value, line_index),
            Expr::Yield(node) => {
                if let Some(ref value) = node.value {
                    self.visit_expr(value, line_index)
                } else {
                    None
                }
            }
            Expr::YieldFrom(node) => self.visit_expr(&node.value, line_index),
            Expr::Compare(node) => {
                self.visit_expr(&node.left, line_index);
                for comp in &node.comparators {
                    self.visit_expr(comp, line_index);
                }
                None
            }
            Expr::Slice(node) => {
                if let Some(ref lower) = node.lower {
                    self.visit_expr(lower, line_index);
                }
                if let Some(ref upper) = node.upper {
                    self.visit_expr(upper, line_index);
                }
                if let Some(ref step) = node.step {
                    self.visit_expr(step, line_index);
                }
                None
            }
            Expr::Named(node) => {
                let val = self.visit_expr(&node.value, line_index);
                self.bind_target_to_value(&node.target, val);
                let shallow = self.resolve_shallow_value(&node.value);
                self.bind_target_to_shallow_value(&node.target, &shallow);
                val
            }
            Expr::FString(node) => {
                for element in node.value.elements() {
                    if let InterpolatedStringElement::Interpolation(interp) = element {
                        self.visit_expr(&interp.expression, line_index);
                    }
                }
                None
            }
            Expr::StringLiteral(_)
            | Expr::BytesLiteral(_)
            | Expr::NumberLiteral(_)
            | Expr::BooleanLiteral(_)
            | Expr::NoneLiteral(_)
            | Expr::EllipsisLiteral(_) => None,
            Expr::TString(node) => {
                for element in node.value.elements() {
                    if let InterpolatedStringElement::Interpolation(interp) = element {
                        self.visit_expr(&interp.expression, line_index);
                    }
                }
                None
            }
            Expr::IpyEscapeCommand(_) => None,
        }
    }

    fn visit_name(&mut self, node: &ExprName, _line_index: &LineIndex) -> Option<NodeId> {
        if node.ctx == ExprContext::Load {
            let tgt_name = node.id.to_string();
            let values = self.get_values(&tgt_name);
            let current_class = self.get_current_class();

            if values.is_empty() {
                // Local with no resolved value — do not emit an edge.
                if self.is_local(&tgt_name) {
                    return None;
                }
                // Unknown namespace — emit wildcard edge.
                let to_id = self.get_node(None, &tgt_name, Flavor::Unknown);
                let from_node = self.get_node_of_current_namespace();
                if self.add_uses_edge(from_node, to_id) {
                    info!(
                        "New edge added for Use from {} to Name {} (wildcard)",
                        self.nodes_arena[from_node].get_name(),
                        self.nodes_arena[to_id].get_name()
                    );
                }
                return Some(to_id);
            }

            // Emit a uses edge for every value in the set (INV-1 / INV-2).
            let from_node = self.get_node_of_current_namespace();
            let mut first = None;
            for to in values.iter() {
                // Do not add a uses edge to the containing class itself.
                if let Some(cls) = current_class
                    && to == cls
                {
                    if first.is_none() {
                        first = Some(to);
                    }
                    continue;
                }
                if self.add_uses_edge(from_node, to) {
                    info!(
                        "New edge added for Use from {} to Name {}",
                        self.nodes_arena[from_node].get_name(),
                        self.nodes_arena[to].get_name()
                    );
                }
                if first.is_none() {
                    first = Some(to);
                }
            }
            first
        } else {
            None
        }
    }

    fn visit_attribute(&mut self, node: &ExprAttribute, line_index: &LineIndex) -> Option<NodeId> {
        if node.ctx == ExprContext::Load {
            let attr_name = node.attr.id.to_string();
            let from_node = self.get_node_of_current_namespace();

            // Collect all possible object NodeIds for the base expression.
            // This handles the multi-value case (INV-1): when `x` was bound in
            // both branches of an if/else, we resolve attr for each candidate.
            let obj_ids = self.get_obj_ids_for_expr(&node.value);

            if !obj_ids.is_empty() {
                let mut first_result: Option<NodeId> = None;
                let mut found_any = false;

                for &obj_id in &obj_ids {
                    if self.nodes_arena[obj_id].namespace.is_none() {
                        continue; // skip wildcards as objects
                    }
                    let ns = self.nodes_arena[obj_id].get_name();

                    // Direct lookup in the object's scope, then MRO.
                    let attr_result = self.lookup_in_scope(&ns, &attr_name).or_else(|| {
                        if let Some(mro) = self.mro.get(&obj_id).cloned() {
                            for &base_id in mro.iter().skip(1) {
                                let base_ns = self.nodes_arena[base_id].get_name();
                                if let Some(val) = self.lookup_in_scope(&base_ns, &attr_name) {
                                    return Some(val);
                                }
                            }
                        }
                        None
                    });

                    let emit_id = if let Some(attr_id) = attr_result {
                        // Attr is known: emit uses edge to the concrete node.
                        if self.add_uses_edge(from_node, attr_id) {
                            info!(
                                "New edge added for Use from {} to {} (multi-value attr)",
                                self.nodes_arena[from_node].get_name(),
                                self.nodes_arena[attr_id].get_name()
                            );
                        }
                        let attr_ns = self.nodes_arena[attr_id].namespace.clone();
                        if attr_ns.is_some() {
                            self.remove_wild(from_node, attr_id, &attr_name);
                        }
                        attr_id
                    } else {
                        // Obj is known but attr is not: create a placeholder
                        // attribute node so callers can chain off it.
                        let attr_node = self.get_node(Some(&ns), &attr_name, Flavor::Attribute);
                        self.add_uses_edge(from_node, attr_node);
                        self.remove_wild(from_node, obj_id, &attr_name);
                        attr_node
                    };

                    found_any = true;
                    if first_result.is_none() {
                        first_result = Some(emit_id);
                    }
                }

                if found_any {
                    // Always visit the value expression for side-effect uses edges
                    // even when type resolution succeeded.  Without this, chained
                    // calls like `self.to_A().b` would resolve the type of the call
                    // result but never emit the uses edge to `to_A` itself.
                    self.visit_expr(&node.value, line_index);
                    return first_result;
                }
            }

            // Fallback: visit the value expression to capture any side-effect
            // uses edges from an unresolvable base expression.
            self.visit_expr(&node.value, line_index)
        } else {
            // Store/Del context — no uses edge needed.
            None
        }
    }

    fn visit_call(&mut self, node: &ExprCall, line_index: &LineIndex) -> Option<NodeId> {
        // Visit args
        for arg in &node.arguments.args {
            self.visit_expr(arg, line_index);
        }
        for kw in &node.arguments.keywords {
            self.visit_expr(&kw.value, line_index);
        }

        // Try to resolve builtins (super, str, repr)
        if let Some(result_id) = self.resolve_builtins_from_call(node, line_index) {
            let from_node = self.get_node_of_current_namespace();
            if self.add_uses_edge(from_node, result_id) {
                info!(
                    "New edge added for Use from {} to {} (via resolved call)",
                    self.nodes_arena[from_node].get_name(),
                    self.nodes_arena[result_id].get_name()
                );
            }
            return Some(result_id);
        }

        // General case: visit the function expression
        let func_node = self.visit_expr(&node.func, line_index);

        // If calling a known class, add __init__ edge and return the class node
        // as the "instance type" so downstream attribute access resolves correctly.
        if let Some(func_id) = func_node
            && self.class_base_ast_info.contains_key(&func_id)
        {
            let from_node = self.get_node_of_current_namespace();
            let func_name = self.nodes_arena[func_id].get_name();
            let init_node = self.get_node(Some(&func_name), "__init__", Flavor::Method);
            if self.add_uses_edge(from_node, init_node) {
                info!(
                    "New edge added for Use from {} to {} (class instantiation)",
                    self.nodes_arena[from_node].get_name(),
                    self.nodes_arena[init_node].get_name()
                );
            }
            return func_node; // class node == instance type
        }

        // Collect all possible function node candidates for return-type
        // propagation.  `visit_expr` may return a placeholder when the callee
        // is reached via a sentinel-bound `self` parameter (e.g.
        // `self.to_A()` returns a placeholder instead of the real `to_A`
        // method node).  `get_obj_ids_for_expr` does pure type resolution and
        // can find the concrete callee that `visit_expr` misses.
        let func_candidates: Vec<NodeId> = {
            let mut cs = self.get_obj_ids_for_expr(&node.func);
            if let Some(fn_id) = func_node {
                if !cs.contains(&fn_id) {
                    cs.push(fn_id);
                }
            }
            cs
        };

        // For function calls with known return types, propagate all return
        // candidates so that callers can resolve attributes on the returned
        // object and multi-return call sites don't under-approximate.
        let from_node = self.get_node_of_current_namespace();
        let mut first_ret: Option<NodeId> = None;
        for &fid in &func_candidates {
            if let Some(ret_ids) = self.function_returns.get(&fid).cloned() {
                for ret_id in &ret_ids {
                    if self.add_uses_edge(from_node, *ret_id) {
                        info!(
                            "New edge added for Use from {} to {} (return-value propagation)",
                            self.nodes_arena[from_node].get_name(),
                            self.nodes_arena[*ret_id].get_name()
                        );
                    }
                    if first_ret.is_none() {
                        first_ret = Some(*ret_id);
                    }
                }
            }
        }
        if first_ret.is_some() {
            return first_ret;
        }

        func_node
    }

    fn visit_lambda(&mut self, node: &ExprLambda, line_index: &LineIndex) -> Option<NodeId> {
        let label = "lambda";
        let parent_ns = {
            let parent_node = self.get_node_of_current_namespace();
            self.nodes_arena[parent_node].get_name()
        };
        let inner_ns = format!("{parent_ns}.{label}");

        // Ensure scope exists
        if !self.scopes.contains_key(&inner_ns) {
            let mut names = HashSet::new();
            if let Some(ref params) = node.parameters {
                for p in &params.args {
                    names.insert(p.parameter.name.id.to_string());
                }
                for p in &params.posonlyargs {
                    names.insert(p.parameter.name.id.to_string());
                }
                for p in &params.kwonlyargs {
                    names.insert(p.parameter.name.id.to_string());
                }
                if let Some(ref va) = params.vararg {
                    names.insert(va.name.id.to_string());
                }
                if let Some(ref kw) = params.kwarg {
                    names.insert(kw.name.id.to_string());
                }
            }
            self.scopes
                .insert(inner_ns.clone(), ScopeInfo::from_names(label, &names));
        }

        self.name_stack.push(label.to_string());
        self.scope_stack.push(inner_ns.clone());
        self.context_stack.push(label.to_string());

        if let Some(ref params) = node.parameters {
            self.generate_args_nodes(params, &inner_ns);
            self.analyze_arguments(params, line_index);
        }
        self.visit_expr(&node.body, line_index);

        self.context_stack.pop();
        self.scope_stack.pop();
        self.name_stack.pop();

        // Add defines edge for the lambda
        let from_node = self.get_node_of_current_namespace();
        let from_name = self.nodes_arena[from_node].get_name();
        let to_node = self.get_node(Some(&from_name), label, Flavor::Namespace);
        self.add_defines_edge(from_node, Some(to_node));

        Some(to_node)
    }

    fn visit_list_comp(&mut self, node: &ExprListComp, line_index: &LineIndex) -> Option<NodeId> {
        self.analyze_comprehension(
            &node.generators,
            Some(&node.elt),
            None,
            "listcomp",
            line_index,
        )
    }

    fn visit_set_comp(&mut self, node: &ExprSetComp, line_index: &LineIndex) -> Option<NodeId> {
        self.analyze_comprehension(
            &node.generators,
            Some(&node.elt),
            None,
            "setcomp",
            line_index,
        )
    }

    fn visit_dict_comp(&mut self, node: &ExprDictComp, line_index: &LineIndex) -> Option<NodeId> {
        self.analyze_comprehension(
            &node.generators,
            Some(&node.key),
            Some(&node.value),
            "dictcomp",
            line_index,
        )
    }

    fn visit_generator(&mut self, node: &ExprGenerator, line_index: &LineIndex) -> Option<NodeId> {
        self.analyze_comprehension(
            &node.generators,
            Some(&node.elt),
            None,
            "genexpr",
            line_index,
        )
    }

    /// Emit uses edges for Python protocol dunder methods on a known class/instance.
    ///
    /// `obj_id` must be the NodeId that the iterated or context expression resolves to.
    /// Only emits edges when `obj_id` is a class we analysed (present in
    /// `class_base_ast_info`).  Each method in `method_names` is looked up first
    /// directly in the class scope, then through the MRO chain.
    fn emit_protocol_edges(&mut self, obj_id: NodeId, method_names: &[&str]) {
        let from_node = self.get_node_of_current_namespace();
        let class_ns = self.nodes_arena[obj_id].get_name();

        for &method_name in method_names {
            // Direct lookup in the class scope.
            if let Some(method_id) = self.lookup_in_scope(&class_ns, method_name) {
                if self.add_uses_edge(from_node, method_id) {
                    info!(
                        "New edge added for Use from {} to {} (protocol: {})",
                        self.nodes_arena[from_node].get_name(),
                        self.nodes_arena[method_id].get_name(),
                        method_name
                    );
                }
            } else {
                // Fall back to MRO chain.
                if let Some(mro) = self.mro.get(&obj_id).cloned() {
                    for &base_id in mro.iter().skip(1) {
                        let base_ns = self.nodes_arena[base_id].get_name();
                        if let Some(method_id) = self.lookup_in_scope(&base_ns, method_name) {
                            if self.add_uses_edge(from_node, method_id) {
                                info!(
                                    "New edge added for Use from {} to {} (protocol via MRO: {})",
                                    self.nodes_arena[from_node].get_name(),
                                    self.nodes_arena[method_id].get_name(),
                                    method_name
                                );
                            }
                            break;
                        }
                    }
                }
            }
        }
    }

    fn analyze_comprehension(
        &mut self,
        generators: &[Comprehension],
        field1: Option<&Expr>,
        field2: Option<&Expr>,
        label: &str,
        line_index: &LineIndex,
    ) -> Option<NodeId> {
        if generators.is_empty() {
            return None;
        }

        let outermost = &generators[0];

        // Evaluate outermost iterator in current scope
        let iter_node = self.visit_expr(&outermost.iter, line_index);

        // Emit iterator protocol edges from the enclosing function scope.
        if let Some(obj_id) = iter_node
            && self.class_base_ast_info.contains_key(&obj_id)
        {
            let methods: &[&str] = if outermost.is_async {
                &["__aiter__", "__anext__"]
            } else {
                &["__iter__", "__next__"]
            };
            self.emit_protocol_edges(obj_id, methods);
        }

        // Ensure comprehension scope exists
        let parent_ns = {
            let parent_node = self.get_node_of_current_namespace();
            self.nodes_arena[parent_node].get_name()
        };
        let inner_ns = format!("{parent_ns}.{label}");
        if !self.scopes.contains_key(&inner_ns) {
            let mut target_names = HashSet::new();
            for comp in generators {
                collect_target_names_from_expr(&comp.target, &mut target_names);
            }
            self.scopes.insert(
                inner_ns.clone(),
                ScopeInfo::from_names(label, &target_names),
            );
        }

        // Enter inner scope
        self.name_stack.push(label.to_string());
        self.scope_stack.push(inner_ns.clone());
        self.context_stack.push(label.to_string());

        // Bind outermost targets
        self.analyze_binding_simple(&outermost.target, iter_node, line_index);
        for if_expr in &outermost.ifs {
            self.visit_expr(if_expr, line_index);
        }

        // Process remaining generators
        for comp in generators.iter().skip(1) {
            let val = self.visit_expr(&comp.iter, line_index);
            // Emit iterator protocol edges for each inner generator.
            if let Some(obj_id) = val
                && self.class_base_ast_info.contains_key(&obj_id)
            {
                let methods: &[&str] = if comp.is_async {
                    &["__aiter__", "__anext__"]
                } else {
                    &["__iter__", "__next__"]
                };
                self.emit_protocol_edges(obj_id, methods);
            }
            self.analyze_binding_simple(&comp.target, val, line_index);
            for if_expr in &comp.ifs {
                self.visit_expr(if_expr, line_index);
            }
        }

        // Visit output expression(s)
        if let Some(f1) = field1 {
            self.visit_expr(f1, line_index);
        }
        if let Some(f2) = field2 {
            self.visit_expr(f2, line_index);
        }

        // Exit inner scope
        self.context_stack.pop();
        self.scope_stack.pop();
        self.name_stack.pop();

        // Add defines edge
        let from_node = self.get_node_of_current_namespace();
        let from_name = self.nodes_arena[from_node].get_name();
        let to_node = self.get_node(Some(&from_name), label, Flavor::Namespace);
        self.add_defines_edge(from_node, Some(to_node));

        Some(to_node)
    }

    // =====================================================================
    // Binding helpers
    // =====================================================================

    /// Simple binding: bind a target expression to a resolved value.
    #[allow(clippy::only_used_in_recursion)]
    fn analyze_binding_simple(
        &mut self,
        target: &Expr,
        value: Option<NodeId>,
        line_index: &LineIndex,
    ) {
        match target {
            Expr::Tuple(t) => {
                for elt in &t.elts {
                    self.analyze_binding_simple(elt, value, line_index);
                }
            }
            Expr::List(l) => {
                for elt in &l.elts {
                    self.analyze_binding_simple(elt, value, line_index);
                }
            }
            Expr::Starred(s) => {
                self.analyze_binding_simple(&s.value, value, line_index);
            }
            _ => {
                // Visit the value expression side for uses edges
                self.bind_target_to_value(target, value);
            }
        }
    }

    /// Bind a single target to a value.
    fn bind_target_to_value(&mut self, target: &Expr, value: Option<NodeId>) {
        match target {
            Expr::Name(n) => {
                self.set_value(n.id.as_ref(), value);
            }
            Expr::Attribute(a) => {
                if let Some(value_id) = value {
                    self.set_attribute(a, Some(value_id));
                }
            }
            Expr::Tuple(t) => {
                for elt in &t.elts {
                    self.bind_target_to_value(elt, value);
                }
            }
            Expr::List(l) => {
                for elt in &l.elts {
                    self.bind_target_to_value(elt, value);
                }
            }
            Expr::Starred(s) => {
                self.bind_target_to_value(&s.value, value);
            }
            _ => {}
        }
    }

    fn bind_target_to_shallow_value(&mut self, target: &Expr, value: &ShallowValue) {
        match target {
            Expr::Name(n) => {
                self.set_value(n.id.as_ref(), value.first_value());
                self.set_containers(n.id.as_ref(), &value.containers);
                for id in value.values.iter().skip(1) {
                    self.set_value(n.id.as_ref(), Some(id));
                }
            }
            Expr::Attribute(a) => {
                self.set_attribute_shallow_value(a, value);
            }
            Expr::Tuple(t) => {
                for elt in &t.elts {
                    self.bind_target_to_shallow_value(elt, value);
                }
            }
            Expr::List(l) => {
                for elt in &l.elts {
                    self.bind_target_to_shallow_value(elt, value);
                }
            }
            Expr::Starred(s) => {
                self.bind_target_to_shallow_value(&s.value, value);
            }
            _ => {}
        }
    }

    // =====================================================================
    // Builtin resolution
    // =====================================================================

    /// Resolve a call expression to a known builtin result.
    ///
    /// Handles:
    /// - `super()` → resolve to parent class in MRO
    /// - `str(x)` / `repr(x)` → emit `__str__`/`__repr__` protocol edge on x's class;
    ///   returns None (str/repr produce strings, which aren't tracked nodes)
    fn resolve_builtins_from_call(
        &mut self,
        node: &ExprCall,
        _line_index: &LineIndex,
    ) -> Option<NodeId> {
        if let Expr::Name(ref func_name) = *node.func {
            let name = func_name.id.as_str();
            if name == "super" {
                return self.resolve_super();
            }
            if name == "str" || name == "repr" {
                // Emit __str__ / __repr__ protocol edge on the argument's class.
                let method: &'static str = if name == "str" { "__str__" } else { "__repr__" };
                if let Some(arg) = node.arguments.args.first() {
                    let obj_ids = self.get_obj_ids_for_expr(arg);
                    for obj_id in obj_ids {
                        if self.class_base_ast_info.contains_key(&obj_id) {
                            self.emit_protocol_edges(obj_id, &[method]);
                        }
                    }
                }
                return None; // str/repr return strings — not a tracked type node
            }
        }
        None
    }

    /// Resolve a call (used in attribute resolution).
    fn resolve_builtins(&mut self, node: &ExprCall) -> Option<NodeId> {
        if let Expr::Name(ref func_name) = *node.func {
            let name = func_name.id.as_str();
            if name == "super" {
                return self.resolve_super();
            }
        }
        None
    }

    fn resolve_super(&self) -> Option<NodeId> {
        let class_id = self.get_current_class()?;
        let mro = self.mro.get(&class_id)?;
        if mro.len() > 1 { Some(mro[1]) } else { None }
    }

    // =====================================================================
    // Base class resolution and MRO
    // =====================================================================

    fn resolve_base_classes(&mut self) {
        debug!("Resolving base classes");

        // Collect all class -> base refs data (need to clone due to borrow issues)
        let class_refs: Vec<(NodeId, Vec<BaseClassRef>)> = self
            .class_base_ast_info
            .iter()
            .map(|(&cls_id, refs)| (cls_id, refs.clone()))
            .collect();

        let mut class_base_nodes: HashMap<NodeId, Vec<NodeId>> = HashMap::new();

        for (cls_id, refs) in &class_refs {
            let mut bases = Vec::new();
            let cls_namespace = self.nodes_arena[*cls_id]
                .namespace
                .clone()
                .unwrap_or_default();

            for base_ref in refs {
                let base_id = match base_ref {
                    BaseClassRef::Name(name) => {
                        // Look up in enclosing scope
                        self.lookup_base_by_name(&cls_namespace, name)
                    }
                    BaseClassRef::Attribute(parts) => {
                        // Resolve attribute chain
                        self.lookup_base_by_attr_parts(parts)
                    }
                };

                if let Some(bid) = base_id
                    && self.nodes_arena[bid].namespace.is_some()
                {
                    bases.push(bid);
                }
            }

            class_base_nodes.insert(*cls_id, bases);
        }

        self.class_base_nodes = class_base_nodes;

        // Compute MRO
        debug!("Computing MRO for all analyzed classes");
        self.mro = resolve_mro(&self.class_base_nodes);
    }

    fn lookup_base_by_name(&self, enclosing_ns: &str, name: &str) -> Option<NodeId> {
        // Look up in enclosing scope
        if let Some(val) = self.lookup_in_scope(enclosing_ns, name) {
            return Some(val);
        }

        // Try module-level scope (walk up namespace hierarchy)
        let parts: Vec<&str> = enclosing_ns.split('.').collect();
        for i in (0..parts.len()).rev() {
            let ns = parts[..=i].join(".");
            if let Some(val) = self.lookup_in_scope(&ns, name) {
                return Some(val);
            }
        }

        None
    }

    fn lookup_base_by_attr_parts(&self, parts: &[String]) -> Option<NodeId> {
        if parts.is_empty() {
            return None;
        }

        // Start by looking up the first part
        let mut current = self.get_value(&parts[0])?;

        // Follow the chain
        for part in parts.iter().skip(1) {
            let ns = self.nodes_arena[current].get_name();
            if let Some(val) = self.lookup_in_scope(&ns, part.as_str()) {
                current = val;
                continue;
            }
            return None;
        }

        Some(current)
    }
}
