use super::*;

impl AnalysisSession {
    /// Gather scope information by walking the cached AST.
    pub(super) fn build_scopes(
        module: &ModModule,
        module_ns: &str,
        interner: &mut Interner,
    ) -> FxHashMap<SymId, ScopeInfo> {
        let mut scopes: FxHashMap<SymId, ScopeInfo> = FxHashMap::default();

        let mut module_scope = ScopeInfo::new();
        Self::collect_scope_defs(&module.body, &mut module_scope, interner);
        let ns_sym = interner.intern(module_ns);
        scopes.insert(ns_sym, module_scope);
        Self::collect_nested_scopes(&module.body, module_ns, &mut scopes, interner);

        scopes
    }

    pub(super) fn merge_scopes(&mut self, scopes: &FxHashMap<SymId, ScopeInfo>) {
        for (&ns, sc) in scopes {
            if let Some(existing) = self.scopes.get_mut(&ns) {
                for (&name, vs) in &sc.defs {
                    existing.defs.entry(name).or_default().union_with(vs);
                }
                for (&name, facts) in &sc.containers {
                    existing
                        .containers
                        .entry(name)
                        .or_default()
                        .union_with(facts);
                }
                if existing.all_exports.is_none() && sc.all_exports.is_some() {
                    existing.all_exports = sc.all_exports.clone();
                }
            } else {
                self.scopes.insert(ns, sc.clone());
            }
        }
    }

    fn collect_scope_defs(stmts: &[Stmt], scope: &mut ScopeInfo, interner: &mut Interner) {
        for stmt in stmts {
            match stmt {
                Stmt::FunctionDef(f) => {
                    let name = interner.intern(f.name.id.as_str());
                    scope.defs.entry(name).or_default();
                    scope.locals.insert(name);
                }
                Stmt::ClassDef(c) => {
                    let name = interner.intern(c.name.id.as_str());
                    scope.defs.entry(name).or_default();
                    scope.locals.insert(name);
                }
                Stmt::Import(imp) => {
                    for alias in &imp.names {
                        let name = if let Some(ref asname) = alias.asname {
                            interner.intern(asname.id.as_str())
                        } else {
                            interner.intern(alias.name.id.as_str())
                        };
                        scope.defs.entry(name).or_default();
                    }
                }
                Stmt::ImportFrom(imp) => {
                    for alias in &imp.names {
                        if alias.name.id.as_str() == "*" {
                            continue;
                        }
                        let name = if let Some(ref asname) = alias.asname {
                            interner.intern(asname.id.as_str())
                        } else {
                            interner.intern(alias.name.id.as_str())
                        };
                        scope.defs.entry(name).or_default();
                    }
                }
                Stmt::Assign(a) => {
                    for target in &a.targets {
                        if let Expr::Name(n) = target
                            && n.id.as_str() == "__all__"
                            && let Some(exports) = extract_all_exports(&a.value, interner)
                        {
                            scope.all_exports = Some(exports);
                        }
                        Self::collect_assign_target_names(target, scope, interner);
                    }
                }
                Stmt::AugAssign(a) => {
                    Self::collect_assign_target_names(&a.target, scope, interner);
                }
                Stmt::AnnAssign(a) => {
                    Self::collect_assign_target_names(&a.target, scope, interner);
                }
                Stmt::For(f) => {
                    Self::collect_assign_target_names(&f.target, scope, interner);
                    Self::collect_scope_defs(&f.body, scope, interner);
                    Self::collect_scope_defs(&f.orelse, scope, interner);
                }
                Stmt::With(w) => {
                    for item in &w.items {
                        if let Some(vars) = &item.optional_vars {
                            Self::collect_assign_target_names(vars, scope, interner);
                        }
                    }
                    Self::collect_scope_defs(&w.body, scope, interner);
                }
                Stmt::Try(s) => {
                    for handler in &s.handlers {
                        let ExceptHandler::ExceptHandler(h) = handler;
                        if let Some(name_ident) = &h.name {
                            let name = interner.intern(name_ident.id.as_str());
                            scope.defs.entry(name).or_default();
                            scope.locals.insert(name);
                        }
                    }
                    Self::collect_scope_defs(&s.body, scope, interner);
                    for handler in &s.handlers {
                        let ExceptHandler::ExceptHandler(h) = handler;
                        Self::collect_scope_defs(&h.body, scope, interner);
                    }
                    Self::collect_scope_defs(&s.orelse, scope, interner);
                    Self::collect_scope_defs(&s.finalbody, scope, interner);
                }
                Stmt::If(s) => {
                    Self::collect_scope_defs(&s.body, scope, interner);
                    for clause in &s.elif_else_clauses {
                        Self::collect_scope_defs(&clause.body, scope, interner);
                    }
                }
                Stmt::While(s) => {
                    Self::collect_scope_defs(&s.body, scope, interner);
                    Self::collect_scope_defs(&s.orelse, scope, interner);
                }
                _ => {}
            }
        }
    }

    fn collect_nested_scopes(
        stmts: &[Stmt],
        parent_ns: &str,
        scopes: &mut FxHashMap<SymId, ScopeInfo>,
        interner: &mut Interner,
    ) {
        for stmt in stmts {
            match stmt {
                Stmt::FunctionDef(f) => {
                    let name = f.name.id.to_string();
                    let ns = format!("{parent_ns}.{name}");
                    let mut scope = ScopeInfo::new();
                    for a in &f.parameters.posonlyargs {
                        let pname = interner.intern(a.parameter.name.id.as_str());
                        scope.defs.entry(pname).or_default();
                        scope.locals.insert(pname);
                    }
                    for a in &f.parameters.args {
                        let pname = interner.intern(a.parameter.name.id.as_str());
                        scope.defs.entry(pname).or_default();
                        scope.locals.insert(pname);
                    }
                    for a in &f.parameters.kwonlyargs {
                        let pname = interner.intern(a.parameter.name.id.as_str());
                        scope.defs.entry(pname).or_default();
                        scope.locals.insert(pname);
                    }
                    if let Some(ref va) = f.parameters.vararg {
                        let pname = interner.intern(va.name.id.as_str());
                        scope.defs.entry(pname).or_default();
                        scope.locals.insert(pname);
                    }
                    if let Some(ref kw) = f.parameters.kwarg {
                        let pname = interner.intern(kw.name.id.as_str());
                        scope.defs.entry(pname).or_default();
                        scope.locals.insert(pname);
                    }

                    Self::collect_scope_defs(&f.body, &mut scope, interner);
                    let ns_sym = interner.intern(&ns);
                    scopes.insert(ns_sym, scope);
                    Self::collect_nested_scopes(&f.body, &ns, scopes, interner);
                }
                Stmt::ClassDef(c) => {
                    let name = c.name.id.to_string();
                    let ns = format!("{parent_ns}.{name}");
                    let mut scope = ScopeInfo::new();
                    Self::collect_scope_defs(&c.body, &mut scope, interner);
                    let ns_sym = interner.intern(&ns);
                    scopes.insert(ns_sym, scope);
                    Self::collect_nested_scopes(&c.body, &ns, scopes, interner);
                }
                Stmt::If(s) => {
                    Self::collect_nested_scopes(&s.body, parent_ns, scopes, interner);
                    for clause in &s.elif_else_clauses {
                        Self::collect_nested_scopes(&clause.body, parent_ns, scopes, interner);
                    }
                }
                Stmt::While(s) => {
                    Self::collect_nested_scopes(&s.body, parent_ns, scopes, interner);
                    Self::collect_nested_scopes(&s.orelse, parent_ns, scopes, interner);
                }
                Stmt::For(s) => {
                    Self::collect_nested_scopes(&s.body, parent_ns, scopes, interner);
                    Self::collect_nested_scopes(&s.orelse, parent_ns, scopes, interner);
                }
                Stmt::With(s) => {
                    Self::collect_nested_scopes(&s.body, parent_ns, scopes, interner);
                }
                Stmt::Try(s) => {
                    Self::collect_nested_scopes(&s.body, parent_ns, scopes, interner);
                    for handler in &s.handlers {
                        let ExceptHandler::ExceptHandler(h) = handler;
                        Self::collect_nested_scopes(&h.body, parent_ns, scopes, interner);
                    }
                    Self::collect_nested_scopes(&s.orelse, parent_ns, scopes, interner);
                    Self::collect_nested_scopes(&s.finalbody, parent_ns, scopes, interner);
                }
                _ => {}
            }
        }
    }

    fn collect_assign_target_names(
        target: &Expr,
        scope: &mut ScopeInfo,
        interner: &mut Interner,
    ) {
        match target {
            Expr::Name(n) => {
                let name = interner.intern(n.id.as_str());
                scope.defs.entry(name).or_default();
                scope.locals.insert(name);
            }
            Expr::Tuple(t) => {
                for elt in &t.elts {
                    Self::collect_assign_target_names(elt, scope, interner);
                }
            }
            Expr::List(l) => {
                for elt in &l.elts {
                    Self::collect_assign_target_names(elt, scope, interner);
                }
            }
            Expr::Starred(s) => {
                Self::collect_assign_target_names(&s.value, scope, interner);
            }
            _ => {}
        }
    }
}
