pub mod analyzer;
pub mod intern;
pub mod node;
pub mod query;
pub mod scope;
pub mod visgraph;
pub mod writer;

/// Project-wide hash map/set aliases using FxHash (much faster than default SipHash).
pub type FxHashMap<K, V> = std::collections::HashMap<K, V, rustc_hash::FxBuildHasher>;
pub type FxHashSet<V> = std::collections::HashSet<V, rustc_hash::FxBuildHasher>;
