use super::*;

impl CallGraph {
    /// Analyze a set of Python files and return the resulting call graph.
    pub fn new(filenames: &[String], root: Option<&str>) -> Result<Self> {
        let mut session = AnalysisSession::new(filenames, root);
        session.process()?;
        Ok(session.into_call_graph())
    }
}

impl AnalysisSession {
    pub(super) fn new(filenames: &[String], root: Option<&str>) -> Self {
        let mut interner = Interner::new();
        let mut module_to_filename = FxHashMap::default();
        for filename in filenames {
            let mod_name = get_module_name(filename, root);
            let mod_sym = interner.intern(&mod_name);
            module_to_filename.insert(mod_sym, filename.clone());
        }

        let empty_sym = interner.intern("");

        Self {
            graph: CallGraph {
                interner,
                nodes_arena: Vec::new(),
                nodes_by_name: FxHashMap::default(),
                defines_edges: FxHashMap::default(),
                uses_edges: FxHashMap::default(),
                defined: FxHashSet::default(),
                diagnostics: AnalysisDiagnostics::default(),
                module_to_filename,
            },
            node_ids_by_key: FxHashMap::default(),
            scopes: FxHashMap::default(),
            function_returns: FxHashMap::default(),
            function_returns_changed: false,
            class_base_ast_info: FxHashMap::default(),
            class_base_nodes: FxHashMap::default(),
            mro: FxHashMap::default(),
            filenames: filenames.to_vec(),
            root: root.map(|s| s.to_string()),
            module_name: empty_sym,
            filename: String::new(),
            name_stack: Vec::new(),
            scope_stack: Vec::new(),
            class_stack: Vec::new(),
            context_stack: Vec::new(),
        }
    }

    pub(super) fn into_call_graph(self) -> CallGraph {
        self.graph
    }

    /// Two-pass analysis followed by a fixpoint loop for return-value propagation.
    pub(super) fn process(&mut self) -> Result<()> {
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

        const MAX_PROPAGATION_PASSES: usize = 8;
        for pass_num in 0..MAX_PROPAGATION_PASSES {
            self.function_returns_changed = false;
            for cached_file in &cached_files {
                debug!(
                    "========== propagation pass {}, file '{}' ==========",
                    pass_num + 1,
                    cached_file.filename
                );
                self.process_one(cached_file);
            }
            if !self.function_returns_changed {
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

    fn prepare_files(&mut self) -> Result<Vec<CachedFile>> {
        let mut cached_files = Vec::with_capacity(self.filenames.len());
        for filename in &self.filenames.clone() {
            let content =
                std::fs::read_to_string(filename).with_context(|| format!("reading {filename}"))?;
            let module_name_str = get_module_name(filename, self.root.as_deref());
            let module_name = self.graph.interner.intern(&module_name_str);
            let parsed =
                ruff_python_parser::parse_unchecked(&content, ParseOptions::from(Mode::Module));
            let module = match parsed.into_syntax() {
                Mod::Module(module) => module,
                _ => continue,
            };
            let line_index = LineIndex::from_source_text(&content);
            let scopes = Self::build_scopes(&module, &module_name_str, &mut self.graph.interner);
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
        self.module_name = cached_file.module_name;

        self.visit_module(&cached_file.module, &cached_file.line_index);

        self.module_name = self.graph.interner.intern("");
        self.filename.clear();
    }
}
