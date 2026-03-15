use crate::{FxHashMap, FxHashSet};

// log::info is used transitively via add_uses_edge/add_defines_edge in super

use crate::node::{Flavor, Node, NodeId};

impl super::AnalysisSession {
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
    fn expand_unknowns(&mut self) {
        // Build index of (from_id, short_name SymId) pairs that already have a
        // concrete (namespaced) uses edge.
        let mut concrete_uses_pairs: FxHashSet<(NodeId, super::SymId)> = FxHashSet::default();
        for (&from, targets) in &self.uses_edges {
            for &to in targets {
                if self.nodes_arena[to].namespace.is_some() {
                    concrete_uses_pairs.insert((from, self.nodes_arena[to].name));
                }
            }
        }

        // Collect new defines edges
        let mut new_defines: Vec<(NodeId, NodeId)> = Vec::new();
        for (&from, targets) in &self.defines_edges {
            for &to in targets {
                if self.nodes_arena[to].namespace.is_none() {
                    let name_sym = self.nodes_arena[to].name;
                    if let Some(ids) = self.nodes_by_name.get(&name_sym) {
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
                    let name_sym = self.nodes_arena[to].name;
                    let name_str = self.graph.interner.resolve(name_sym);
                    if name_str.starts_with('_') {
                        continue;
                    }
                    if concrete_uses_pairs.contains(&(from, name_sym)) {
                        continue;
                    }
                    if let Some(ids) = self.nodes_by_name.get(&name_sym) {
                        for &candidate in ids {
                            if let Some(ns_sym) = self.nodes_arena[candidate].namespace {
                                let ns_str = self.graph.interner.resolve(ns_sym);
                                if !ns_str.is_empty() {
                                    if let Some(scope) = self.scopes.get(&ns_sym)
                                        && let Some(ref exports) = scope.all_exports
                                        && !exports.contains(&name_sym)
                                    {
                                        continue;
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
        let unknown_ids: Vec<NodeId> = self
            .nodes_by_name
            .values()
            .flat_map(|ids| ids.iter().copied())
            .filter(|&id| self.nodes_arena[id].namespace.is_none())
            .collect();
        for id in unknown_ids {
            self.defined.remove(&id);
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

        let mut import_mapping: FxHashMap<NodeId, NodeId> = FxHashMap::default();
        let mut to_resolve: Vec<NodeId> = import_nodes;

        while let Some(from_id) = to_resolve.pop() {
            if import_mapping.contains_key(&from_id) {
                continue;
            }

            let mod_name = {
                let ns_sym = self.nodes_arena[from_id].namespace;
                match ns_sym {
                    Some(s) => self.graph.interner.resolve(s).to_owned(),
                    None => String::new(),
                }
            };
            let item_name = {
                let name_sym = self.nodes_arena[from_id].name;
                self.graph.interner.resolve(name_sym).to_owned()
            };

            // Strategy 1: scope lookup in the source module.
            let scope_vals: Vec<NodeId> = self
                .lookup_values_in_scope(&mod_name, &item_name)
                .iter()
                .collect();
            if !scope_vals.is_empty() {
                let best = scope_vals
                    .iter()
                    .find(|&&id| self.nodes_arena[id].flavor != Flavor::ImportedItem)
                    .or_else(|| scope_vals.first())
                    .copied();
                if let Some(target) = best {
                    import_mapping.insert(from_id, target);
                    if self.nodes_arena[target].flavor == Flavor::ImportedItem && target != from_id
                    {
                        to_resolve.push(target);
                    }
                    continue;
                }
            }

            // Strategy 2: uses-edge walk
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
            let module_id = {
                let ns_sym = self.nodes_arena[to_id].namespace;
                let ns_str = ns_sym
                    .map(|s| self.graph.interner.resolve(s))
                    .unwrap_or("");
                if ns_str == "" && ns_sym.is_some() {
                    to_id
                } else {
                    let ns_owned = ns_sym
                        .map(|s| self.graph.interner.resolve(s).to_owned())
                        .unwrap_or_default();
                    self.get_node(Some(""), &ns_owned, Flavor::Namespace)
                }
            };

            if let Some(module_uses) = self.uses_edges.get(&module_id).cloned() {
                let from_name = self.nodes_arena[from_id].name;
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

            let old_uses: Vec<(NodeId, FxHashSet<NodeId>)> = self.uses_edges.drain().collect();
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

            let old_defines: Vec<(NodeId, FxHashSet<NodeId>)> =
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
        let mut to_contract: Vec<(NodeId, NodeId)> = Vec::new();

        for (&from, targets) in &self.uses_edges {
            for &to in targets {
                if self.nodes_arena[to].namespace.is_some() && !self.defined.contains(&to) {
                    to_contract.push((from, to));
                }
            }
        }

        for (from, to) in to_contract {
            let external_kind = match self.nodes_arena[to].flavor {
                Flavor::ImportedItem => Some(super::ExternalReferenceKind::Import),
                Flavor::Module => Some(super::ExternalReferenceKind::Module),
                _ => None,
            };
            if let Some(kind) = external_kind {
                let canonical = self.nodes_arena[to].get_name(&self.graph.interner);
                self.record_external_reference(from, kind, canonical);
            }
            let name_str = self.graph.interner.resolve(self.nodes_arena[to].name).to_owned();
            let wild_id = self.get_node(None, &name_str, Flavor::Unknown);
            self.defined.remove(&wild_id);
            self.add_uses_edge(from, wild_id);
            self.remove_uses_edge(from, to);
        }
    }

    /// Remove inherited edges.
    fn cull_inherited(&mut self) {
        let mut removed: Vec<(NodeId, NodeId)> = Vec::new();

        let uses_snapshot: Vec<(NodeId, FxHashSet<NodeId>)> = self
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
                    let to_name = self.nodes_arena[to].name;
                    let other_name = self.nodes_arena[other].name;
                    let to_ns = self.nodes_arena[to].namespace;
                    let other_ns = self.nodes_arena[other].namespace;

                    if to_name == other_name
                        && to_ns.is_some()
                        && other_ns.is_some()
                        && to_ns != other_ns
                    {
                        let parent_to = self.get_parent_node(to);
                        let parent_other = self.get_parent_node(other);
                        if let Some(parent_other_uses) = self.uses_edges.get(&parent_other)
                            && parent_other_uses.contains(&parent_to)
                        {
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
            let label_sym = self.graph.interner.intern(label);
            if let Some(ids) = self.nodes_by_name.get(&label_sym).cloned() {
                for id in ids {
                    let parent_id = self.get_parent_node(id);

                    if let Some(inner_uses) = self.uses_edges.get(&id).cloned() {
                        for target in inner_uses {
                            self.add_uses_edge(parent_id, target);
                        }
                    }

                    self.defined.remove(&id);
                }
            }
        }
    }
}

impl super::CallGraph {
    /// Derive a module-level dependency graph.
    pub fn derive_module_graph(
        &mut self,
    ) -> (Vec<Node>, FxHashMap<NodeId, FxHashSet<NodeId>>, FxHashSet<NodeId>) {
        // Build filename -> module name mapping, owning the strings to avoid
        // borrowing interner across the entire function.
        let filename_to_module: FxHashMap<String, String> = self
            .module_to_filename
            .iter()
            .map(|(&m, f)| (f.clone(), self.interner.resolve(m).to_owned()))
            .collect();

        let mut module_ids: FxHashMap<String, NodeId> = FxHashMap::default();
        let mut new_nodes: Vec<Node> = Vec::new();

        let mut ensure_module =
            |name: &str, nodes: &mut Vec<Node>, interner: &mut super::Interner| -> NodeId {
                if let Some(&id) = module_ids.get(name) {
                    return id;
                }
                let id = nodes.len();
                let ns_sym = interner.intern("");
                let name_sym = interner.intern(name);
                nodes.push(Node::new(Some(ns_sym), name_sym, Flavor::Module));
                module_ids.insert(name.to_string(), id);
                id
            };

        let mod_entries: Vec<(String, String)> = self
            .module_to_filename
            .iter()
            .map(|(&m, f)| (self.interner.resolve(m).to_owned(), f.clone()))
            .collect();
        for (mod_name, filename) in &mod_entries {
            let id = ensure_module(mod_name, &mut new_nodes, &mut self.interner);
            new_nodes[id].filename = Some(filename.clone());
        }

        let mut module_edges: FxHashMap<NodeId, FxHashSet<NodeId>> = FxHashMap::default();

        for (&src, targets) in &self.uses_edges {
            let src_node = &self.nodes_arena[src];
            let src_mod = match src_node.filename.as_deref() {
                Some(f) => filename_to_module.get(f).map(|s| s.as_str()),
                None => None,
            };
            let Some(src_mod) = src_mod else { continue };

            for &tgt in targets {
                let tgt_node = &self.nodes_arena[tgt];

                let tgt_mod: Option<String> = if tgt_node.flavor == Flavor::Module {
                    Some(tgt_node.get_name(&self.interner))
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

                let src_mid = ensure_module(src_mod, &mut new_nodes, &mut self.interner);
                let tgt_mid = ensure_module(&tgt_mod, &mut new_nodes, &mut self.interner);
                module_edges.entry(src_mid).or_default().insert(tgt_mid);
            }
        }

        let defined: FxHashSet<NodeId> = (0..new_nodes.len()).collect();
        (new_nodes, module_edges, defined)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{AnalysisSession, ScopeInfo};

    use crate::node::Flavor;

    #[test]
    fn expand_unknowns_replaces_wildcard_uses_with_concrete_candidates() {
        let mut session = AnalysisSession::new(&[], None);
        let caller = session.get_node(Some("pkg"), "caller", Flavor::Function);
        let unknown = session.get_node(None, "work", Flavor::Unknown);
        let worker_a = session.get_node(Some("pkg.WorkerA"), "work", Flavor::Method);
        let worker_b = session.get_node(Some("pkg.WorkerB"), "work", Flavor::Method);

        session.add_uses_edge(caller, unknown);
        session.expand_unknowns();

        let targets = session.uses_edges.get(&caller).expect("caller uses edges");
        assert!(targets.contains(&worker_a));
        assert!(targets.contains(&worker_b));
        assert!(
            !targets.contains(&unknown),
            "wildcard edge should be replaced by concrete candidates",
        );
        assert!(
            !session.defined.contains(&unknown),
            "wildcard nodes should be marked undefined after expansion",
        );
    }

    #[test]
    fn resolve_imports_remaps_scope_backed_imported_items() {
        let mut session = AnalysisSession::new(&[], None);
        let caller = session.get_node(Some("app"), "caller", Flavor::Function);
        let imported = session.get_node(Some("lib"), "item", Flavor::ImportedItem);
        let concrete = session.get_node(Some("lib.impl"), "item", Flavor::Function);

        let lib_sym = session.graph.interner.intern("lib");
        let item_sym = session.graph.interner.intern("item");
        let mut scope = ScopeInfo::new();
        scope.defs.entry(item_sym).or_default().insert(concrete);
        session.scopes.insert(lib_sym, scope);
        session.add_uses_edge(caller, imported);

        session.resolve_imports();

        let targets = session.uses_edges.get(&caller).expect("caller uses edges");
        assert!(targets.contains(&concrete));
        assert!(
            !targets.contains(&imported),
            "import placeholder should be remapped to the concrete target",
        );
    }

    #[test]
    fn contract_nonexistents_records_external_references_before_wildcarding() {
        let mut session = AnalysisSession::new(&[], None);
        let caller = session.get_node(Some("app"), "caller", Flavor::Function);
        session.associate_node(caller, "app.py", 12);
        let external = session.get_node(Some("numpy"), "array", Flavor::ImportedItem);
        session.add_uses_edge(caller, external);

        session.contract_nonexistents();

        assert_eq!(session.graph.diagnostics.external_references.len(), 1);
        let diagnostic = &session.graph.diagnostics.external_references[0];
        assert_eq!(diagnostic.source_canonical_name, "app.caller");
        assert_eq!(diagnostic.source_filename.as_deref(), Some("app.py"));
        assert_eq!(diagnostic.source_line, Some(12));
        assert_eq!(diagnostic.kind.as_str(), "import");
        assert_eq!(diagnostic.canonical_name, "numpy.array");
    }

    #[test]
    fn cull_inherited_removes_redundant_parent_method_edges() {
        let mut session = AnalysisSession::new(&[], None);
        let caller = session.get_node(Some("pkg"), "caller", Flavor::Function);
        let parent = session.get_node(Some("pkg"), "Parent", Flavor::Class);
        let child = session.get_node(Some("pkg"), "Child", Flavor::Class);
        let parent_method = session.get_node(Some("pkg.Parent"), "shared", Flavor::Method);
        let child_method = session.get_node(Some("pkg.Child"), "shared", Flavor::Method);

        session.add_uses_edge(child, parent);
        session.add_uses_edge(caller, parent_method);
        session.add_uses_edge(caller, child_method);

        session.cull_inherited();

        let targets = session.uses_edges.get(&caller).expect("caller uses edges");
        assert!(
            !targets.contains(&parent_method),
            "redundant inherited edge should be removed",
        );
        assert!(targets.contains(&child_method));
    }
}
