use super::*;

impl AnalysisSession {
    // =====================================================================
    // Attribute access and value resolution
    // =====================================================================

    pub(super) fn resolve_attribute(&mut self, expr: &ExprAttribute) -> (Option<NodeId>, String) {
        let attr_name = expr.attr.id.to_string();

        match expr.value.as_ref() {
            Expr::Attribute(inner_attr) => {
                let (obj_node, inner_attr_name) = self.resolve_attribute(inner_attr);

                if let Some(obj_id) = obj_node
                    && self.nodes_arena[obj_id].namespace.is_some()
                {
                    let ns = self.nodes_arena[obj_id].get_name(&self.graph.interner);
                    if let Some(val) = self.lookup_in_scope(&ns, &inner_attr_name) {
                        return (Some(val), attr_name);
                    }
                }
                (None, attr_name)
            }
            Expr::Call(call) => self
                .resolve_builtins(call)
                .map_or((None, attr_name.clone()), |result_id| {
                    (Some(result_id), attr_name)
                }),
            _ => {
                let obj_name = get_ast_node_name(&expr.value);
                self.get_value(&obj_name)
                    .map_or((None, attr_name.clone()), |obj_id| {
                        (Some(obj_id), attr_name)
                    })
            }
        }
    }

    pub(super) fn get_obj_ids_for_expr(&mut self, expr: &Expr) -> Vec<NodeId> {
        match expr {
            Expr::Name(n) if n.ctx == ExprContext::Load => {
                self.get_values(n.id.as_ref()).iter().collect()
            }
            Expr::Attribute(a) => {
                let attr_name = a.attr.id.to_string();
                let obj_ids = self.get_obj_ids_for_expr(&a.value);
                let mut results = Vec::new();
                for obj_id in obj_ids {
                    if self.nodes_arena[obj_id].namespace.is_none() {
                        continue;
                    }
                    let ns = self.nodes_arena[obj_id].get_name(&self.graph.interner);
                    let values = self.lookup_values_in_scope(&ns, &attr_name);
                    if values.is_empty() {
                        if let Some(mro) = self.mro.get(&obj_id) {
                            for &base_id in mro.iter().skip(1) {
                                let base_ns =
                                    self.nodes_arena[base_id].get_name(&self.graph.interner);
                                let base_values =
                                    self.lookup_values_in_scope(&base_ns, &attr_name);
                                for id in base_values.iter() {
                                    if !results.contains(&id) {
                                        results.push(id);
                                    }
                                }
                                if !base_values.is_empty() {
                                    break;
                                }
                            }
                        }
                    } else {
                        for id in values.iter() {
                            if !results.contains(&id) {
                                results.push(id);
                            }
                        }
                    }
                }
                results
            }
            Expr::Call(call) => {
                if let Some(id) = self.resolve_builtins(call) {
                    return vec![id];
                }

                let func_ids = self.get_obj_ids_for_expr(&call.func);
                let mut results = Vec::new();
                for &func_id in &func_ids {
                    if self.class_base_ast_info.contains_key(&func_id)
                        && !results.contains(&func_id)
                    {
                        results.push(func_id);
                    }
                    if let Some(ret_ids) = self.function_returns.get(&func_id) {
                        for &ret_id in ret_ids {
                            if !results.contains(&ret_id) {
                                results.push(ret_id);
                            }
                        }
                    }
                }
                results
            }
            Expr::Subscript(node) => {
                let resolved = self.resolve_subscript_value(node);
                if !resolved.values.is_empty() {
                    resolved.values.iter().collect()
                } else {
                    self.get_obj_ids_for_expr(&node.value)
                }
            }
            _ => vec![],
        }
    }

    pub(super) fn lookup_in_scope(&self, ns: &str, name: &str) -> Option<NodeId> {
        self.lookup_values_in_scope(ns, name).first()
    }

    pub(super) fn lookup_values_in_scope(&self, ns: &str, name: &str) -> ValueSet {
        let ns_sym = self.graph.interner.lookup(ns);
        let name_sym = self.graph.interner.lookup(name);
        if let (Some(ns_sym), Some(name_sym)) = (ns_sym, name_sym) {
            if let Some(scope) = self.scopes.get(&ns_sym)
                && let Some(vs) = scope.defs.get(&name_sym)
            {
                return vs.clone();
            }
        }
        ValueSet::empty()
    }

    pub(super) fn lookup_containers_in_scope(&self, ns: &str, name: &str) -> ContainerFacts {
        let ns_sym = self.graph.interner.lookup(ns);
        let name_sym = self.graph.interner.lookup(name);
        if let (Some(ns_sym), Some(name_sym)) = (ns_sym, name_sym) {
            if let Some(scope) = self.scopes.get(&ns_sym)
                && let Some(facts) = scope.containers.get(&name_sym)
            {
                return facts.clone();
            }
        }
        ContainerFacts::default()
    }

    pub(super) fn set_attribute(&mut self, expr: &ExprAttribute, value: Option<NodeId>) -> bool {
        let (obj_node, attr_name) = self.resolve_attribute(expr);

        if let Some(obj_id) = obj_node
            && self.nodes_arena[obj_id].namespace.is_some()
        {
            let ns = self.nodes_arena[obj_id].get_name(&self.graph.interner).to_owned();
            let ns_sym = self.graph.interner.intern(&ns);
            let attr_sym = self.graph.interner.intern(&attr_name);
            if let Some(scope) = self.scopes.get_mut(&ns_sym) {
                if let Some(id) = value {
                    scope.defs.entry(attr_sym).or_default().insert(id);
                } else {
                    scope.defs.entry(attr_sym).or_default();
                }
                return true;
            }
        }
        false
    }

    pub(super) fn set_attribute_shallow_value(
        &mut self,
        expr: &ExprAttribute,
        value: &ShallowValue,
    ) -> bool {
        let (obj_node, attr_name) = self.resolve_attribute(expr);

        if let Some(obj_id) = obj_node
            && self.nodes_arena[obj_id].namespace.is_some()
        {
            let ns = self.nodes_arena[obj_id].get_name(&self.graph.interner).to_owned();
            let ns_sym = self.graph.interner.intern(&ns);
            let attr_sym = self.graph.interner.intern(&attr_name);
            if let Some(scope) = self.scopes.get_mut(&ns_sym) {
                if !value.values.is_empty() {
                    scope
                        .defs
                        .entry(attr_sym)
                        .or_default()
                        .union_with(&value.values);
                } else {
                    scope.defs.entry(attr_sym).or_default();
                }
                if !value.containers.is_empty() {
                    scope
                        .containers
                        .entry(attr_sym)
                        .or_default()
                        .union_with(&value.containers);
                }
                return true;
            }
        }
        false
    }

    pub(super) fn resolve_shallow_value(&mut self, expr: &Expr) -> ShallowValue {
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

                    let ns = self.nodes_arena[obj_id].get_name(&self.graph.interner);
                    let attr_name = node.attr.id.to_string();
                    let direct_values = self.lookup_values_in_scope(&ns, &attr_name);
                    let direct_containers = self.lookup_containers_in_scope(&ns, &attr_name);

                    if direct_values.is_empty() && direct_containers.is_empty() {
                        if let Some(mro) = self.mro.get(&obj_id) {
                            for &base_id in mro.iter().skip(1) {
                                let base_ns =
                                    self.nodes_arena[base_id].get_name(&self.graph.interner);
                                let base_values =
                                    self.lookup_values_in_scope(&base_ns, &attr_name);
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
                    if let Some(ret_ids) = self.function_returns.get(&func_id) {
                        for &ret_id in ret_ids {
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
                let mut items = FxHashMap::default();
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

    pub(super) fn resolve_subscript_value(&mut self, node: &ExprSubscript) -> ShallowValue {
        let container = self.resolve_shallow_value(&node.value);
        let key = literal_key_from_expr(&node.slice);
        container.containers.resolve_subscript(key.as_ref())
    }

    // =====================================================================
    // Builtins and inheritance resolution
    // =====================================================================

    pub(super) fn resolve_builtins_from_call(
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
                let method: &'static str = if name == "str" { "__str__" } else { "__repr__" };
                if let Some(arg) = node.arguments.args.first() {
                    let obj_ids = self.get_obj_ids_for_expr(arg);
                    for obj_id in obj_ids {
                        if self.class_base_ast_info.contains_key(&obj_id) {
                            self.emit_protocol_edges(obj_id, &[method]);
                        }
                    }
                }
                return None;
            }
        }
        None
    }

    pub(super) fn resolve_builtins(&mut self, node: &ExprCall) -> Option<NodeId> {
        if let Expr::Name(ref func_name) = *node.func
            && func_name.id.as_str() == "super"
        {
            return self.resolve_super();
        }
        None
    }

    pub(super) fn resolve_super(&self) -> Option<NodeId> {
        let class_id = self.get_current_class()?;
        let mro = self.mro.get(&class_id)?;
        if mro.len() > 1 { Some(mro[1]) } else { None }
    }

    pub(super) fn resolve_base_classes(&mut self) {
        debug!("Resolving base classes");

        let class_refs: Vec<(NodeId, Vec<BaseClassRef>)> = self
            .class_base_ast_info
            .iter()
            .map(|(&cls_id, refs)| (cls_id, refs.clone()))
            .collect();

        let mut class_base_nodes: FxHashMap<NodeId, Vec<NodeId>> = FxHashMap::default();

        for (cls_id, refs) in &class_refs {
            let mut bases = Vec::new();
            let cls_namespace = self.nodes_arena[*cls_id]
                .namespace
                .map(|s| self.graph.interner.resolve(s).to_owned())
                .unwrap_or_default();

            for base_ref in refs {
                let base_id = match base_ref {
                    BaseClassRef::Name(name) => self.lookup_base_by_name(&cls_namespace, name),
                    BaseClassRef::Attribute(parts) => self.lookup_base_by_attr_parts(parts),
                };

                if let Some(base_id) = base_id
                    && self.nodes_arena[base_id].namespace.is_some()
                {
                    bases.push(base_id);
                }
            }

            class_base_nodes.insert(*cls_id, bases);
        }

        self.class_base_nodes = class_base_nodes;

        debug!("Computing MRO for all analyzed classes");
        self.mro = resolve_mro(&self.class_base_nodes);
    }

    pub(super) fn lookup_base_by_name(&self, enclosing_ns: &str, name: &str) -> Option<NodeId> {
        if let Some(val) = self.lookup_in_scope(enclosing_ns, name) {
            return Some(val);
        }

        let parts: Vec<&str> = enclosing_ns.split('.').collect();
        for i in (0..parts.len()).rev() {
            let ns = parts[..=i].join(".");
            if let Some(val) = self.lookup_in_scope(&ns, name) {
                return Some(val);
            }
        }

        None
    }

    pub(super) fn lookup_base_by_attr_parts(&self, parts: &[String]) -> Option<NodeId> {
        if parts.is_empty() {
            return None;
        }

        let mut current = self.get_value(&parts[0])?;
        for part in parts.iter().skip(1) {
            let ns = self.nodes_arena[current].get_name(&self.graph.interner);
            if let Some(val) = self.lookup_in_scope(&ns, part.as_str()) {
                current = val;
                continue;
            }
            return None;
        }

        Some(current)
    }
}
