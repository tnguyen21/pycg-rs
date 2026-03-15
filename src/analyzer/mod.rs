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
mod pipeline;
mod postprocess;
mod prepass;
mod resolution;
mod state;
mod util;

use mro::resolve_mro;
pub use util::get_module_name;
use util::{collect_target_names_from_expr, get_ast_node_name, literal_key_from_expr};

use crate::{FxHashMap, FxHashSet};
use std::ops::{Deref, DerefMut};

use anyhow::{Context, Result};
use log::{debug, info};
use ruff_python_ast::*;
use ruff_python_parser::{self, Mode, ParseOptions};
use ruff_source_file::LineIndex;
use ruff_text_size::Ranged;

use crate::intern::{Interner, SymId};
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
    defs: FxHashMap<SymId, ValueSet>,
    /// Shallow container facts for locally-bound names/attributes.
    ///
    /// This tracks statically-known list/tuple/dict literal contents so that
    /// later `x[i]` / `x["k"]` expressions can resolve through the retrieved
    /// value instead of collapsing back to the container object.
    containers: FxHashMap<SymId, ContainerFacts>,
    locals: FxHashSet<SymId>,
    /// Statically-known `__all__` exports for this module scope.
    ///
    /// `Some(names)` when the module contains a top-level `__all__ = [...]`
    /// whose elements are all string literals; `None` otherwise (either
    /// because `__all__` is absent or because it is not statically analyzable).
    ///
    /// When `Some`, `handle_star_import` uses this as the definitive filter
    /// for `from mod import *`, allowing private names that are explicitly
    /// listed and excluding public names that are not.
    all_exports: Option<FxHashSet<SymId>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExternalReferenceKind {
    Import,
    Module,
}

impl ExternalReferenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Import => "import",
            Self::Module => "module",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExternalReferenceDiagnostic {
    pub source_canonical_name: String,
    pub source_filename: Option<SymId>,
    pub source_line: Option<usize>,
    pub kind: ExternalReferenceKind,
    pub canonical_name: String,
}

#[derive(Debug, Clone, Default)]
pub struct AnalysisDiagnostics {
    pub external_references: Vec<ExternalReferenceDiagnostic>,
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
    Mapping(FxHashMap<LiteralKey, ShallowValue>),
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
    fn new() -> Self {
        Self {
            defs: FxHashMap::default(),
            containers: FxHashMap::default(),
            locals: FxHashSet::default(),
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
fn extract_all_exports(expr: &Expr, interner: &mut Interner) -> Option<FxHashSet<SymId>> {
    let elts = match expr {
        Expr::List(l) => &l.elts,
        Expr::Tuple(t) => &t.elts,
        _ => return None,
    };
    let mut names = FxHashSet::default();
    for elt in elts {
        if let Expr::StringLiteral(s) = elt {
            names.insert(interner.intern(s.value.to_str()));
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
    // String interner ---------------------------------------------------
    pub interner: Interner,

    // Node arena --------------------------------------------------------
    pub nodes_arena: Vec<Node>,
    /// Short name -> list of node IDs (there may be several in different
    /// namespaces).
    pub nodes_by_name: FxHashMap<SymId, Vec<NodeId>>,

    // Edges -------------------------------------------------------------
    pub defines_edges: FxHashMap<NodeId, FxHashSet<NodeId>>,
    pub uses_edges: FxHashMap<NodeId, FxHashSet<NodeId>>,

    /// Which nodes have been marked *defined* (have a defines edge from
    /// them, or were created as wildcard nodes).
    pub defined: FxHashSet<NodeId>,

    /// Analyzer-owned diagnostics that survive graph postprocessing.
    pub diagnostics: AnalysisDiagnostics,

    // File mapping ------------------------------------------------------
    pub(super) module_to_filename: FxHashMap<SymId, SymId>,
}

/// Internal mutable analysis session.
///
/// This owns the work-in-progress state needed to build a [`CallGraph`], but
/// that transient state does not leak into the public result type.
#[derive(Debug)]
pub(super) struct AnalysisSession {
    pub(super) graph: CallGraph,
    node_ids_by_key: FxHashMap<NodeKey, NodeId>,
    /// Index: (from_id, name_sym) -> wild_node_id for O(1) wildcard lookup.
    wild_edge_index: FxHashMap<(NodeId, SymId), NodeId>,

    // Scope tracking (persistent across files/passes) -------------------
    pub(super) scopes: FxHashMap<SymId, ScopeInfo>,

    // Class information -------------------------------------------------
    /// Pass 1: class NodeId -> list of base-class AST info (stored as
    /// (namespace, name) pairs extracted from AST nodes).
    pub(super) class_base_ast_info: FxHashMap<NodeId, Vec<BaseClassRef>>,
    /// Pass 2: class NodeId -> resolved base NodeIds.
    pub(super) class_base_nodes: FxHashMap<NodeId, Vec<NodeId>>,
    /// MRO for each class.
    pub(super) mro: FxHashMap<NodeId, Vec<NodeId>>,

    /// Collected return values per function node, used for return-value propagation.
    /// Maps function/method NodeId -> set of NodeIds that the function may return.
    /// Populated during `visit_stmt(Return)` and consumed in `visit_call`.
    pub(super) function_returns: FxHashMap<NodeId, FxHashSet<NodeId>>,
    /// Set during a propagation pass when a function gains a newly-discovered
    /// return value. Used to detect fixpoint convergence without cloning the
    /// entire `function_returns` map every pass.
    function_returns_changed: bool,

    pub(super) filenames: Vec<String>,
    pub(super) root: Option<String>,

    // Transient state (reset per file) ----------------------------------
    pub(super) module_name: SymId,
    pub(super) filename: SymId,
    pub(super) name_stack: Vec<SymId>,
    /// Cached FQN for each name_stack depth. `fqn_cache[i]` is the interned
    /// join of `name_stack[0..=i]` with `"."`. Avoids re-joining and
    /// re-interning on every `get_node_of_current_namespace` call.
    pub(super) fqn_cache: Vec<SymId>,
    pub(super) scope_stack: Vec<SymId>, // keys into self.scopes
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
    filename_sym: SymId,
    module_name: SymId,
    module: ModModule,
    line_index: LineIndex,
    scopes: FxHashMap<SymId, ScopeInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct NodeKey {
    namespace: Option<SymId>,
    name: SymId,
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

impl AnalysisSession {
    // =====================================================================
    // Visitor methods
    // =====================================================================

    fn visit_module(&mut self, module: &ModModule, line_index: &LineIndex) {
        let mod_name_str = self.graph.interner.resolve(self.module_name).to_owned();
        let filename_str = self.graph.interner.resolve(self.filename).to_owned();
        debug!("Module {}, {}", mod_name_str, filename_str);

        let fname = self.filename;
        let module_node = self.get_node(Some(""), &mod_name_str, Flavor::Module);
        let line = line_index.line_index(module.range().start()).get();
        self.associate_node(module_node, fname, line);

        let ns_sym = self.module_name;
        self.push_name(ns_sym);
        self.scope_stack.push(ns_sym);
        self.context_stack.push(format!("Module {mod_name_str}"));

        for stmt in &module.body {
            self.visit_stmt(stmt, line_index);
        }

        self.context_stack.pop();
        self.scope_stack.pop();
        self.pop_name();

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
                    let sentinel = self.graph.interner.intern("^^^argument^^^");
                    for ret_id in ret_ids {
                        let is_sentinel = self.nodes_arena[ret_id].name == sentinel;
                        let is_unknown = self.nodes_arena[ret_id].namespace.is_none();
                        if !is_sentinel && !is_unknown {
                            let fn_node = self.get_node_of_current_namespace();
                            self.record_function_return(fn_node, ret_id);
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
            self.graph.interner.resolve(self.filename),
            line_index.line_index(node.range().start()).get()
        );

        let from_node = self.get_node_of_current_namespace();
        let ns = self.nodes_arena[from_node].get_name(&self.graph.interner).to_owned();
        let to_node = self.get_node(Some(&ns), &class_name, Flavor::Class);
        if self.add_defines_edge(from_node, Some(to_node)) {
            info!(
                "Def from {} to Class {}",
                self.nodes_arena[from_node].get_name(&self.graph.interner),
                self.nodes_arena[to_node].get_name(&self.graph.interner)
            );
        }

        let line = line_index.line_index(node.range().start()).get();
        self.associate_node(to_node, self.filename, line);
        self.set_value(&class_name, Some(to_node));

        self.class_stack.push(to_node);
        let class_sym = self.graph.interner.intern(&class_name);
        self.push_name(class_sym);
        let _inner_node = self.get_node_of_current_namespace(); // ensure node exists
        let inner_ns_sym = *self.fqn_cache.last().unwrap();
        self.scope_stack.push(inner_ns_sym);
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
        self.pop_name();
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
            self.graph.interner.resolve(self.filename),
            line_index.line_index(node.range().start()).get()
        );

        // Analyze decorators and determine flavor
        let (self_name, flavor, deco_ids) = self.analyze_function_def(node, line_index);

        let from_node = self.get_node_of_current_namespace();
        let ns = self.nodes_arena[from_node].get_name(&self.graph.interner).to_owned();
        let to_node = self.get_node(Some(&ns), &func_name, flavor);
        if self.add_defines_edge(from_node, Some(to_node)) {
            info!(
                "Def from {} to Function {}",
                self.nodes_arena[from_node].get_name(&self.graph.interner),
                self.nodes_arena[to_node].get_name(&self.graph.interner)
            );
        }

        let line = line_index.line_index(node.range().start()).get();
        self.associate_node(to_node, self.filename, line);
        self.set_value(&func_name, Some(to_node));

        // Decorator-chain call flow: each concrete (non-wildcard) decorator receives
        // the function as its argument, so emit decorator -> function uses edges.
        for &deco_id in &deco_ids {
            if self.nodes_arena[deco_id].namespace.is_some() && self.add_uses_edge(deco_id, to_node)
            {
                info!(
                    "New edge added: decorator {} uses function {}",
                    self.nodes_arena[deco_id].get_name(&self.graph.interner),
                    self.nodes_arena[to_node].get_name(&self.graph.interner)
                );
            }
        }

        // Enter function scope
        let func_sym = self.graph.interner.intern(&func_name);
        self.push_name(func_sym);
        let _inner_node = self.get_node_of_current_namespace(); // ensure node exists
        let inner_ns_sym = *self.fqn_cache.last().unwrap();
        self.scope_stack.push(inner_ns_sym);
        self.context_stack.push(format!("FunctionDef {func_name}"));

        // Capture arg names as nonsense nodes
        let inner_ns = self.graph.interner.resolve(inner_ns_sym).to_owned();
        self.generate_args_nodes(&node.parameters, &inner_ns);

        // Bind self_name to current class (additive insert into ValueSet)
        if let Some(ref sname) = self_name
            && let Some(class_id) = self.get_current_class()
        {
            let sname_sym = self.graph.interner.intern(sname);
            if let Some(scope) = self.scopes.get_mut(&inner_ns_sym) {
                scope.defs.entry(sname_sym).or_default().insert(class_id);
            }
            info!(
                "Method def: setting self name \"{}\" to {}",
                sname,
                self.nodes_arena[class_id].get_name(&self.graph.interner)
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
        self.pop_name();
    }

    fn analyze_function_def(
        &mut self,
        node: &StmtFunctionDef,
        line_index: &LineIndex,
    ) -> (Option<String>, Flavor, Vec<NodeId>) {
        // Visit decorators; collect resolved node IDs for decorator-chain flow.
        let mut deco_name_syms = Vec::new();
        let mut deco_ids: Vec<NodeId> = Vec::new();
        for deco in &node.decorator_list {
            let deco_node = self.visit_expr(&deco.expression, line_index);
            if let Some(did) = deco_node {
                deco_name_syms.push(self.nodes_arena[did].name);
                deco_ids.push(did);
            }
        }

        // Determine flavor
        let in_class_ns = self
            .context_stack
            .last()
            .is_some_and(|c| c.starts_with("ClassDef"));

        let staticmethod_sym = self.graph.interner.intern("staticmethod");
        let classmethod_sym = self.graph.interner.intern("classmethod");
        let flavor = if !in_class_ns {
            Flavor::Function
        } else if deco_name_syms.iter().any(|&n| n == staticmethod_sym) {
            Flavor::StaticMethod
        } else if deco_name_syms.iter().any(|&n| n == classmethod_sym) {
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
        let inner_ns_sym = self.graph.interner.intern(inner_ns);
        // Collect all param syms first to avoid borrow issues
        let mut param_syms: Vec<SymId> = Vec::new();
        for a in &params.args {
            param_syms.push(self.graph.interner.intern(a.parameter.name.id.as_str()));
        }
        for a in &params.posonlyargs {
            param_syms.push(self.graph.interner.intern(a.parameter.name.id.as_str()));
        }
        if let Some(ref va) = params.vararg {
            param_syms.push(self.graph.interner.intern(va.name.id.as_str()));
        }
        for a in &params.kwonlyargs {
            param_syms.push(self.graph.interner.intern(a.parameter.name.id.as_str()));
        }
        if let Some(ref kw) = params.kwarg {
            param_syms.push(self.graph.interner.intern(kw.name.id.as_str()));
        }
        if let Some(scope) = self.scopes.get_mut(&inner_ns_sym) {
            for sym in param_syms {
                scope.defs.entry(sym).or_default().insert(nonsense_node);
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
            self.graph.interner.resolve(self.filename),
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
            self.graph.interner.resolve(self.filename),
            line_index.line_index(node.range().start()).get()
        );

        let from_node = self.get_node_of_current_namespace();

        // Resolve the target module name
        let module_name_str = self.graph.interner.resolve(self.module_name).to_owned();
        let tgt_name = if let Some(ref module) = node.module {
            let module_str = module.id.to_string();
            if node.level > 0 {
                // Relative import
                let parts: Vec<&str> = module_name_str.split('.').collect();
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
            let parts: Vec<&str> = module_name_str.split('.').collect();
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
            let full_name_sym = self.graph.interner.intern(&full_name);
            if self.module_to_filename.contains_key(&full_name_sym) {
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
                        self.nodes_arena[id].get_name(&self.graph.interner)
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
                    self.nodes_arena[from_node].get_name(&self.graph.interner),
                    self.nodes_arena[to_node].get_name(&self.graph.interner)
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
        let tgt_sym = self.graph.interner.intern(tgt_module);
        // Collect the source module's bindings while holding an immutable
        // borrow on self.scopes.
        let bindings: Vec<(SymId, ValueSet)> = if let Some(scope) = self.scopes.get(&tgt_sym) {
            let all_exports = scope.all_exports.clone();
            scope
                .defs
                .iter()
                .filter(|&(&name_sym, ref vs)| {
                    if vs.is_empty() {
                        return false;
                    }
                    if let Some(ref exports) = all_exports {
                        return exports.contains(&name_sym);
                    }
                    let name_str = self.graph.interner.resolve(name_sym);
                    !name_str.starts_with('_')
                })
                .map(|(&k, v)| (k, v.clone()))
                .collect()
        } else {
            Vec::new()
        };

        for (name_sym, values) in bindings {
            let name_str = self.graph.interner.resolve(name_sym).to_owned();
            for id in values.iter() {
                self.set_value(&name_str, Some(id));
                self.add_uses_edge(from_node, id);
                debug!(
                    "Star-import: {} -> {}",
                    name_str,
                    self.nodes_arena[id].get_name(&self.graph.interner)
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
                        if let Some(rets) = self.function_returns.get(&fid) {
                            for &ret_id in rets {
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
                for rhs in rhs_elts.iter().take(middle_end).skip(middle_start) {
                    self.visit_expr(rhs, line_index);
                    let shallow = self.resolve_shallow_value(rhs);
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
            self.graph.interner.resolve(self.filename),
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

        // Bind loop targets to the iterable's element values when shallow
        // container facts are available; otherwise fall back to the iterator.
        self.bind_iteration_target(&node.target, &node.iter, iter_node, line_index);

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
            let ns = self.nodes_arena[from_node].get_name(&self.graph.interner).to_owned();
            let to_node = self.get_node(Some(&ns), &alias_name, Flavor::Name);
            self.add_defines_edge(from_node, Some(to_node));
            let line = line_index.line_index(node.range().start()).get();
            self.associate_node(to_node, self.filename, line);
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
                        self.nodes_arena[from_node].get_name(&self.graph.interner),
                        self.nodes_arena[to_id].get_name(&self.graph.interner)
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
                        self.nodes_arena[from_node].get_name(&self.graph.interner),
                        self.nodes_arena[to].get_name(&self.graph.interner)
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
                    let ns = self.nodes_arena[obj_id].get_name(&self.graph.interner).to_owned();

                    // Direct lookup in the object's scope, then MRO.
                    let attr_result = self.lookup_in_scope(&ns, &attr_name).or_else(|| {
                        if let Some(mro) = self.mro.get(&obj_id) {
                            return mro.iter().skip(1).find_map(|&base_id| {
                                let base_ns = self.nodes_arena[base_id].get_name(&self.graph.interner).to_owned();
                                self.lookup_in_scope(&base_ns, &attr_name)
                            });
                        }
                        None
                    });

                    let emit_id = if let Some(attr_id) = attr_result {
                        // Attr is known: emit uses edge to the concrete node.
                        if self.add_uses_edge(from_node, attr_id) {
                            info!(
                                "New edge added for Use from {} to {} (multi-value attr)",
                                self.nodes_arena[from_node].get_name(&self.graph.interner),
                                self.nodes_arena[attr_id].get_name(&self.graph.interner)
                            );
                        }
                        let attr_ns = self.nodes_arena[attr_id].namespace;
                        if attr_ns.is_some() {
                            let attr_name_sym = self.graph.interner.intern(&attr_name);
                            self.remove_wild(from_node, attr_id, attr_name_sym);
                        }
                        attr_id
                    } else {
                        // Obj is known but attr is not: create a placeholder
                        // attribute node so callers can chain off it.
                        let attr_node = self.get_node(Some(&ns), &attr_name, Flavor::Attribute);
                        self.add_uses_edge(from_node, attr_node);
                        let attr_name_sym = self.graph.interner.intern(&attr_name);
                        self.remove_wild(from_node, obj_id, attr_name_sym);
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
                    self.nodes_arena[from_node].get_name(&self.graph.interner),
                    self.nodes_arena[result_id].get_name(&self.graph.interner)
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
            let func_name = self.nodes_arena[func_id].get_name(&self.graph.interner).to_owned();
            let init_node = self.get_node(Some(&func_name), "__init__", Flavor::Method);
            if self.add_uses_edge(from_node, init_node) {
                info!(
                    "New edge added for Use from {} to {} (class instantiation)",
                    self.nodes_arena[from_node].get_name(&self.graph.interner),
                    self.nodes_arena[init_node].get_name(&self.graph.interner)
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
            if let Some(fn_id) = func_node
                && !cs.contains(&fn_id)
            {
                cs.push(fn_id);
            }
            cs
        };

        // For function calls with known return types, propagate all return
        // candidates so that callers can resolve attributes on the returned
        // object and multi-return call sites don't under-approximate.
        let from_node = self.get_node_of_current_namespace();
        let mut first_ret: Option<NodeId> = None;
        for &fid in &func_candidates {
            let ret_ids: Vec<NodeId> = self
                .function_returns
                .get(&fid)
                .map(|ret_ids| ret_ids.iter().copied().collect())
                .unwrap_or_default();
            for ret_id in ret_ids {
                if self.add_uses_edge(from_node, ret_id) {
                    info!(
                        "New edge added for Use from {} to {} (return-value propagation)",
                        self.nodes_arena[from_node].get_name(&self.graph.interner),
                        self.nodes_arena[ret_id].get_name(&self.graph.interner)
                    );
                }
                if first_ret.is_none() {
                    first_ret = Some(ret_id);
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
        let parent_fqn = *self.fqn_cache.last().unwrap();
        let parent_ns = self.graph.interner.resolve(parent_fqn).to_owned();
        let inner_ns = format!("{parent_ns}.{label}");
        let inner_ns_sym = self.graph.interner.intern(&inner_ns);

        // Ensure scope exists
        if !self.scopes.contains_key(&inner_ns_sym) {
            let mut scope = ScopeInfo::new();
            if let Some(ref params) = node.parameters {
                for p in &params.args {
                    let sym = self.graph.interner.intern(p.parameter.name.id.as_str());
                    scope.defs.entry(sym).or_default();
                    scope.locals.insert(sym);
                }
                for p in &params.posonlyargs {
                    let sym = self.graph.interner.intern(p.parameter.name.id.as_str());
                    scope.defs.entry(sym).or_default();
                    scope.locals.insert(sym);
                }
                for p in &params.kwonlyargs {
                    let sym = self.graph.interner.intern(p.parameter.name.id.as_str());
                    scope.defs.entry(sym).or_default();
                    scope.locals.insert(sym);
                }
                if let Some(ref va) = params.vararg {
                    let sym = self.graph.interner.intern(va.name.id.as_str());
                    scope.defs.entry(sym).or_default();
                    scope.locals.insert(sym);
                }
                if let Some(ref kw) = params.kwarg {
                    let sym = self.graph.interner.intern(kw.name.id.as_str());
                    scope.defs.entry(sym).or_default();
                    scope.locals.insert(sym);
                }
            }
            self.scopes.insert(inner_ns_sym, scope);
        }

        let label_sym = self.graph.interner.intern(label);
        self.push_name(label_sym);
        self.scope_stack.push(inner_ns_sym);
        self.context_stack.push(label.to_string());

        if let Some(ref params) = node.parameters {
            self.generate_args_nodes(params, &inner_ns);
            self.analyze_arguments(params, line_index);
        }
        self.visit_expr(&node.body, line_index);

        self.context_stack.pop();
        self.scope_stack.pop();
        self.pop_name();

        // Add defines edge for the lambda
        let from_node = self.get_node_of_current_namespace();
        let from_name = self.nodes_arena[from_node].get_name(&self.graph.interner).to_owned();
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
        let class_ns = self.nodes_arena[obj_id].get_name(&self.graph.interner).to_owned();

        for &method_name in method_names {
            // Direct lookup in the class scope.
            if let Some(method_id) = self.lookup_in_scope(&class_ns, method_name) {
                if self.add_uses_edge(from_node, method_id) {
                    info!(
                        "New edge added for Use from {} to {} (protocol: {})",
                        self.nodes_arena[from_node].get_name(&self.graph.interner),
                        self.nodes_arena[method_id].get_name(&self.graph.interner),
                        method_name
                    );
                }
            } else {
                // Fall back to MRO chain.
                let mro_ids: Option<Vec<usize>> = self.mro.get(&obj_id).map(|m| m.clone());
                let method_id = mro_ids.as_ref().and_then(|mro| {
                    mro.iter().skip(1).find_map(|&base_id| {
                        let base_ns = self.nodes_arena[base_id].get_name(&self.graph.interner).to_owned();
                        self.lookup_in_scope(&base_ns, method_name)
                    })
                });
                if let Some(method_id) = method_id
                    && self.add_uses_edge(from_node, method_id)
                {
                    info!(
                        "New edge added for Use from {} to {} (protocol via MRO: {})",
                        self.nodes_arena[from_node].get_name(&self.graph.interner),
                        self.nodes_arena[method_id].get_name(&self.graph.interner),
                        method_name
                    );
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
        let parent_fqn = *self.fqn_cache.last().unwrap();
        let parent_ns = self.graph.interner.resolve(parent_fqn).to_owned();
        let inner_ns = format!("{parent_ns}.{label}");
        let inner_ns_sym = self.graph.interner.intern(&inner_ns);
        if !self.scopes.contains_key(&inner_ns_sym) {
            let mut target_names = FxHashSet::default();
            for comp in generators {
                collect_target_names_from_expr(&comp.target, &mut target_names);
            }
            let mut scope = ScopeInfo::new();
            for name_str in &target_names {
                let sym = self.graph.interner.intern(name_str);
                scope.defs.entry(sym).or_default();
                scope.locals.insert(sym);
            }
            self.scopes.insert(inner_ns_sym, scope);
        }

        // Enter inner scope
        let label_sym = self.graph.interner.intern(label);
        self.push_name(label_sym);
        self.scope_stack.push(inner_ns_sym);
        self.context_stack.push(label.to_string());

        // Bind outermost targets
        self.bind_iteration_target(&outermost.target, &outermost.iter, iter_node, line_index);
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
            self.bind_iteration_target(&comp.target, &comp.iter, val, line_index);
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
        self.pop_name();

        // Add defines edge
        let from_node = self.get_node_of_current_namespace();
        let from_name = self.nodes_arena[from_node].get_name(&self.graph.interner).to_owned();
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

    fn bind_iteration_target(
        &mut self,
        target: &Expr,
        iter_expr: &Expr,
        iter_node: Option<NodeId>,
        line_index: &LineIndex,
    ) {
        let iterated = self
            .resolve_shallow_value(iter_expr)
            .containers
            .resolve_subscript(None);
        if !iterated.values.is_empty() || !iterated.containers.is_empty() {
            self.bind_target_to_shallow_value(target, &iterated);
        } else {
            self.analyze_binding_simple(target, iter_node, line_index);
        }
    }
}

#[cfg(test)]
mod prepass_tests {
    use super::*;

    use std::fs;

    use tempfile::tempdir;

    fn parse_module(source: &str) -> ModModule {
        let parsed = ruff_python_parser::parse_unchecked(source, ParseOptions::from(Mode::Module));
        match parsed.into_syntax() {
            Mod::Module(module) => module,
            other => panic!("expected module syntax, got {other:?}"),
        }
    }

    fn find_exact_node(cg: &CallGraph, exact_name: &str) -> NodeId {
        cg.nodes_arena
            .iter()
            .enumerate()
            .find_map(|(id, node)| (node.get_name(&cg.interner) == exact_name).then_some(id))
            .unwrap_or_else(|| panic!("node {exact_name} not found"))
    }

    fn exact_uses(cg: &CallGraph, exact_name: &str) -> FxHashSet<String> {
        let from_id = find_exact_node(cg, exact_name);
        cg.uses_edges
            .get(&from_id)
            .into_iter()
            .flat_map(|targets| targets.iter())
            .map(|&id| cg.nodes_arena[id].get_name(&cg.interner).to_string())
            .collect()
    }

    #[test]
    fn build_scopes_collects_compound_bindings() {
        let module = parse_module(
            r#"
total += 1
annotated: int = 1
first, (second, *rest) = values
[left, right] = pairs
for item, (loop_left, loop_right) in rows:
    pass
if cond:
    from_if = 1
elif other:
    from_elif = 1
while cond2:
    from_while = 1
else:
    from_while_else = 1
with ctx() as handle, other() as (cm_left, cm_right):
    from_with = 1
try:
    from_try = 1
except Err as err:
    from_except = 1
else:
    from_else = 1
finally:
    from_finally = 1
__all__ = ["public_name"]
"#,
        );

        let mut interner = Interner::new();
        let scopes = AnalysisSession::build_scopes(&module, "pkg.mod", &mut interner);
        let ns_sym = interner.intern("pkg.mod");
        let scope = scopes.get(&ns_sym).expect("module scope should exist");

        for name in [
            "total",
            "annotated",
            "first",
            "second",
            "rest",
            "left",
            "right",
            "item",
            "loop_left",
            "loop_right",
            "from_if",
            "from_elif",
            "from_while",
            "from_while_else",
            "handle",
            "cm_left",
            "cm_right",
            "from_with",
            "from_try",
            "err",
            "from_except",
            "from_else",
            "from_finally",
        ] {
            let sym = interner.intern(name);
            assert!(
                scope.defs.contains_key(&sym),
                "missing scope def for {name}"
            );
            assert!(scope.locals.contains(&sym), "missing local binding for {name}");
        }

        let public_name_sym = interner.intern("public_name");
        assert_eq!(
            scope.all_exports,
            Some(FxHashSet::from_iter([public_name_sym])),
            "__all__ should be collected from a literal assignment"
        );
    }

    #[test]
    fn build_scopes_collects_nested_scopes_from_compound_statements() {
        let module = parse_module(
            r#"
if cond:
    def in_if():
        pass
elif other:
    class InElif:
        pass
while cond2:
    def in_while():
        pass
else:
    class InWhileElse:
        pass
for item in rows:
    def in_for():
        pass
else:
    class InForElse:
        pass
with ctx() as handle:
    def in_with():
        pass
try:
    def in_try():
        pass
except Err:
    class InExcept:
        pass
else:
    def in_else():
        pass
finally:
    class InFinally:
        pass
"#,
        );

        let mut interner = Interner::new();
        let scopes = AnalysisSession::build_scopes(&module, "pkg.mod", &mut interner);
        for scope_name in [
            "pkg.mod.in_if",
            "pkg.mod.InElif",
            "pkg.mod.in_while",
            "pkg.mod.InWhileElse",
            "pkg.mod.in_for",
            "pkg.mod.InForElse",
            "pkg.mod.in_with",
            "pkg.mod.in_try",
            "pkg.mod.InExcept",
            "pkg.mod.in_else",
            "pkg.mod.InFinally",
        ] {
            let sym = interner.intern(scope_name);
            assert!(
                scopes.contains_key(&sym),
                "missing nested scope {scope_name}"
            );
        }
    }

    #[test]
    fn build_scopes_collects_for_body_and_else_bindings() {
        let module = parse_module(
            r#"
for item in rows:
    loop_value = item
else:
    from_for_else = 1
"#,
        );

        let mut interner = Interner::new();
        let scopes = AnalysisSession::build_scopes(&module, "pkg.mod", &mut interner);
        let ns_sym = interner.intern("pkg.mod");
        let scope = scopes.get(&ns_sym).expect("module scope should exist");

        for name in ["item", "loop_value", "from_for_else"] {
            let sym = interner.intern(name);
            assert!(
                scope.defs.contains_key(&sym),
                "missing scope def for {name}"
            );
            assert!(scope.locals.contains(&sym), "missing local binding for {name}");
        }
    }

    #[test]
    fn merge_scopes_preserves_existing_exports() {
        let mut session = AnalysisSession::new(&[], None);
        let kept_sym = session.graph.interner.intern("kept");
        let added_sym = session.graph.interner.intern("added");
        let ns_sym = session.graph.interner.intern("pkg.mod");
        let mut existing = ScopeInfo::new();
        existing.defs.insert(kept_sym, ValueSet::empty());
        existing.all_exports = Some(FxHashSet::from_iter([kept_sym]));
        session.scopes.insert(ns_sym, existing);

        let mut incoming = ScopeInfo::new();
        incoming.defs.insert(added_sym, ValueSet::empty());

        session.merge_scopes(&FxHashMap::from_iter([(ns_sym, incoming)]));

        let merged = session.scopes.get(&ns_sym).expect("merged scope should exist");
        assert!(merged.defs.contains_key(&kept_sym));
        assert!(merged.defs.contains_key(&added_sym));
        assert_eq!(
            merged.all_exports,
            Some(FxHashSet::from_iter([kept_sym])),
            "existing __all__ exports should not be overwritten by None"
        );
    }

    #[test]
    fn merge_scopes_unions_container_facts() {
        let mut session = AnalysisSession::new(&[], None);
        let items_sym = session.graph.interner.intern("items");
        let ns_sym = session.graph.interner.intern("pkg.mod");

        let mut existing = ScopeInfo::new();
        let mut existing_facts = ContainerFacts::default();
        existing_facts.push(ContainerFact::Sequence(vec![ShallowValue::default()]));
        existing.containers.insert(items_sym, existing_facts);
        session.scopes.insert(ns_sym, existing);

        let mut incoming = ScopeInfo::new();
        let mut mapping = FxHashMap::default();
        mapping.insert(LiteralKey::String("k".to_string()), ShallowValue::default());
        let mut incoming_facts = ContainerFacts::default();
        incoming_facts.push(ContainerFact::Mapping(mapping));
        incoming.containers.insert(items_sym, incoming_facts);

        session.merge_scopes(&FxHashMap::from_iter([(ns_sym, incoming)]));

        let merged = session
            .scopes
            .get(&ns_sym)
            .and_then(|scope| scope.containers.get(&items_sym))
            .expect("merged containers should exist");
        assert_eq!(
            merged.0.len(),
            2,
            "container facts should be unioned instead of replaced"
        );
    }

    #[test]
    fn get_node_reuses_existing_id_and_upgrades_flavor() {
        let filename = "pkg/mod.py".to_string();
        let mut session = AnalysisSession::new(std::slice::from_ref(&filename), None);
        session.filename = session.graph.interner.intern(&filename);

        let generic = session.get_node(Some("pkg.mod"), "thing", Flavor::Namespace);
        let upgraded = session.get_node(Some("pkg.mod"), "thing", Flavor::Function);
        let sibling = session.get_node(Some("pkg.other"), "thing", Flavor::Function);

        assert_eq!(
            generic, upgraded,
            "same namespace/name should reuse node id"
        );
        assert_eq!(session.nodes_arena[generic].flavor, Flavor::Function);
        assert_ne!(generic, sibling, "different namespaces must not alias");
        let thing_sym = session.graph.interner.intern("thing");
        assert_eq!(session.nodes_by_name[&thing_sym].len(), 2);
    }

    #[test]
    fn is_local_checks_only_the_innermost_scope() {
        let mut session = AnalysisSession::new(&[], None);
        let outer_only_sym = session.graph.interner.intern("outer_only");
        let inner_only_sym = session.graph.interner.intern("inner_only");
        let pkg_sym = session.graph.interner.intern("pkg");
        let pkg_fn_sym = session.graph.interner.intern("pkg.fn");
        let mut outer = ScopeInfo::new();
        outer.locals.insert(outer_only_sym);
        session.scopes.insert(pkg_sym, outer);

        let mut inner = ScopeInfo::new();
        inner.locals.insert(inner_only_sym);
        session.scopes.insert(pkg_fn_sym, inner);

        session.scope_stack = vec![pkg_sym, pkg_fn_sym];

        assert!(session.is_local("inner_only"));
        assert!(
            !session.is_local("outer_only"),
            "outer locals should not count as locals in the current scope"
        );
    }

    #[test]
    fn process_reanalyzes_until_return_types_converge() {
        let temp = tempdir().expect("temp dir should be created");
        let path = temp.path().join("propagation_chain.py");
        fs::write(
            &path,
            r#"
def caller():
    return first().make()

def first():
    return second()

def second():
    return third()

def third():
    return Product()

class Product:
    def make(self):
        pass
"#,
        )
        .expect("fixture should be written");

        let files = vec![path.to_string_lossy().to_string()];
        let cg = CallGraph::new(&files, None).expect("analysis should succeed");
        let caller_uses = exact_uses(&cg, "propagation_chain.caller");

        assert!(
            caller_uses.contains("propagation_chain.first"),
            "caller should keep the direct call edge"
        );
        assert!(
            caller_uses.contains("propagation_chain.Product.make"),
            "caller should resolve make() after multi-pass return propagation, got: {caller_uses:?}"
        );
    }
}

#[cfg(test)]
mod visitor_tests {
    use super::*;

    use crate::FxHashSet;
    use std::fs;

    use tempfile::tempdir;

    fn parse_module(source: &str) -> ModModule {
        let parsed = ruff_python_parser::parse_unchecked(source, ParseOptions::from(Mode::Module));
        match parsed.into_syntax() {
            Mod::Module(module) => module,
            _ => panic!("expected module syntax"),
        }
    }

    fn return_expr<'a>(module: &'a ModModule, fn_name: &str) -> &'a Expr {
        let func = module
            .body
            .iter()
            .find_map(|stmt| match stmt {
                Stmt::FunctionDef(func) if func.name.id.as_str() == fn_name => Some(func),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing function {fn_name}"));
        func.body
            .iter()
            .find_map(|stmt| match stmt {
                Stmt::Return(ret) => ret.value.as_ref(),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing return expr in {fn_name}"))
    }

    fn enter_function(session: &mut AnalysisSession, module_ns: &str, fn_name: &str) {
        session.module_name = session.graph.interner.intern(module_ns);
        session.filename = session.graph.interner.intern(&format!("{module_ns}.py"));
        let ns_sym = session.graph.interner.intern(module_ns);
        let fn_sym = session.graph.interner.intern(fn_name);
        let fn_ns_str = format!("{module_ns}.{fn_name}");
        let fn_ns_sym = session.graph.interner.intern(&fn_ns_str);
        session.name_stack = vec![ns_sym, fn_sym];
        session.fqn_cache = vec![ns_sym, fn_ns_sym];
        session.scope_stack = vec![ns_sym, fn_ns_sym];
        session.context_stack = vec![
            format!("Module {module_ns}"),
            format!("FunctionDef {fn_name}"),
        ];
    }

    fn has_uses_edge(cg: &CallGraph, from_suffix: &str, to_suffix: &str) -> bool {
        for (from_id, targets) in &cg.uses_edges {
            if cg.nodes_arena[*from_id].get_name(&cg.interner).ends_with(from_suffix) {
                for target in targets {
                    if cg.nodes_arena[*target].get_name(&cg.interner).ends_with(to_suffix) {
                        return true;
                    }
                }
            }
        }
        false
    }

    #[test]
    fn build_scopes_collects_bindings_from_compound_statements() {
        let mut interner = Interner::new();
        let module = parse_module(
            r#"
def sample(items, manager, cond):
    total = 0
    total += 1
    value: int = 1
    pair, [inner, *rest] = items
    for first, *tail in items:
        loop_value = first
    if cond:
        branch_value = total
    else:
        fallback_value = value
    while cond:
        while_value = loop_value
    with manager as resource:
        with_value = resource
    try:
        try_value = branch_value
    except Exception as err:
        except_value = err
    finally:
        final_value = total
"#,
        );

        let scopes = AnalysisSession::build_scopes(&module, "fixture", &mut interner);
        let scope_sym = interner.intern("fixture.sample");
        let scope = scopes.get(&scope_sym).expect("function scope");

        for name in [
            "items",
            "manager",
            "cond",
            "total",
            "value",
            "pair",
            "inner",
            "rest",
            "first",
            "tail",
            "branch_value",
            "fallback_value",
            "while_value",
            "resource",
            "with_value",
            "try_value",
            "err",
            "except_value",
            "final_value",
        ] {
            let sym = interner.intern(name);
            assert!(scope.defs.contains_key(&sym), "missing binding for {name}");
        }
    }

    #[test]
    fn build_scopes_collects_nested_scopes_inside_compound_statements() {
        let mut interner = Interner::new();
        let module = parse_module(
            r#"
def outer(cond, items, manager):
    if cond:
        def in_if():
            pass
    while cond:
        def in_while():
            pass
    for item in items:
        def in_for():
            pass
    with manager:
        def in_with():
            pass
    try:
        def in_try():
            pass
    except Exception:
        def in_except():
            pass
    else:
        def in_else():
            pass
    finally:
        def in_finally():
            pass
"#,
        );

        let scopes = AnalysisSession::build_scopes(&module, "fixture", &mut interner);
        for scope_name in [
            "fixture.outer",
            "fixture.outer.in_if",
            "fixture.outer.in_while",
            "fixture.outer.in_for",
            "fixture.outer.in_with",
            "fixture.outer.in_try",
            "fixture.outer.in_except",
            "fixture.outer.in_else",
            "fixture.outer.in_finally",
        ] {
            let sym = interner.intern(scope_name);
            assert!(
                scopes.contains_key(&sym),
                "missing nested scope {scope_name}"
            );
        }
    }

    #[test]
    fn merge_scopes_preserves_existing_all_exports() {
        let mut session = AnalysisSession::new(&[], None);
        let keep_id = session.get_node(Some("pkg"), "kept", Flavor::Function);
        let extra_id = session.get_node(Some("pkg"), "extra", Flavor::Function);

        let kept_sym = session.graph.interner.intern("kept");
        let extra_sym = session.graph.interner.intern("extra");
        let pkg_sym = session.graph.interner.intern("pkg");

        let mut existing = ScopeInfo::new();
        existing
            .defs
            .entry(kept_sym)
            .or_default()
            .insert(keep_id);
        existing.all_exports = Some(FxHashSet::from_iter([kept_sym]));
        session.scopes.insert(pkg_sym, existing);

        let mut incoming = ScopeInfo::new();
        incoming
            .defs
            .entry(extra_sym)
            .or_default()
            .insert(extra_id);

        let mut incoming_scopes = FxHashMap::default();
        incoming_scopes.insert(pkg_sym, incoming);

        session.merge_scopes(&incoming_scopes);

        let merged = session.scopes.get(&pkg_sym).expect("merged scope");
        assert_eq!(
            merged.all_exports.as_ref(),
            Some(&FxHashSet::from_iter([kept_sym])),
            "existing __all__ should not be cleared by an incoming scope without exports",
        );
        assert!(
            merged
                .defs
                .get(&extra_sym)
                .is_some_and(|values| values.iter().collect::<Vec<_>>() == vec![extra_id]),
            "incoming defs should still be merged"
        );
    }

    #[test]
    fn get_node_reuses_ids_and_upgrades_flavor() {
        let mut session = AnalysisSession::new(&[], None);
        session.filename = session.graph.interner.intern("fixture.py");

        let first = session.get_node(Some("pkg"), "thing", Flavor::Namespace);
        let second = session.get_node(Some("pkg"), "thing", Flavor::Method);

        assert_eq!(
            first, second,
            "same (namespace, name) should reuse the node id"
        );
        assert_eq!(
            session.nodes_arena[first].flavor,
            Flavor::Method,
            "later, more-specific lookups should upgrade the stored flavor",
        );
    }

    #[test]
    fn is_local_reads_the_innermost_scope() {
        let mut session = AnalysisSession::new(&[], None);
        let local_sym = session.graph.interner.intern("local_value");
        let ns_sym = session.graph.interner.intern("pkg.inner");
        let mut scope = ScopeInfo::new();
        scope.locals.insert(local_sym);
        session.scopes.insert(ns_sym, scope);
        session.scope_stack.push(ns_sym);

        assert!(session.is_local("local_value"));
        assert!(!session.is_local("missing_value"));
    }

    #[test]
    fn comprehension_visitors_return_their_namespace_nodes() {
        let source = r#"
def set_case(items):
    return {item for item in items}

def dict_case(items):
    return {item: item for item in items}

def gen_case(items):
    return (item for item in items)
"#;
        let module = parse_module(source);
        let line_index = LineIndex::from_source_text(source);

        for (fn_name, expected_label) in [
            ("set_case", "setcomp"),
            ("dict_case", "dictcomp"),
            ("gen_case", "genexpr"),
        ] {
            let mut session = AnalysisSession::new(&[], None);
            session.scopes = AnalysisSession::build_scopes(&module, "fixture", &mut session.graph.interner);
            enter_function(&mut session, "fixture", fn_name);

            let node_id = match return_expr(&module, fn_name) {
                Expr::SetComp(expr) => session
                    .visit_set_comp(expr, &line_index)
                    .expect("set comp should create a namespace node"),
                Expr::DictComp(expr) => session
                    .visit_dict_comp(expr, &line_index)
                    .expect("dict comp should create a namespace node"),
                Expr::Generator(expr) => session
                    .visit_generator(expr, &line_index)
                    .expect("generator should create a namespace node"),
                other => panic!("unexpected expression for {fn_name}: {other:?}"),
            };

            let parent_id = session.get_node(Some("fixture"), fn_name, Flavor::Namespace);
            let targets = session
                .defines_edges
                .get(&parent_id)
                .expect("comprehension defines edge");
            assert!(
                targets.contains(&node_id),
                "{fn_name} should define its comprehension node"
            );
            let expected_sym = session.graph.interner.intern(expected_label);
            assert_eq!(session.nodes_arena[node_id].name, expected_sym);
        }
    }

    #[test]
    fn process_propagates_return_types_until_fixpoint() {
        let dir = tempdir().expect("temp dir");
        let fixture = dir.path().join("chain.py");
        fs::write(
            &fixture,
            r#"
def caller():
    return top().ping()

def top():
    return mid()

def mid():
    return leaf()

def leaf():
    return Product()

class Product:
    def ping(self):
        return 1
"#,
        )
        .expect("write fixture");

        let files = vec![fixture.to_string_lossy().to_string()];
        let cg = CallGraph::new(&files, None).expect("analysis should succeed");

        assert!(
            has_uses_edge(&cg, "chain.caller", "chain.Product.ping"),
            "caller should resolve ping through multiple return-propagation passes",
        );
    }

    #[test]
    fn process_resolves_super_calls_through_computed_mro() {
        let dir = tempdir().expect("temp dir");
        let fixture = dir.path().join("super_chain.py");
        fs::write(
            &fixture,
            r#"
class Base:
    def greet(self):
        return 1

class Derived(Base):
    def greet(self):
        return super().greet()

def call_greet():
    inst = Derived()
    return inst.greet()
"#,
        )
        .expect("write fixture");

        let files = vec![fixture.to_string_lossy().to_string()];
        let cg = CallGraph::new(&files, None).expect("analysis should succeed");

        assert!(
            has_uses_edge(&cg, "super_chain.Derived.greet", "super_chain.Base.greet"),
            "super() should resolve to the next class in the MRO",
        );
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;

    fn parse_module(src: &str) -> ModModule {
        let parsed = ruff_python_parser::parse_unchecked(src, ParseOptions::from(Mode::Module));
        match parsed.into_syntax() {
            Mod::Module(module) => module,
            _ => panic!("expected module AST"),
        }
    }

    #[test]
    fn build_scopes_collects_compound_bindings_and_nested_scopes() {
        let module = parse_module(
            r#"
counter = 0
counter += 1
value: int = 1
for item, (left, *rest) in items:
    pass
if cond:
    from_if = 1
elif other:
    from_elif = 2
else:
    from_else = 3
while cond:
    from_while = 4
else:
    from_while_else = 5
with ctx() as manager, other() as (a, b):
    from_with = 6
try:
    from_try = 7
except Err as exc:
    from_except = 8
else:
    from_try_else = 9
finally:
    from_finally = 10

def outer(posonly, /, arg, *va, kw, **kwarg):
    def nested():
        pass

    class Inner:
        pass

    if flag:
        def in_if():
            pass

    while flag:
        def in_while():
            pass

    for thing in items:
        def in_for():
            pass

    with ctx() as bound:
        def in_with():
            pass

    try:
        def in_try():
            pass
    except Err:
        def in_except():
            pass
    else:
        def in_else():
            pass
    finally:
        def in_finally():
            pass
"#,
        );

        let mut interner = Interner::new();
        let scopes = AnalysisSession::build_scopes(&module, "pkg.mod", &mut interner);
        let module_sym = interner.intern("pkg.mod");
        let module_scope = scopes.get(&module_sym).expect("module scope present");
        for name in [
            "counter",
            "value",
            "item",
            "left",
            "rest",
            "from_if",
            "from_elif",
            "from_else",
            "from_while",
            "from_while_else",
            "manager",
            "a",
            "b",
            "from_with",
            "from_try",
            "exc",
            "from_except",
            "from_try_else",
            "from_finally",
            "outer",
        ] {
            let sym = interner.intern(name);
            assert!(
                module_scope.defs.contains_key(&sym),
                "module scope should define {name}"
            );
        }

        let outer_sym = interner.intern("pkg.mod.outer");
        let outer_scope = scopes.get(&outer_sym).expect("outer scope present");
        for name in [
            "posonly",
            "arg",
            "va",
            "kw",
            "kwarg",
            "nested",
            "Inner",
            "thing",
            "bound",
            "in_if",
            "in_while",
            "in_with",
            "in_try",
            "in_except",
            "in_else",
            "in_finally",
        ] {
            let sym = interner.intern(name);
            assert!(
                outer_scope.defs.contains_key(&sym),
                "outer scope should define {name}"
            );
        }

        for ns in [
            "pkg.mod.outer.nested",
            "pkg.mod.outer.Inner",
            "pkg.mod.outer.in_if",
            "pkg.mod.outer.in_while",
            "pkg.mod.outer.in_for",
            "pkg.mod.outer.in_with",
            "pkg.mod.outer.in_try",
            "pkg.mod.outer.in_except",
            "pkg.mod.outer.in_else",
            "pkg.mod.outer.in_finally",
        ] {
            let sym = interner.intern(ns);
            assert!(scopes.contains_key(&sym), "missing nested scope {ns}");
        }
    }

    #[test]
    fn merge_scopes_preserves_existing_all_exports() {
        let mut session = AnalysisSession::new(&[], None);

        let keep_sym = session.graph.interner.intern("keep");
        let replace_sym = session.graph.interner.intern("replace");
        let pkg_mod_sym = session.graph.interner.intern("pkg.mod");
        let current_sym = session.graph.interner.intern("current");

        let mut existing = ScopeInfo::new();
        existing.all_exports = Some(FxHashSet::from_iter([keep_sym]));
        session.scopes.insert(pkg_mod_sym, existing);

        let mut incoming = ScopeInfo::new();
        incoming.all_exports = Some(FxHashSet::from_iter([replace_sym]));
        session
            .scopes
            .get_mut(&pkg_mod_sym)
            .expect("existing scope")
            .defs
            .insert(current_sym, ValueSet::empty());

        let incoming_scopes = FxHashMap::from_iter([(pkg_mod_sym, incoming)]);
        session.merge_scopes(&incoming_scopes);

        let merged = session.scopes.get(&pkg_mod_sym).expect("merged scope");
        assert_eq!(
            merged.all_exports.as_ref(),
            Some(&FxHashSet::from_iter([keep_sym])),
            "existing __all__ should not be overwritten"
        );
    }

    #[test]
    fn get_node_reuses_identity_and_only_upgrades_flavor() {
        let mut session = AnalysisSession::new(&[], None);
        session.filename = session.graph.interner.intern("fixture.py");

        let id = session.get_node(Some("pkg.mod"), "thing", Flavor::Namespace);
        let upgraded = session.get_node(Some("pkg.mod"), "thing", Flavor::Method);
        let downgraded = session.get_node(Some("pkg.mod"), "thing", Flavor::Name);

        assert_eq!(id, upgraded, "node lookup should reuse the same node id");
        assert_eq!(
            id, downgraded,
            "node lookup should stay keyed by namespace+name"
        );
        assert_eq!(session.nodes_arena[id].flavor, Flavor::Method);
    }

    #[test]
    fn is_local_checks_current_scope_locals() {
        let mut session = AnalysisSession::new(&[], None);
        let local_name_sym = session.graph.interner.intern("local_name");
        let pkg_mod_sym = session.graph.interner.intern("pkg.mod");
        let mut scope = ScopeInfo::new();
        scope.locals.insert(local_name_sym);
        session.scopes.insert(pkg_mod_sym, scope);
        session.scope_stack.push(pkg_mod_sym);

        assert!(session.is_local("local_name"));
        assert!(!session.is_local("missing"));
    }

    #[test]
    fn record_function_return_sets_dirty_flag_only_for_new_values() {
        let mut session = AnalysisSession::new(&[], None);
        let func = session.get_node(Some("pkg.mod"), "factory", Flavor::Function);
        let ret = session.get_node(Some("pkg.mod"), "Product", Flavor::Class);

        assert!(session.record_function_return(func, ret));
        assert!(
            session.function_returns_changed,
            "new return discoveries should mark the pass dirty"
        );

        session.function_returns_changed = false;
        assert!(
            !session.record_function_return(func, ret),
            "re-inserting an existing return should be a no-op"
        );
        assert!(
            !session.function_returns_changed,
            "duplicate return discoveries should not keep the pass dirty"
        );
    }

    #[test]
    fn state_helpers_write_to_nearest_declaring_scope() {
        let mut session = AnalysisSession::new(&[], None);

        let shared_sym = session.graph.interner.intern("shared");
        let inner_only_sym = session.graph.interner.intern("inner_only");
        let pkg_mod_sym = session.graph.interner.intern("pkg.mod");
        let pkg_mod_inner_sym = session.graph.interner.intern("pkg.mod.inner");

        let mut outer = ScopeInfo::new();
        outer.defs.insert(shared_sym, ValueSet::empty());
        let mut inner = ScopeInfo::new();
        inner
            .defs
            .insert(inner_only_sym, ValueSet::empty());

        session.scopes.insert(pkg_mod_sym, outer);
        session.scopes.insert(pkg_mod_inner_sym, inner);
        session.scope_stack.push(pkg_mod_sym);
        session.scope_stack.push(pkg_mod_inner_sym);

        let value_id = session.get_node(Some("pkg.mod"), "Product", Flavor::Class);
        let mut containers = ContainerFacts::default();
        containers.push(ContainerFact::Sequence(vec![ShallowValue::default()]));

        session.set_value("shared", Some(value_id));
        session.set_containers("shared", &containers);

        let outer_scope = session
            .scopes
            .get(&pkg_mod_sym)
            .expect("outer scope should exist");
        let inner_scope = session
            .scopes
            .get(&pkg_mod_inner_sym)
            .expect("inner scope should exist");

        assert!(
            outer_scope
                .defs
                .get(&shared_sym)
                .expect("outer binding should exist")
                .iter()
                .any(|id| id == value_id)
        );
        assert!(
            outer_scope
                .containers
                .get(&shared_sym)
                .is_some_and(|facts| !facts.is_empty()),
            "outer declaring scope should receive container facts"
        );
        assert!(
            !inner_scope.defs.contains_key(&shared_sym),
            "writes should not create a shadowing binding in the innermost scope"
        );
        assert!(
            !inner_scope.containers.contains_key(&shared_sym),
            "container writes should target the nearest declaration"
        );
    }

    #[test]
    fn add_uses_edge_replaces_matching_wildcard_only() {
        let mut session = AnalysisSession::new(&[], None);

        let caller = session.get_node(Some("pkg.mod"), "caller", Flavor::Function);
        let wildcard_foo = session.get_node(None, "foo", Flavor::Name);
        let wildcard_bar = session.get_node(None, "bar", Flavor::Name);
        let concrete_foo = session.get_node(Some("pkg.mod"), "foo", Flavor::Function);

        assert!(session.add_uses_edge(caller, wildcard_foo));
        assert!(session.add_uses_edge(caller, wildcard_bar));
        assert!(session.add_uses_edge(caller, concrete_foo));

        let edges = session
            .uses_edges
            .get(&caller)
            .expect("caller should have uses edges");
        assert!(
            !edges.contains(&wildcard_foo),
            "concrete resolution should remove the matching wildcard edge"
        );
        assert!(
            edges.contains(&wildcard_bar),
            "unrelated wildcard edges should remain"
        );
        assert!(
            edges.contains(&concrete_foo),
            "concrete target should remain after wildcard cleanup"
        );
    }
}
