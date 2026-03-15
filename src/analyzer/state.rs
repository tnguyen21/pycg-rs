use super::*;

impl AnalysisSession {
    pub(super) fn record_external_reference(
        &mut self,
        source_id: NodeId,
        kind: ExternalReferenceKind,
        canonical_name: String,
    ) {
        let source = &self.nodes_arena[source_id];
        let diagnostic = ExternalReferenceDiagnostic {
            source_canonical_name: source.get_name(&self.graph.interner).to_string(),
            source_filename: source.filename.clone(),
            source_line: source.line,
            kind,
            canonical_name,
        };
        if !self
            .graph
            .diagnostics
            .external_references
            .contains(&diagnostic)
        {
            self.graph.diagnostics.external_references.push(diagnostic);
        }
    }

    pub(super) fn get_node(
        &mut self,
        namespace: Option<&str>,
        name: &str,
        flavor: Flavor,
    ) -> NodeId {
        let ns_sym = namespace.map(|s| self.graph.interner.intern(s));
        let name_sym = self.graph.interner.intern(name);
        let key = NodeKey {
            namespace: ns_sym,
            name: name_sym,
        };
        if let Some(&id) = self.node_ids_by_key.get(&key) {
            let n = &self.nodes_arena[id];
            if flavor.specificity() > n.flavor.specificity() {
                self.nodes_arena[id].flavor = flavor;
            }
            return id;
        }

        let filename = if let Some(ns) = namespace {
            let ns_sym = self.graph.interner.intern(ns);
            if let Some(f) = self.module_to_filename.get(&ns_sym) {
                Some(f.clone())
            } else {
                Some(self.filename.clone())
            }
        } else {
            Some(self.filename.clone())
        };

        let fqn_sym = match ns_sym {
            Some(ns) => {
                let ns_str = self.graph.interner.resolve(ns);
                if !ns_str.is_empty() {
                    let fqn = format!("{ns_str}.{name}");
                    self.graph.interner.intern(&fqn)
                } else {
                    name_sym
                }
            }
            None => name_sym,
        };
        let mut node = Node::new(ns_sym, name_sym, fqn_sym, flavor);
        node.filename = filename;
        let id = self.nodes_arena.len();
        if ns_sym.is_none() {
            self.defined.insert(id);
        }

        self.nodes_arena.push(node);
        self.node_ids_by_key.insert(key, id);
        self.nodes_by_name.entry(name_sym).or_default().push(id);
        id
    }

    /// Push a name onto the name_stack and update the FQN cache.
    pub(super) fn push_name(&mut self, name: SymId) {
        self.name_stack.push(name);
        let fqn = if self.name_stack.len() == 1 {
            name
        } else {
            let prev_fqn = *self.fqn_cache.last().unwrap();
            let prev_str = self.graph.interner.resolve(prev_fqn);
            let name_str = self.graph.interner.resolve(name);
            let joined = format!("{prev_str}.{name_str}");
            self.graph.interner.intern(&joined)
        };
        self.fqn_cache.push(fqn);
    }

    /// Pop a name from the name_stack and FQN cache.
    pub(super) fn pop_name(&mut self) {
        self.name_stack.pop();
        self.fqn_cache.pop();
    }

    /// Get or create a node by pre-interned SymIds. Avoids re-interning.
    pub(super) fn get_node_by_sym(
        &mut self,
        namespace: Option<SymId>,
        name: SymId,
        flavor: Flavor,
    ) -> NodeId {
        let key = NodeKey {
            namespace,
            name,
        };
        if let Some(&id) = self.node_ids_by_key.get(&key) {
            let n = &self.nodes_arena[id];
            if flavor.specificity() > n.flavor.specificity() {
                self.nodes_arena[id].flavor = flavor;
            }
            return id;
        }

        let filename = if let Some(ns_sym) = namespace {
            if let Some(f) = self.module_to_filename.get(&ns_sym) {
                Some(f.clone())
            } else {
                Some(self.filename.clone())
            }
        } else {
            Some(self.filename.clone())
        };

        let fqn = match namespace {
            Some(ns) => {
                let ns_str = self.graph.interner.resolve(ns);
                if !ns_str.is_empty() {
                    let name_str = self.graph.interner.resolve(name);
                    let fqn_str = format!("{ns_str}.{name_str}");
                    self.graph.interner.intern(&fqn_str)
                } else {
                    name
                }
            }
            None => name,
        };
        let mut node = Node::new(namespace, name, fqn, flavor);
        node.filename = filename;
        let id = self.nodes_arena.len();
        if namespace.is_none() {
            self.defined.insert(id);
        }

        self.nodes_arena.push(node);
        self.node_ids_by_key.insert(key, id);
        self.nodes_by_name.entry(name).or_default().push(id);
        id
    }

    pub(super) fn get_node_of_current_namespace(&mut self) -> NodeId {
        assert!(!self.name_stack.is_empty());
        let len = self.fqn_cache.len();
        let ns_sym = if len > 1 {
            Some(self.fqn_cache[len - 2])
        } else {
            Some(self.graph.interner.intern(""))
        };
        let name_sym = *self.name_stack.last().unwrap();
        self.get_node_by_sym(ns_sym, name_sym, Flavor::Namespace)
    }

    pub(super) fn get_parent_node(&mut self, node_id: NodeId) -> NodeId {
        let node = &self.nodes_arena[node_id];
        let (ns, name) = if let Some(ns_sym) = node.namespace {
            let ns_str = self.graph.interner.resolve(ns_sym).to_owned();
            if ns_str.contains('.') {
                let (parent_ns, parent_name) = ns_str
                    .rsplit_once('.')
                    .expect("namespace contains '.' (checked above)");
                (parent_ns.to_string(), parent_name.to_string())
            } else {
                (String::new(), ns_str)
            }
        } else {
            (String::new(), String::new())
        };
        self.get_node(Some(&ns), &name, Flavor::Namespace)
    }

    pub(super) fn associate_node(&mut self, node_id: NodeId, filename: &str, line: usize) {
        self.nodes_arena[node_id].filename = Some(filename.to_string());
        self.nodes_arena[node_id].line = Some(line);
    }

    pub(super) fn record_function_return(&mut self, fn_node: NodeId, ret_id: NodeId) -> bool {
        let changed = self
            .function_returns
            .entry(fn_node)
            .or_default()
            .insert(ret_id);
        self.function_returns_changed |= changed;
        changed
    }

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
            let to_name = self.nodes_arena[to_id].name;
            let to_ns = self.nodes_arena[to_id].namespace;
            if to_ns.is_some() {
                self.remove_wild(from_id, to_id, to_name);
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

    pub(super) fn remove_wild(&mut self, from_id: NodeId, to_id: NodeId, name_sym: SymId) {
        let name_str = self.graph.interner.resolve(name_sym);
        if name_str.is_empty() {
            return;
        }
        let Some(edges) = self.uses_edges.get(&from_id) else {
            return;
        };

        let to_name_str = self.nodes_arena[to_id].get_name(&self.graph.interner);
        if to_name_str.contains("^^^argument^^^") || to_id == from_id {
            return;
        }

        let wild = edges
            .iter()
            .find(|&&eid| {
                let n = &self.nodes_arena[eid];
                n.namespace.is_none() && n.name == name_sym
            })
            .copied();

        if let Some(wild_id) = wild {
            info!(
                "Use from {} to {} resolves {}; removing wildcard",
                self.nodes_arena[from_id].get_name(&self.graph.interner),
                self.nodes_arena[to_id].get_name(&self.graph.interner),
                self.nodes_arena[wild_id].get_name(&self.graph.interner)
            );
            self.remove_uses_edge(from_id, wild_id);
        }
    }

    pub(super) fn get_value(&self, name: &str) -> Option<NodeId> {
        self.get_values(name).first()
    }

    pub(super) fn get_values(&self, name: &str) -> ValueSet {
        let Some(name_sym) = self.graph.interner.lookup(name) else {
            return ValueSet::empty();
        };
        for scope_key in self.scope_stack.iter().rev() {
            if let Some(scope) = self.scopes.get(scope_key)
                && let Some(vs) = scope.defs.get(&name_sym)
            {
                return vs.clone();
            }
        }
        ValueSet::empty()
    }

    pub(super) fn get_containers(&self, name: &str) -> ContainerFacts {
        let Some(name_sym) = self.graph.interner.lookup(name) else {
            return ContainerFacts::default();
        };
        for scope_key in self.scope_stack.iter().rev() {
            if let Some(scope) = self.scopes.get(scope_key)
                && let Some(facts) = scope.containers.get(&name_sym)
            {
                return facts.clone();
            }
        }
        ContainerFacts::default()
    }

    pub(super) fn set_value(&mut self, name: &str, value: Option<NodeId>) {
        let name_sym = self.graph.interner.intern(name);
        // First pass: find existing scope with this name
        let found_scope_key = self
            .scope_stack
            .iter()
            .rev()
            .find(|scope_key| {
                self.scopes
                    .get(*scope_key)
                    .is_some_and(|scope| scope.defs.contains_key(&name_sym))
            })
            .copied();

        if let Some(scope_key) = found_scope_key {
            let scope = self
                .scopes
                .get_mut(&scope_key)
                .expect("scope confirmed to exist above");
            if let Some(id) = value {
                scope.defs.entry(name_sym).or_default().insert(id);
            }
            return;
        }

        if let Some(&scope_key) = self.scope_stack.last() {
            if let Some(scope) = self.scopes.get_mut(&scope_key) {
                if let Some(id) = value {
                    scope.defs.entry(name_sym).or_default().insert(id);
                } else {
                    scope.defs.entry(name_sym).or_default();
                }
            }
        }
    }

    pub(super) fn set_containers(&mut self, name: &str, containers: &ContainerFacts) {
        if containers.is_empty() {
            return;
        }

        let name_sym = self.graph.interner.intern(name);
        let found_scope_key = self
            .scope_stack
            .iter()
            .rev()
            .find(|scope_key| {
                self.scopes
                    .get(*scope_key)
                    .is_some_and(|scope| scope.defs.contains_key(&name_sym))
            })
            .copied();

        if let Some(scope_key) = found_scope_key {
            let scope = self
                .scopes
                .get_mut(&scope_key)
                .expect("scope confirmed to exist above");
            scope
                .containers
                .entry(name_sym)
                .or_default()
                .union_with(containers);
            return;
        }

        if let Some(&scope_key) = self.scope_stack.last() {
            if let Some(scope) = self.scopes.get_mut(&scope_key) {
                scope
                    .containers
                    .entry(name_sym)
                    .or_default()
                    .union_with(containers);
            }
        }
    }

    pub(super) fn is_local(&self, name: &str) -> bool {
        let Some(name_sym) = self.graph.interner.lookup(name) else {
            return false;
        };
        if let Some(&scope_key) = self.scope_stack.last()
            && let Some(scope) = self.scopes.get(&scope_key)
        {
            return scope.locals.contains(&name_sym);
        }
        false
    }

    pub(super) fn get_current_class(&self) -> Option<NodeId> {
        self.class_stack.last().copied()
    }
}
