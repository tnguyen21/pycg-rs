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

use std::collections::{HashMap, HashSet};
use std::path::Path;

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
struct ScopeInfo {
    #[allow(dead_code)]
    name: String,
    defs: HashMap<String, ValueSet>,
    locals: HashSet<String>,
}

impl ScopeInfo {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            defs: HashMap::new(),
            locals: HashSet::new(),
        }
    }

    fn from_names(name: &str, identifiers: &HashSet<String>) -> Self {
        let defs = identifiers
            .iter()
            .map(|id| (id.clone(), ValueSet::empty()))
            .collect();
        let locals = identifiers.clone();
        Self {
            name: name.to_string(),
            defs,
            locals,
        }
    }
}

// ---------------------------------------------------------------------------
// Public call-graph struct
// ---------------------------------------------------------------------------

/// The primary output of the analyzer: a call graph over Python symbols.
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

    // Scope tracking (persistent across files/passes) -------------------
    scopes: HashMap<String, ScopeInfo>,

    // Class information -------------------------------------------------
    /// Pass 1: class NodeId -> list of base-class AST info (stored as
    /// (namespace, name) pairs extracted from AST nodes).
    class_base_ast_info: HashMap<NodeId, Vec<BaseClassRef>>,
    /// Pass 2: class NodeId -> resolved base NodeIds.
    class_base_nodes: HashMap<NodeId, Vec<NodeId>>,
    /// MRO for each class.
    mro: HashMap<NodeId, Vec<NodeId>>,

    // File mapping ------------------------------------------------------
    module_to_filename: HashMap<String, String>,
    filenames: Vec<String>,
    root: Option<String>,

    // Transient state (reset per file) ----------------------------------
    module_name: String,
    filename: String,
    name_stack: Vec<String>,
    scope_stack: Vec<String>, // keys into self.scopes
    class_stack: Vec<NodeId>,
    context_stack: Vec<String>,
}

/// Describes how a base class was referenced in the source.
#[derive(Debug, Clone)]
enum BaseClassRef {
    Name(String),
    Attribute(Vec<String>),
}

// =========================================================================
// Construction and high-level processing
// =========================================================================

impl CallGraph {
    /// Analyze a set of Python files and return the resulting call graph.
    pub fn new(filenames: &[String], root: Option<&str>) -> Result<Self> {
        let mut module_to_filename = HashMap::new();
        for filename in filenames {
            let mod_name = get_module_name(filename, root);
            module_to_filename.insert(mod_name, filename.clone());
        }

        let mut cg = Self {
            nodes_arena: Vec::new(),
            nodes_by_name: HashMap::new(),
            defines_edges: HashMap::new(),
            uses_edges: HashMap::new(),
            defined: HashSet::new(),
            scopes: HashMap::new(),
            class_base_ast_info: HashMap::new(),
            class_base_nodes: HashMap::new(),
            mro: HashMap::new(),
            module_to_filename,
            filenames: filenames.to_vec(),
            root: root.map(|s| s.to_string()),
            module_name: String::new(),
            filename: String::new(),
            name_stack: Vec::new(),
            scope_stack: Vec::new(),
            class_stack: Vec::new(),
            context_stack: Vec::new(),
        };

        cg.process()?;
        Ok(cg)
    }

    /// Two-pass analysis.
    fn process(&mut self) -> Result<()> {
        for pass_num in 0..2 {
            for filename in self.filenames.clone() {
                debug!(
                    "========== pass {}, file '{}' ==========",
                    pass_num + 1,
                    filename
                );
                self.process_one(&filename)?;
            }
            if pass_num == 0 {
                self.resolve_base_classes();
            }
        }
        self.postprocess();
        Ok(())
    }

    /// Analyze a single Python source file.
    fn process_one(&mut self, filename: &str) -> Result<()> {
        let content =
            std::fs::read_to_string(filename).with_context(|| format!("reading {filename}"))?;
        self.filename = filename.to_string();
        self.module_name = get_module_name(filename, self.root.as_deref());

        // Pre-pass: gather scope info from AST.
        self.analyze_scopes(&content);

        // Parse and visit.
        let parsed = ruff_python_parser::parse_unchecked(&content, ParseOptions::from(Mode::Module));
        let module = match parsed.syntax() {
            Mod::Module(m) => m,
            _ => return Ok(()),
        };

        let line_index = LineIndex::from_source_text(&content);
        self.visit_module(module, &line_index);

        self.module_name.clear();
        self.filename.clear();
        Ok(())
    }

    // =====================================================================
    // Scope analysis (pre-pass) — replaces Python's `symtable`
    // =====================================================================

    /// Gather scope information by walking the AST.
    fn analyze_scopes(&mut self, source: &str) {
        let parsed = ruff_python_parser::parse_unchecked(source, ParseOptions::from(Mode::Module));
        let module = match parsed.syntax() {
            Mod::Module(m) => m,
            _ => return,
        };

        let mut scopes: HashMap<String, ScopeInfo> = HashMap::new();
        let module_ns = self.module_name.clone();

        // Module-level scope
        let mut module_scope = ScopeInfo::new("");
        self.collect_scope_defs(&module.body, &mut module_scope);
        scopes.insert(module_ns.clone(), module_scope);

        // Nested scopes
        self.collect_nested_scopes(&module.body, &module_ns, &mut scopes);

        // Merge into existing scopes (union values rather than overwrite)
        for (ns, sc) in scopes {
            if let Some(existing) = self.scopes.get_mut(&ns) {
                for (name, vs) in sc.defs {
                    existing.defs.entry(name).or_default().union_with(&vs);
                }
            } else {
                self.scopes.insert(ns, sc);
            }
        }
    }

    /// Collect names defined (bound) at this scope level.
    fn collect_scope_defs(&self, stmts: &[Stmt], scope: &mut ScopeInfo) {
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
                        self.collect_assign_target_names(target, scope);
                    }
                }
                Stmt::AugAssign(a) => {
                    self.collect_assign_target_names(&a.target, scope);
                }
                Stmt::AnnAssign(a) => {
                    self.collect_assign_target_names(&a.target, scope);
                }
                Stmt::For(f) => {
                    self.collect_assign_target_names(&f.target, scope);
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
                    self.collect_scope_defs(&s.body, scope);
                    for clause in &s.elif_else_clauses {
                        self.collect_scope_defs(&clause.body, scope);
                    }
                }
                Stmt::While(s) => {
                    self.collect_scope_defs(&s.body, scope);
                    self.collect_scope_defs(&s.orelse, scope);
                }
                Stmt::With(s) => {
                    for item in &s.items {
                        if let Some(ref vars) = item.optional_vars {
                            self.collect_assign_target_names(vars, scope);
                        }
                    }
                    self.collect_scope_defs(&s.body, scope);
                }
                Stmt::Try(s) => {
                    self.collect_scope_defs(&s.body, scope);
                    for handler in &s.handlers {
                        let ExceptHandler::ExceptHandler(h) = handler;
                        if let Some(ref name) = h.name {
                            scope.defs.entry(name.id.to_string()).or_default();
                            scope.locals.insert(name.id.to_string());
                        }
                        self.collect_scope_defs(&h.body, scope);
                    }
                    self.collect_scope_defs(&s.orelse, scope);
                    self.collect_scope_defs(&s.finalbody, scope);
                }
                _ => {}
            }
        }
    }

    /// Recurse into function/class bodies to create child scopes.
    fn collect_nested_scopes(
        &self,
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

                    self.collect_scope_defs(&f.body, &mut scope);
                    scopes.insert(ns.clone(), scope);
                    self.collect_nested_scopes(&f.body, &ns, scopes);
                }
                Stmt::ClassDef(c) => {
                    let name = c.name.id.to_string();
                    let ns = format!("{parent_ns}.{name}");
                    let mut scope = ScopeInfo::new(&name);
                    self.collect_scope_defs(&c.body, &mut scope);
                    scopes.insert(ns.clone(), scope);
                    self.collect_nested_scopes(&c.body, &ns, scopes);
                }
                // Recurse into compound statements that don't create new
                // scopes (if/while/with/try/for).
                Stmt::If(s) => {
                    self.collect_nested_scopes(&s.body, parent_ns, scopes);
                    for clause in &s.elif_else_clauses {
                        self.collect_nested_scopes(&clause.body, parent_ns, scopes);
                    }
                }
                Stmt::While(s) => {
                    self.collect_nested_scopes(&s.body, parent_ns, scopes);
                    self.collect_nested_scopes(&s.orelse, parent_ns, scopes);
                }
                Stmt::For(s) => {
                    self.collect_nested_scopes(&s.body, parent_ns, scopes);
                    self.collect_nested_scopes(&s.orelse, parent_ns, scopes);
                }
                Stmt::With(s) => {
                    self.collect_nested_scopes(&s.body, parent_ns, scopes);
                }
                Stmt::Try(s) => {
                    self.collect_nested_scopes(&s.body, parent_ns, scopes);
                    for handler in &s.handlers {
                        let ExceptHandler::ExceptHandler(h) = handler;
                        self.collect_nested_scopes(&h.body, parent_ns, scopes);
                    }
                    self.collect_nested_scopes(&s.orelse, parent_ns, scopes);
                    self.collect_nested_scopes(&s.finalbody, parent_ns, scopes);
                }
                _ => {}
            }
        }
    }

    /// Extract names from an assignment target expression.
    fn collect_assign_target_names(&self, target: &Expr, scope: &mut ScopeInfo) {
        match target {
            Expr::Name(n) => {
                let name = n.id.to_string();
                scope.defs.entry(name.clone()).or_default();
                scope.locals.insert(name);
            }
            Expr::Tuple(t) => {
                for elt in &t.elts {
                    self.collect_assign_target_names(elt, scope);
                }
            }
            Expr::List(l) => {
                for elt in &l.elts {
                    self.collect_assign_target_names(elt, scope);
                }
            }
            Expr::Starred(s) => {
                self.collect_assign_target_names(&s.value, scope);
            }
            _ => {} // Attribute, Subscript — not local bindings
        }
    }

    // =====================================================================
    // Node creation and lookup
    // =====================================================================

    /// Get or create the unique node for (namespace, name).
    fn get_node(
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
        // Wildcard nodes (namespace=None) start as defined
        if namespace.is_none() {
            self.defined.insert(self.nodes_arena.len());
        }

        let id = self.nodes_arena.len();
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
        let name = self.name_stack.last().unwrap().clone();
        self.get_node(Some(&namespace), &name, Flavor::Namespace)
    }

    /// Get the parent node of the given node (by splitting its namespace).
    fn get_parent_node(&mut self, node_id: NodeId) -> NodeId {
        let node = &self.nodes_arena[node_id];
        let (ns, name) = if let Some(ref namespace) = node.namespace {
            if namespace.contains('.') {
                let (parent_ns, parent_name) = namespace.rsplit_once('.').unwrap();
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

    fn add_defines_edge(&mut self, from_id: NodeId, to_id: Option<NodeId>) -> bool {
        self.defined.insert(from_id);
        let entry = self.defines_edges.entry(from_id).or_default();
        if let Some(to) = to_id {
            self.defined.insert(to);
            entry.insert(to)
        } else {
            false
        }
    }

    fn add_uses_edge(&mut self, from_id: NodeId, to_id: NodeId) -> bool {
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

    fn remove_uses_edge(&mut self, from_id: NodeId, to_id: NodeId) {
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
                let scope = self.scopes.get_mut(scope_key).unwrap();
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

    /// Check if a name is a local in the current (innermost) scope.
    fn is_local(&self, name: &str) -> bool {
        if let Some(scope_key) = self.scope_stack.last()
            && let Some(scope) = self.scopes.get(scope_key) {
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
                    vec![id]
                } else {
                    vec![]
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
    fn lookup_values_in_scope(&self, ns: &str, name: &str) -> ValueSet {
        if let Some(scope) = self.scopes.get(ns) {
            if let Some(vs) = scope.defs.get(name) {
                return vs.clone();
            }
        }
        ValueSet::empty()
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
                    self.visit_expr(value, line_index);
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
            Stmt::Global(_) | Stmt::Nonlocal(_) | Stmt::Pass(_)
            | Stmt::Break(_) | Stmt::Continue(_) | Stmt::IpyEscapeCommand(_) => {}
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
        let (self_name, flavor) = self.analyze_function_def(node, line_index);

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
                scope.defs.entry(sname.clone()).or_default().insert(class_id);
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
            && let Some(ref annotation) = va.annotation {
                self.visit_expr(annotation, line_index);
            }
        if let Some(ref kw) = node.parameters.kwarg
            && let Some(ref annotation) = kw.annotation {
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
    ) -> (Option<String>, Flavor) {
        // Visit decorators
        let mut deco_names = Vec::new();
        for deco in &node.decorator_list {
            let deco_node = self.visit_expr(&deco.expression, line_index);
            if let Some(did) = deco_node {
                deco_names.push(self.nodes_arena[did].name.clone());
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

        (self_name, flavor)
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
                self.bind_target_to_value(&Expr::Name(ExprName {
                    node_index: AtomicNodeIndex::default(),
                    range: arg.parameter.name.range(),
                    id: arg.parameter.name.id.clone(),
                    ctx: ExprContext::Store,
                }), val);
            }
        }

        // Keyword-only args with defaults
        for arg in &params.kwonlyargs {
            if let Some(ref default) = arg.default {
                let val = self.visit_expr(default, line_index);
                self.bind_target_to_value(&Expr::Name(ExprName {
                    node_index: AtomicNodeIndex::default(),
                    range: arg.parameter.name.range(),
                    id: arg.parameter.name.id.clone(),
                    ctx: ExprContext::Store,
                }), val);
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

        for alias in &node.names {
            let item_name = alias.name.id.to_string();

            // Check if import is a module itself
            let full_name = format!("{tgt_name}.{item_name}");
            let to_node = if self.module_to_filename.contains_key(&full_name) {
                self.get_node(Some(""), &full_name, Flavor::Module)
            } else {
                self.get_node(Some(&tgt_name), &item_name, Flavor::ImportedItem)
            };

            let alias_name = if let Some(ref asname) = alias.asname {
                asname.id.to_string()
            } else {
                item_name.clone()
            };

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

    fn visit_assign(&mut self, node: &StmtAssign, line_index: &LineIndex) {
        let value_node = self.visit_expr(&node.value, line_index);
        for target in &node.targets {
            self.analyze_binding_simple(target, value_node, line_index);
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
            self.visit_expr(target, line_index);
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
                let val = self.visit_expr(&node.value, line_index);
                self.visit_expr(&node.slice, line_index);
                val
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

    fn visit_attribute(
        &mut self,
        node: &ExprAttribute,
        line_index: &LineIndex,
    ) -> Option<NodeId> {
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
                        let attr_node =
                            self.get_node(Some(&ns), &attr_name, Flavor::Attribute);
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

        // If calling a known class, add __init__ edge
        if let Some(func_id) = func_node
            && self.class_base_ast_info.contains_key(&func_id) {
                let from_node = self.get_node_of_current_namespace();
                let func_name = self.nodes_arena[func_id].get_name();
                let init_node =
                    self.get_node(Some(&func_name), "__init__", Flavor::Method);
                if self.add_uses_edge(from_node, init_node) {
                    info!(
                        "New edge added for Use from {} to {} (class instantiation)",
                        self.nodes_arena[from_node].get_name(),
                        self.nodes_arena[init_node].get_name()
                    );
                }
            }

        func_node
    }

    fn visit_lambda(
        &mut self,
        node: &ExprLambda,
        line_index: &LineIndex,
    ) -> Option<NodeId> {
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
            self.scopes.insert(inner_ns.clone(), ScopeInfo::from_names(label, &names));
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

    fn visit_list_comp(
        &mut self,
        node: &ExprListComp,
        line_index: &LineIndex,
    ) -> Option<NodeId> {
        self.analyze_comprehension(&node.generators, Some(&node.elt), None, "listcomp", line_index)
    }

    fn visit_set_comp(
        &mut self,
        node: &ExprSetComp,
        line_index: &LineIndex,
    ) -> Option<NodeId> {
        self.analyze_comprehension(&node.generators, Some(&node.elt), None, "setcomp", line_index)
    }

    fn visit_dict_comp(
        &mut self,
        node: &ExprDictComp,
        line_index: &LineIndex,
    ) -> Option<NodeId> {
        self.analyze_comprehension(&node.generators, Some(&node.key), Some(&node.value), "dictcomp", line_index)
    }

    fn visit_generator(
        &mut self,
        node: &ExprGenerator,
        line_index: &LineIndex,
    ) -> Option<NodeId> {
        self.analyze_comprehension(&node.generators, Some(&node.elt), None, "genexpr", line_index)
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
            self.scopes.insert(inner_ns.clone(), ScopeInfo::from_names(label, &target_names));
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

    // =====================================================================
    // Builtin resolution
    // =====================================================================

    /// Resolve a call expression to a known builtin result.
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
        if mro.len() > 1 {
            Some(mro[1])
        } else {
            None
        }
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
                    && self.nodes_arena[bid].namespace.is_some() {
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

    // =====================================================================
    // Postprocessing
    // =====================================================================

    fn postprocess(&mut self) {
        self.expand_unknowns();
        self.resolve_imports();
        self.contract_nonexistents();
        self.cull_inherited();
        self.collapse_inner();
    }

    /// For each unknown node `*.name`, replace all its incoming edges
    /// with edges to `X.name` for all possible Xs.
    fn expand_unknowns(&mut self) {
        // Collect new defines edges
        let mut new_defines: Vec<(NodeId, NodeId)> = Vec::new();
        for (&from, targets) in &self.defines_edges {
            for &to in targets {
                if self.nodes_arena[to].namespace.is_none() {
                    let name = self.nodes_arena[to].name.clone();
                    if let Some(ids) = self.nodes_by_name.get(&name) {
                        for &candidate in ids {
                            if self.nodes_arena[candidate].namespace.is_some() {
                                new_defines.push((from, candidate));
                            }
                        }
                    }
                }
            }
        }
        for (from, to) in new_defines {
            self.add_defines_edge(from, Some(to));
        }

        // Collect new uses edges
        let mut new_uses: Vec<(NodeId, NodeId)> = Vec::new();
        for (&from, targets) in &self.uses_edges {
            for &to in targets {
                if self.nodes_arena[to].namespace.is_none() {
                    let name = self.nodes_arena[to].name.clone();
                    if let Some(ids) = self.nodes_by_name.get(&name) {
                        for &candidate in ids {
                            if self.nodes_arena[candidate].namespace.is_some() {
                                new_uses.push((from, candidate));
                            }
                        }
                    }
                }
            }
        }
        for (from, to) in new_uses {
            self.add_uses_edge(from, to);
        }

        // Mark all unknown nodes as not defined
        for ids in self.nodes_by_name.values() {
            for &id in ids {
                if self.nodes_arena[id].namespace.is_none() {
                    self.defined.remove(&id);
                }
            }
        }
    }

    /// Resolve import edges: follow import chains to their definitions.
    fn resolve_imports(&mut self) {
        // Find all imported item nodes
        let import_nodes: Vec<NodeId> = self
            .nodes_by_name
            .values()
            .flat_map(|ids| ids.iter())
            .copied()
            .filter(|&id| self.nodes_arena[id].flavor == Flavor::ImportedItem)
            .collect();

        let mut import_mapping: HashMap<NodeId, NodeId> = HashMap::new();
        let mut to_resolve: Vec<NodeId> = import_nodes;

        while let Some(from_id) = to_resolve.pop() {
            if import_mapping.contains_key(&from_id) {
                continue;
            }

            // Check what from_id uses
            let to_id = if let Some(targets) = self.uses_edges.get(&from_id) {
                if targets.len() == 1 {
                    *targets.iter().next().unwrap()
                } else {
                    continue;
                }
            } else {
                continue;
            };

            // Resolve namespace
            let module_id = if self.nodes_arena[to_id].namespace.as_deref() == Some("") {
                to_id
            } else {
                let ns = self.nodes_arena[to_id]
                    .namespace
                    .clone()
                    .unwrap_or_default();
                self.get_node(Some(""), &ns, Flavor::Namespace)
            };

            if let Some(module_uses) = self.uses_edges.get(&module_id).cloned() {
                let from_name = self.nodes_arena[from_id].name.clone();
                for candidate in &module_uses {
                    if self.nodes_arena[*candidate].name == from_name {
                        import_mapping.insert(from_id, *candidate);
                        if self.nodes_arena[*candidate].flavor == Flavor::ImportedItem
                            && *candidate != from_id
                        {
                            to_resolve.push(*candidate);
                        }
                        break;
                    }
                }
            }
        }

        // Apply mapping to edges
        if !import_mapping.is_empty() {
            let remap = |id: NodeId| -> NodeId { *import_mapping.get(&id).unwrap_or(&id) };

            // Remap uses_edges
            let old_uses: Vec<(NodeId, HashSet<NodeId>)> =
                self.uses_edges.drain().collect();
            for (from, targets) in old_uses {
                if targets.is_empty() {
                    continue;
                }
                let new_from = remap(from);
                let entry = self.uses_edges.entry(new_from).or_default();
                for to in targets {
                    entry.insert(remap(to));
                }
            }

            // Remap defines_edges
            let old_defines: Vec<(NodeId, HashSet<NodeId>)> =
                self.defines_edges.drain().collect();
            for (from, targets) in old_defines {
                if targets.is_empty() {
                    continue;
                }
                let new_from = remap(from);
                let entry = self.defines_edges.entry(new_from).or_default();
                for to in targets {
                    entry.insert(remap(to));
                }
            }

            // Remap nodes_by_name
            for ids in self.nodes_by_name.values_mut() {
                for id in ids.iter_mut() {
                    if let Some(&mapped) = import_mapping.get(id) {
                        *id = mapped;
                    }
                }
            }
        }
    }

    /// For all use edges to non-existent nodes X.name, replace with *.name.
    fn contract_nonexistents(&mut self) {
        // First pass: collect edges that need changes (from, to, wildcard_name)
        let mut to_contract: Vec<(NodeId, NodeId, String)> = Vec::new();

        for (&from, targets) in &self.uses_edges {
            for &to in targets {
                if self.nodes_arena[to].namespace.is_some() && !self.defined.contains(&to) {
                    let name = self.nodes_arena[to].name.clone();
                    to_contract.push((from, to, name));
                }
            }
        }

        // Second pass: create wildcard nodes and update edges
        for (from, to, name) in to_contract {
            let wild_id = self.get_node(None, &name, Flavor::Unknown);
            self.defined.remove(&wild_id);
            self.add_uses_edge(from, wild_id);
            self.remove_uses_edge(from, to);
        }
    }

    /// Remove inherited edges: if W->X.name and W->Y.name where Y inherits
    /// from X, remove the edge to X.name.
    fn cull_inherited(&mut self) {
        let mut removed: Vec<(NodeId, NodeId)> = Vec::new();

        let uses_snapshot: Vec<(NodeId, HashSet<NodeId>)> = self
            .uses_edges
            .iter()
            .map(|(&k, v)| (k, v.clone()))
            .collect();

        for (from, targets) in &uses_snapshot {
            for &to in targets {
                let mut inherited = false;
                for &other in targets {
                    if other == to {
                        continue;
                    }
                    let to_name = &self.nodes_arena[to].name;
                    let other_name = &self.nodes_arena[other].name;
                    let to_ns = &self.nodes_arena[to].namespace;
                    let other_ns = &self.nodes_arena[other].namespace;

                    if to_name == other_name
                        && to_ns.is_some()
                        && other_ns.is_some()
                        && to_ns != other_ns
                    {
                        let parent_to = self.get_parent_node(to);
                        let parent_other = self.get_parent_node(other);
                        if let Some(parent_to_uses) = self.uses_edges.get(&parent_to)
                            && parent_to_uses.contains(&parent_other) {
                                inherited = true;
                                break;
                            }
                    }
                }
                if inherited {
                    removed.push((*from, to));
                }
            }
        }

        for (from, to) in removed {
            self.remove_uses_edge(from, to);
        }
    }

    /// Collapse lambda and comprehension nodes into their parents.
    fn collapse_inner(&mut self) {
        let inner_labels = ["lambda", "listcomp", "setcomp", "dictcomp", "genexpr"];

        for label in &inner_labels {
            if let Some(ids) = self.nodes_by_name.get(*label).cloned() {
                for id in ids {
                    let parent_id = self.get_parent_node(id);

                    // Move uses edges from inner to parent
                    if let Some(inner_uses) = self.uses_edges.get(&id).cloned() {
                        for target in inner_uses {
                            self.add_uses_edge(parent_id, target);
                        }
                    }

                    // Mark as not defined
                    self.defined.remove(&id);
                }
            }
        }
    }
}

// =========================================================================
// Free functions
// =========================================================================

/// Extract a human-readable name from an expression (for debugging / attr
/// chain resolution).
fn get_ast_node_name(expr: &Expr) -> String {
    match expr {
        Expr::Name(n) => n.id.to_string(),
        Expr::Attribute(a) => {
            format!("{}.{}", get_ast_node_name(&a.value), a.attr.id)
        }
        _ => String::new(),
    }
}

/// Collect all Name identifiers from an assignment target expression.
fn collect_target_names_from_expr(target: &Expr, names: &mut HashSet<String>) {
    match target {
        Expr::Name(n) => {
            names.insert(n.id.to_string());
        }
        Expr::Tuple(t) => {
            for elt in &t.elts {
                collect_target_names_from_expr(elt, names);
            }
        }
        Expr::List(l) => {
            for elt in &l.elts {
                collect_target_names_from_expr(elt, names);
            }
        }
        Expr::Starred(s) => {
            collect_target_names_from_expr(&s.value, names);
        }
        _ => {}
    }
}

/// Convert a Python source filename to a dotted module name.
///
/// If `root` is `None`, walks up directories checking for `__init__.py` to
/// find the package root.
pub fn get_module_name(filename: &str, root: Option<&str>) -> String {
    let path = Path::new(filename);

    // Determine the module path (without .py extension)
    let module_path_buf;
    let module_path: &Path = if path.file_name().is_some_and(|f| f == "__init__.py") {
        path.parent().unwrap_or(path)
    } else {
        module_path_buf = path.with_extension("");
        &module_path_buf
    };

    if let Some(root_dir) = root {
        // Root is known -- just strip it and join with dots
        let root_path = Path::new(root_dir);
        if let Ok(relative) = module_path.strip_prefix(root_path) {
            return relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join(".");
        }
    }

    // Walk up directories checking for __init__.py
    let mut directories: Vec<(std::path::PathBuf, bool)> =
        vec![(module_path.to_path_buf(), true)];

    let mut current = module_path.parent();
    while let Some(dir) = current {
        if dir == Path::new("") || dir == Path::new("/") {
            break;
        }
        let has_init = dir.join("__init__.py").exists();
        directories.insert(0, (dir.to_path_buf(), has_init));
        if !has_init {
            break;
        }
        current = dir.parent();
    }

    // Keep only from the first directory that is a package root
    while directories.len() > 1 && !directories[0].1 {
        directories.remove(0);
    }

    directories
        .iter()
        .map(|(p, _)| {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// Compute the method resolution order (MRO) using C3 linearization.
fn resolve_mro(class_base_nodes: &HashMap<NodeId, Vec<NodeId>>) -> HashMap<NodeId, Vec<NodeId>> {
    fn head(lst: &[NodeId]) -> Option<NodeId> {
        lst.first().copied()
    }

    fn tail(lst: &[NodeId]) -> Vec<NodeId> {
        if lst.len() > 1 {
            lst[1..].to_vec()
        } else {
            Vec::new()
        }
    }

    fn c3_find_good_head(heads: &[NodeId], tails: &[Vec<NodeId>]) -> Option<NodeId> {
        let flat_tails: Vec<NodeId> = tails.iter().flat_map(|t| t.iter().copied()).collect();
        heads.iter().find(|&&hd| !flat_tails.contains(&hd)).copied() // Cyclic dependency
    }

    fn c3_merge(lists: &mut [Vec<NodeId>]) -> Vec<NodeId> {
        let mut out = Vec::new();
        loop {
            let heads: Vec<NodeId> = lists
                .iter()
                .filter_map(|l| head(l))
                .collect();
            if heads.is_empty() {
                break;
            }
            let tails: Vec<Vec<NodeId>> = lists.iter().map(|l| tail(l)).collect();
            if let Some(hd) = c3_find_good_head(&heads, &tails) {
                out.push(hd);
                for list in lists.iter_mut() {
                    list.retain(|&x| x != hd);
                }
            } else {
                break; // Cyclic — give up
            }
        }
        out
    }

    let mut mro = HashMap::new();
    let mut memo: HashMap<NodeId, Vec<NodeId>> = HashMap::new();

    fn c3_linearize(
        node: NodeId,
        class_base_nodes: &HashMap<NodeId, Vec<NodeId>>,
        memo: &mut HashMap<NodeId, Vec<NodeId>>,
        seen: &mut HashSet<NodeId>,
    ) -> Vec<NodeId> {
        seen.insert(node);
        if let Some(cached) = memo.get(&node) {
            return cached.clone();
        }

        let result = if !class_base_nodes.contains_key(&node)
            || class_base_nodes[&node].is_empty()
        {
            vec![node]
        } else {
            let mut lists = Vec::new();
            for &base in &class_base_nodes[&node] {
                if !seen.contains(&base) {
                    lists.push(c3_linearize(base, class_base_nodes, memo, seen));
                }
            }
            lists.push(class_base_nodes[&node].clone());
            let mut result = vec![node];
            result.extend(c3_merge(&mut lists));
            result
        };

        memo.insert(node, result.clone());
        result
    }

    for &cls in class_base_nodes.keys() {
        let mut seen = HashSet::new();
        let lin = c3_linearize(cls, class_base_nodes, &mut memo, &mut seen);
        mro.insert(cls, lin);
    }

    mro
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_module_name_simple() {
        // Without package structure, just the filename stem
        let name = get_module_name("foo.py", None);
        assert!(name.ends_with("foo"), "got: {name}");
    }

    #[test]
    fn test_get_module_name_init() {
        // __init__.py should use directory name
        let name = get_module_name("pkg/__init__.py", Some(""));
        assert!(name.ends_with("pkg"), "got: {name}");
    }

    #[test]
    fn test_resolve_mro_simple() {
        // A -> B -> C (linear chain)
        let mut bases = HashMap::new();
        bases.insert(0, vec![1]); // A inherits from B
        bases.insert(1, vec![2]); // B inherits from C
        bases.insert(2, vec![]); // C has no bases

        let mro = resolve_mro(&bases);
        assert_eq!(mro[&0], vec![0, 1, 2]);
        assert_eq!(mro[&1], vec![1, 2]);
        assert_eq!(mro[&2], vec![2]);
    }

    #[test]
    fn test_resolve_mro_diamond() {
        // D inherits from B, C; both B and C inherit from A
        let mut bases = HashMap::new();
        bases.insert(3, vec![1, 2]); // D -> B, C
        bases.insert(1, vec![0]); // B -> A
        bases.insert(2, vec![0]); // C -> A
        bases.insert(0, vec![]); // A

        let mro = resolve_mro(&bases);
        assert_eq!(mro[&3], vec![3, 1, 2, 0]);
    }

    #[test]
    fn test_get_ast_node_name() {
        // Just a basic smoke test — the real tests happen via integration.
        assert_eq!(get_ast_node_name(&Expr::Name(ExprName {
            node_index: AtomicNodeIndex::default(),
            range: ruff_text_size::TextRange::default(),
            id: "foo".into(),
            ctx: ExprContext::Load,
        })), "foo");
    }
}
