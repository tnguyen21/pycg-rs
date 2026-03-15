use crate::FxHashMap;

/// An interned symbol identifier — a cheap, copyable handle to a string.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SymId(u32);

impl std::fmt::Debug for SymId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SymId({})", self.0)
    }
}

/// A string interner: deduplicates strings and hands out cheap `SymId` handles.
#[derive(Debug)]
pub struct Interner {
    map: FxHashMap<String, SymId>,
    vec: Vec<String>,
}

impl Interner {
    pub fn new() -> Self {
        Self {
            map: FxHashMap::default(),
            vec: Vec::new(),
        }
    }

    /// Intern a string, returning its unique `SymId`. Creates a new entry if
    /// the string has not been seen before.
    pub fn intern(&mut self, s: &str) -> SymId {
        if let Some(&id) = self.map.get(s) {
            return id;
        }
        let id = SymId(self.vec.len() as u32);
        self.vec.push(s.to_owned());
        self.map.insert(s.to_owned(), id);
        id
    }

    /// Look up a string without creating a new entry. Returns `None` if the
    /// string has never been interned.
    pub fn lookup(&self, s: &str) -> Option<SymId> {
        self.map.get(s).copied()
    }

    /// Resolve a `SymId` back to its string.
    pub fn resolve(&self, id: SymId) -> &str {
        &self.vec[id.0 as usize]
    }
}

impl Default for Interner {
    fn default() -> Self {
        Self::new()
    }
}
