use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// ValueSet — compact abstract-value set for name bindings
// ---------------------------------------------------------------------------

/// A compact set of abstract values (NodeIds) for a name binding.
///
/// Represents the set of all possible pointees for a name at a given program
/// point.  Designed to stay small in the common case (single binding), so a
/// plain `Vec` with dedup is preferred over a full `HashSet`.
///
/// # Invariants
/// * No duplicate NodeIds are stored.
/// * Order is insertion-order (deterministic, first-wins semantics for
///   `first()`).
#[derive(Debug, Clone, Default)]
pub struct ValueSet(Vec<usize>);

impl ValueSet {
    /// Create an empty set (name declared but unresolved).
    pub fn empty() -> Self {
        Self(Vec::new())
    }

    /// Create a singleton set.
    pub fn singleton(id: usize) -> Self {
        Self(vec![id])
    }

    /// Add a value.  Returns `true` if the set changed (id was not already
    /// present).
    pub fn insert(&mut self, id: usize) -> bool {
        if !self.0.contains(&id) {
            self.0.push(id);
            true
        } else {
            false
        }
    }

    /// Union `other` into `self`.  Returns `true` if `self` changed.
    pub fn union_with(&mut self, other: &ValueSet) -> bool {
        let mut changed = false;
        for &id in &other.0 {
            changed |= self.insert(id);
        }
        changed
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// First value (insertion-order), if any.  Used as a backward-compat
    /// fallback by callers that only need one resolved target.
    pub fn first(&self) -> Option<usize> {
        self.0.first().copied()
    }

    /// Iterator over all values.
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.0.iter().copied()
    }

    /// All values as a slice.
    pub fn as_slice(&self) -> &[usize] {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// Scope — lexical scope with string-keyed bindings (used by the public API)
// ---------------------------------------------------------------------------

/// Tracks name bindings within a lexical scope.
#[derive(Debug, Clone)]
pub struct Scope {
    /// The fully qualified name of this scope (e.g., "module.Class.method").
    pub name: String,
    /// Names defined (bound) in this scope.
    pub defs: HashMap<String, Option<String>>,
    /// Names that are local-only (assigned in this scope).
    pub locals: HashSet<String>,
}

impl Scope {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            defs: HashMap::new(),
            locals: HashSet::new(),
        }
    }

    /// Bind a name in this scope, optionally to a fully qualified target.
    pub fn bind(&mut self, name: &str, target: Option<&str>) {
        self.defs
            .insert(name.to_string(), target.map(|s| s.to_string()));
        self.locals.insert(name.to_string());
    }

    /// Look up a name in this scope.
    pub fn get(&self, name: &str) -> Option<&Option<String>> {
        self.defs.get(name)
    }

    /// Check if a name is defined in this scope.
    pub fn has(&self, name: &str) -> bool {
        self.defs.contains_key(name)
    }
}

/// A stack of scopes for lexical name resolution.
#[derive(Debug)]
pub struct ScopeStack {
    scopes: Vec<Scope>,
}

impl Default for ScopeStack {
    fn default() -> Self {
        Self::new()
    }
}

impl ScopeStack {
    pub fn new() -> Self {
        Self { scopes: Vec::new() }
    }

    pub fn push(&mut self, scope: Scope) {
        self.scopes.push(scope);
    }

    pub fn pop(&mut self) -> Option<Scope> {
        self.scopes.pop()
    }

    pub fn current(&self) -> Option<&Scope> {
        self.scopes.last()
    }

    pub fn current_mut(&mut self) -> Option<&mut Scope> {
        self.scopes.last_mut()
    }

    /// Look up a name by walking scopes from innermost to outermost.
    /// Returns the fully qualified target if bound, or None.
    pub fn resolve(&self, name: &str) -> Option<String> {
        for scope in self.scopes.iter().rev() {
            if let Some(target) = scope.defs.get(name) {
                return target.clone();
            }
        }
        None
    }

    /// Check if a name is defined in any scope.
    pub fn is_defined(&self, name: &str) -> bool {
        self.scopes.iter().rev().any(|s| s.defs.contains_key(name))
    }

    /// Get the current namespace (fully qualified scope name).
    pub fn current_namespace(&self) -> String {
        self.scopes
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(".")
    }

    pub fn depth(&self) -> usize {
        self.scopes.len()
    }
}
