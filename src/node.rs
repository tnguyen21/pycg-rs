use crate::intern::{Interner, SymId};

/// The flavor (kind) of a graph node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Flavor {
    Unspecified,
    Unknown,
    Namespace,
    Attribute,
    Name,
    ImportedItem,
    Module,
    Class,
    Function,
    Method,
    StaticMethod,
    ClassMethod,
}

impl Flavor {
    /// More specific flavors should overwrite less specific ones.
    pub fn specificity(self) -> u8 {
        match self {
            Flavor::Unspecified => 0,
            Flavor::Unknown => 1,
            Flavor::Namespace => 2,
            Flavor::Attribute => 3,
            Flavor::Name => 4,
            Flavor::ImportedItem => 5,
            Flavor::Module => 6,
            Flavor::Class => 7,
            Flavor::Function => 8,
            Flavor::Method => 9,
            Flavor::StaticMethod => 10,
            Flavor::ClassMethod => 11,
        }
    }
}

impl std::fmt::Display for Flavor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Flavor::Unspecified => "unspecified",
            Flavor::Unknown => "unknown",
            Flavor::Namespace => "namespace",
            Flavor::Attribute => "attribute",
            Flavor::Name => "name",
            Flavor::ImportedItem => "importeditem",
            Flavor::Module => "module",
            Flavor::Class => "class",
            Flavor::Function => "function",
            Flavor::Method => "method",
            Flavor::StaticMethod => "staticmethod",
            Flavor::ClassMethod => "classmethod",
        };
        write!(f, "{s}")
    }
}

/// A node in the call graph.
#[derive(Debug, Clone)]
pub struct Node {
    /// The namespace (dotted path) this node belongs to, or None for wildcard.
    pub namespace: Option<SymId>,
    /// The short name of this node.
    pub name: SymId,
    /// Cached fully-qualified name ("namespace.name"), interned once at creation.
    pub fqn: SymId,
    /// The flavor of this node.
    pub flavor: Flavor,
    /// The filename where this node is defined.
    pub filename: Option<String>,
    /// The line number where this node is defined.
    pub line: Option<usize>,
}

impl Node {
    pub fn new(namespace: Option<SymId>, name: SymId, fqn: SymId, flavor: Flavor) -> Self {
        Self {
            namespace,
            name,
            fqn,
            flavor,
            filename: None,
            line: None,
        }
    }

    pub fn with_location(mut self, filename: &str, line: usize) -> Self {
        self.filename = Some(filename.to_string());
        self.line = Some(line);
        self
    }

    /// Get the fully qualified name — zero allocation, just a lookup.
    pub fn get_name<'a>(&self, interner: &'a Interner) -> &'a str {
        interner.resolve(self.fqn)
    }

    /// Get the short name for display.
    pub fn get_short_name<'a>(&self, interner: &'a Interner) -> &'a str {
        interner.resolve(self.name)
    }
}

impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        self.namespace == other.namespace && self.name == other.name
    }
}

impl Eq for Node {}

impl std::hash::Hash for Node {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.namespace.hash(state);
        self.name.hash(state);
    }
}

/// A unique identifier for a node in the graph, used as index.
pub type NodeId = usize;
