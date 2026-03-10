use std::collections::{HashMap, HashSet};

// log::info is used transitively via add_uses_edge/add_defines_edge in super

use crate::node::{Flavor, Node, NodeId};

impl super::CallGraph {
    // =====================================================================
    // Postprocessing
    // =====================================================================

    pub(super) fn postprocess(&mut self) {
        self.expand_unknowns();
        self.resolve_imports();
        self.contract_nonexistents();
        self.cull_inherited();
        self.collapse_inner();
    }

    /// For each unknown node `*.name`, replace all its incoming edges
    /// with edges to `X.name` for all possible Xs.
    ///
    /// Precision heuristic (INV-2): if a caller already has a *concrete*
    /// uses edge to any node named `name`, skip wildcard expansion for that
    /// `(caller, name)` pair.  This prevents broad false-positive fanout when
    /// the abstract-value machinery has already produced a better resolution.
    fn expand_unknowns(&mut self) {
        // Build index of (from_id, short_name) pairs that already have a
        // concrete (namespaced) uses edge.  Wildcards whose (from, name) are
        // covered by a concrete edge will be suppressed.
        let mut concrete_uses_pairs: HashSet<(NodeId, String)> = HashSet::new();
        for (&from, targets) in &self.uses_edges {
            for &to in targets {
                if self.nodes_arena[to].namespace.is_some() {
                    concrete_uses_pairs.insert((from, self.nodes_arena[to].name.clone()));
                }
            }
        }

        // Collect new defines edges (unchanged -- no precision scoping for defines).
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

        // Collect new uses edges -- skip when a concrete resolution already exists
        // for the same (from, name) pair (precision heuristic).
        let mut new_uses: Vec<(NodeId, NodeId)> = Vec::new();
        for (&from, targets) in &self.uses_edges {
            for &to in targets {
                if self.nodes_arena[to].namespace.is_none() {
                    let name = self.nodes_arena[to].name.clone();
                    // INV-3: Do not fan out private names globally.
                    // A private name that was filtered by star-import should
                    // remain unresolved rather than being reattached to an
                    // unrelated module's private function with the same short
                    // name.  Within-module private calls are already handled
                    // by scope analysis and will appear in concrete_uses_pairs.
                    if name.starts_with('_') {
                        continue;
                    }
                    // Suppress global fanout when the caller already has a
                    // concrete resolution for this short name.
                    if concrete_uses_pairs.contains(&(from, name.clone())) {
                        continue;
                    }
                    if let Some(ids) = self.nodes_by_name.get(&name) {
                        for &candidate in ids {
                            if let Some(ref ns) = self.nodes_arena[candidate].namespace.clone() {
                                // INV-1: If the candidate's module defines __all__ and
                                // this name is not listed, skip the expansion.  Such a
                                // name is intentionally unexported; an unresolved call
                                // to it must not be attributed to the module's private
                                // implementation.
                                if !ns.is_empty() {
                                    if let Some(scope) = self.scopes.get(ns) {
                                        if let Some(ref exports) = scope.all_exports.clone() {
                                            if !exports.contains(&name) {
                                                continue;
                                            }
                                        }
                                    }
                                }
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
    ///
    /// Two strategies are tried in order for each remaining `ImportedItem`:
    ///
    /// 1. **Scope lookup** -- the ImportedItem's namespace IS the source module
    ///    name.  If `scopes[namespace][name]` is non-empty we can remap
    ///    directly, without needing any outgoing edge on the placeholder node.
    ///    This handles re-export chains that were not yet resolved when
    ///    `visit_import_from` ran.
    ///
    /// 2. **Uses-edge walk** -- legacy path kept for modules whose scopes are
    ///    not available (external packages, etc.).
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

            let mod_name = self.nodes_arena[from_id]
                .namespace
                .clone()
                .unwrap_or_default();
            let item_name = self.nodes_arena[from_id].name.clone();

            // Strategy 1: scope lookup in the source module.
            let scope_vals: Vec<NodeId> =
                self.lookup_values_in_scope(&mod_name, &item_name).iter().collect();
            if !scope_vals.is_empty() {
                // Prefer the first concrete (non-ImportedItem) candidate so
                // that we chain through rather than stop at another placeholder.
                let best = scope_vals
                    .iter()
                    .find(|&&id| self.nodes_arena[id].flavor != Flavor::ImportedItem)
                    .or_else(|| scope_vals.first())
                    .copied();
                if let Some(target) = best {
                    import_mapping.insert(from_id, target);
                    // If the target is itself still an ImportedItem, chase it.
                    if self.nodes_arena[target].flavor == Flavor::ImportedItem
                        && target != from_id
                    {
                        to_resolve.push(target);
                    }
                    continue;
                }
            }

            // Strategy 2: uses-edge walk (legacy path for external modules).
            let to_id = if let Some(targets) = self.uses_edges.get(&from_id) {
                if targets.len() == 1 {
                    *targets.iter().next().expect("len == 1 checked above")
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

    // ------------------------------------------------------------------
    // Module-level dependency graph
    // ------------------------------------------------------------------

    /// Derive a module-level dependency graph by collapsing all cross-module
    /// uses edges.
    ///
    /// For each uses edge `src -> tgt`, both nodes are mapped back to their
    /// owning module (via filename for analyzed code, via node name for
    /// external `Flavor::Module` targets).  If they differ, a module-level
    /// edge is emitted.  Deduplication at the module level keeps the graph
    /// clean.
    ///
    /// Returns `(nodes_arena, uses_edges, defined)` suitable for passing
    /// directly to `VisualGraph::from_call_graph`.
    pub fn derive_module_graph(
        &self,
    ) -> (
        Vec<Node>,
        HashMap<NodeId, HashSet<NodeId>>,
        HashSet<NodeId>,
    ) {
        // Reverse mapping: filename -> module name (for analyzed files).
        let filename_to_module: HashMap<&str, &str> = self
            .module_to_filename
            .iter()
            .map(|(m, f)| (f.as_str(), m.as_str()))
            .collect();

        // Collect module nodes and assign new compact IDs.
        let mut module_ids: HashMap<String, NodeId> = HashMap::new();
        let mut new_nodes: Vec<Node> = Vec::new();

        let mut ensure_module = |name: &str, nodes: &mut Vec<Node>| -> NodeId {
            if let Some(&id) = module_ids.get(name) {
                return id;
            }
            let id = nodes.len();
            nodes.push(Node::new(Some(""), name, Flavor::Module));
            module_ids.insert(name.to_string(), id);
            id
        };

        // Seed with all analyzed modules.
        for (mod_name, filename) in &self.module_to_filename {
            let id = ensure_module(mod_name, &mut new_nodes);
            new_nodes[id].filename = Some(filename.clone());
        }

        // Collapse uses_edges to module granularity.
        let mut module_edges: HashMap<NodeId, HashSet<NodeId>> = HashMap::new();

        for (&src, targets) in &self.uses_edges {
            let src_node = &self.nodes_arena[src];
            let src_mod = match src_node.filename.as_deref() {
                Some(f) => filename_to_module.get(f).copied(),
                None => None,
            };
            let Some(src_mod) = src_mod else { continue };

            for &tgt in targets {
                let tgt_node = &self.nodes_arena[tgt];

                // Map target to its owning module: Module-flavored nodes
                // use their name (handles external/stdlib); all others
                // use their filename.
                let tgt_mod: Option<String> = if tgt_node.flavor == Flavor::Module {
                    Some(tgt_node.get_name())
                } else {
                    tgt_node
                        .filename
                        .as_deref()
                        .and_then(|f| filename_to_module.get(f))
                        .map(|s| s.to_string())
                };

                let Some(tgt_mod) = tgt_mod else { continue };
                if tgt_mod == src_mod {
                    continue;
                }

                let src_mid = ensure_module(src_mod, &mut new_nodes);
                let tgt_mid = ensure_module(&tgt_mod, &mut new_nodes);
                module_edges.entry(src_mid).or_default().insert(tgt_mid);
            }
        }

        let defined: HashSet<NodeId> = (0..new_nodes.len()).collect();
        (new_nodes, module_edges, defined)
    }
}
