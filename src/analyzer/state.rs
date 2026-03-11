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
            source_canonical_name: source.get_name(),
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
        let key = NodeKey::new(namespace, name);
        if let Some(&id) = self.node_ids_by_key.get(&key) {
            let n = &self.nodes_arena[id];
            if flavor.specificity() > n.flavor.specificity() {
                self.nodes_arena[id].flavor = flavor;
            }
            return id;
        }

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
        if namespace.is_none() {
            self.defined.insert(id);
        }

        self.nodes_arena.push(node);
        self.node_ids_by_key.insert(key, id);
        self.nodes_by_name
            .entry(name.to_string())
            .or_default()
            .push(id);
        id
    }

    pub(super) fn get_node_of_current_namespace(&mut self) -> NodeId {
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

    pub(super) fn remove_wild(&mut self, from_id: NodeId, to_id: NodeId, name: &str) {
        if name.is_empty() {
            return;
        }
        let Some(edges) = self.uses_edges.get(&from_id) else {
            return;
        };

        let to_name = &self.nodes_arena[to_id].get_name();
        if to_name.contains("^^^argument^^^") || to_id == from_id {
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

    pub(super) fn get_value(&self, name: &str) -> Option<NodeId> {
        self.get_values(name).first()
    }

    pub(super) fn get_values(&self, name: &str) -> ValueSet {
        for scope_key in self.scope_stack.iter().rev() {
            if let Some(scope) = self.scopes.get(scope_key)
                && let Some(vs) = scope.defs.get(name)
            {
                return vs.clone();
            }
        }
        ValueSet::empty()
    }

    pub(super) fn get_containers(&self, name: &str) -> ContainerFacts {
        for scope_key in self.scope_stack.iter().rev() {
            if let Some(scope) = self.scopes.get(scope_key)
                && let Some(facts) = scope.containers.get(name)
            {
                return facts.clone();
            }
        }
        ContainerFacts::default()
    }

    pub(super) fn set_value(&mut self, name: &str, value: Option<NodeId>) {
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
                return;
            }
        }

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

    pub(super) fn set_containers(&mut self, name: &str, containers: &ContainerFacts) {
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

    pub(super) fn is_local(&self, name: &str) -> bool {
        if let Some(scope_key) = self.scope_stack.last()
            && let Some(scope) = self.scopes.get(scope_key)
        {
            return scope.locals.contains(name);
        }
        false
    }

    pub(super) fn get_current_class(&self) -> Option<NodeId> {
        self.class_stack.last().copied()
    }
}
