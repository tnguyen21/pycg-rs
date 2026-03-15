//! Format-agnostic visual representation of the call graph.
//!
//! Converts the raw call graph (node arena + edge maps) into [`VisualGraph`],
//! which writers can render to DOT, TGF, plain text, etc.

use crate::intern::Interner;
use crate::node::{Node, NodeId};
use crate::{FxHashMap, FxHashSet};
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Color helpers
// ---------------------------------------------------------------------------

/// Convert HLS (hue, lightness, saturation) to RGB, each in [0.0, 1.0].
///
/// This mirrors Python's `colorsys.hls_to_rgb`.
pub fn hls_to_rgb(h: f64, l: f64, s: f64) -> (f64, f64, f64) {
    if s == 0.0 {
        return (l, l, l);
    }

    let m2 = if l <= 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let m1 = 2.0 * l - m2;

    fn channel(m1: f64, m2: f64, mut hue: f64) -> f64 {
        hue = hue.rem_euclid(1.0);
        if hue < 1.0 / 6.0 {
            m1 + (m2 - m1) * hue * 6.0
        } else if hue < 0.5 {
            m2
        } else if hue < 2.0 / 3.0 {
            m1 + (m2 - m1) * (2.0 / 3.0 - hue) * 6.0
        } else {
            m1
        }
    }

    let r = channel(m1, m2, h + 1.0 / 3.0);
    let g = channel(m1, m2, h);
    let b = channel(m1, m2, h - 1.0 / 3.0);
    (r, g, b)
}

/// Format floating-point RGBA values (each in [0.0, 1.0]) as `#rrggbbaa`.
pub fn rgba_hex(r: f64, g: f64, b: f64, a: f64) -> String {
    let ri = (255.0 * r) as u8;
    let gi = (255.0 * g) as u8;
    let bi = (255.0 * b) as u8;
    let ai = (255.0 * a) as u8;
    format!("#{ri:02x}{gi:02x}{bi:02x}{ai:02x}")
}

/// Format floating-point RGB values (each in [0.0, 1.0]) as `#rrggbb`.
pub fn rgb_hex(r: f64, g: f64, b: f64) -> String {
    let ri = (255.0 * r) as u8;
    let gi = (255.0 * g) as u8;
    let bi = (255.0 * b) as u8;
    format!("#{ri:02x}{gi:02x}{bi:02x}")
}

// ---------------------------------------------------------------------------
// Colorizer
// ---------------------------------------------------------------------------

/// Assigns colors to nodes based on their source filename and nesting depth.
///
/// Hue is distributed evenly across the number of distinct files, lightness
/// decreases with nesting level, and saturation is always 1.0.
pub struct Colorizer {
    colored: bool,
    hues: Vec<f64>,
    idx_of: FxHashMap<Option<String>, usize>,
    idx: usize,
}

impl Colorizer {
    /// Create a new colorizer that distributes `num_colors` hues evenly
    /// around the color wheel.
    pub fn new(num_colors: usize, colored: bool) -> Self {
        let hues: Vec<f64> = if num_colors == 0 {
            vec![0.0]
        } else {
            (0..num_colors)
                .map(|j| j as f64 / num_colors as f64)
                .collect()
        };
        Self {
            colored,
            hues,
            idx_of: FxHashMap::default(),
            idx: 0,
        }
    }

    fn next_idx(&mut self) -> usize {
        let result = self.idx;
        self.idx += 1;
        if self.idx >= self.hues.len() {
            self.idx = 0; // wrap
        }
        result
    }

    fn node_to_idx(&mut self, node: &Node) -> usize {
        let key = node.filename.clone();
        if let Some(&i) = self.idx_of.get(&key) {
            i
        } else {
            let i = self.next_idx();
            self.idx_of.insert(key, i);
            i
        }
    }

    /// Return `(group_index, fill_rgba_hex, text_rgb_hex)` for the given node.
    pub fn make_colors(&mut self, node: &Node, interner: &Interner) -> (usize, String, String) {
        let idx = self.node_to_idx(node);

        if self.colored {
            let h = self.hues[idx];
            let level = get_level(node, interner);
            let l = (1.0 - 0.1 * level as f64).max(0.1);
            let s = 1.0;
            let a = 0.7;
            let (r, g, b) = hls_to_rgb(h, l, s);
            let fill = rgba_hex(r, g, b, a);
            let text = if l >= 0.5 {
                "#000000".to_string()
            } else {
                "#ffffff".to_string()
            };
            (idx, fill, text)
        } else {
            let fill = rgba_hex(1.0, 1.0, 1.0, 0.7);
            let text = "#000000".to_string();
            (idx, fill, text)
        }
    }
}

// ---------------------------------------------------------------------------
// Node helpers
// ---------------------------------------------------------------------------

/// Compute the nesting level of a node: number of dots in the namespace + 1,
/// or 0 if the namespace is empty / absent.
fn get_level(node: &Node, interner: &Interner) -> usize {
    match node.namespace {
        Some(ns) => {
            let ns_str = interner.resolve(ns);
            if !ns_str.is_empty() {
                1 + ns_str.chars().filter(|&c| c == '.').count()
            } else {
                0
            }
        }
        _ => 0,
    }
}

/// Escape GraphViz reserved words and problematic characters so the label can
/// be used as a node/subgraph identifier.
pub fn make_safe_label(name: &str) -> String {
    let unsafe_words = ["digraph", "graph", "cluster", "subgraph", "node"];
    let mut out = name.to_string();
    for word in &unsafe_words {
        // Replace each occurrence of the reserved word with word + "X".
        // This is intentionally naive (matching Python's `str.replace`).
        out = out.replace(word, &format!("{word}X"));
    }
    out = out.replace('.', "__");
    out = out.replace('-', "_");
    out = out.replace('*', "");
    out
}

// ---------------------------------------------------------------------------
// Visual graph types
// ---------------------------------------------------------------------------

/// A single node in the visual output graph.
pub struct VisualNode {
    /// GraphViz-safe identifier (no dots, no reserved words).
    pub id: String,
    /// Human-readable label.
    pub label: String,
    /// Node flavor as a string (e.g. `"function"`).
    pub flavor: String,
    /// Fill color in `#rrggbbaa` hex.
    pub fill_color: String,
    /// Font color in `#rrggbb` hex.
    pub text_color: String,
    /// Group index (used for GraphViz `group` attribute).
    pub group: usize,
}

/// A directed edge in the visual output graph.
pub struct VisualEdge {
    /// Index into [`VisualGraph::nodes`] for the source node.
    pub source_idx: usize,
    /// Index into [`VisualGraph::nodes`] for the target node.
    pub target_idx: usize,
    /// `"uses"` or `"defines"`.
    pub flavor: String,
    /// Edge color in hex.
    pub color: String,
}

/// A (possibly nested) graph of visual nodes and edges.
pub struct VisualGraph {
    /// Identifier (used as GraphViz graph id / subgraph name).
    pub id: String,
    /// Human-readable label for the (sub)graph.
    pub label: String,
    /// Nodes directly contained in this graph (not in subgraphs).
    pub nodes: Vec<VisualNode>,
    /// Edges (only stored on the root graph).
    pub edges: Vec<VisualEdge>,
    /// Nested subgraphs (one per namespace when grouping is enabled).
    pub subgraphs: Vec<VisualGraph>,
    /// Whether nodes are grouped into subgraphs by namespace.
    pub grouped: bool,
}

/// Options controlling what gets drawn in the visual graph.
pub struct VisualOptions {
    pub draw_defines: bool,
    pub draw_uses: bool,
    pub colored: bool,
    pub grouped: bool,
    pub annotated: bool,
}

impl VisualGraph {
    /// Build a visual graph from the analyzer's raw data.
    ///
    /// # Arguments
    ///
    /// * `nodes_arena`    - Flat arena of all nodes discovered by the analyzer.
    /// * `defined`        - Set of node IDs that are *defined* (not just referenced).
    /// * `defines_edges`  - Adjacency map: source → set of targets for "defines" edges.
    /// * `uses_edges`     - Adjacency map: source → set of targets for "uses" edges.
    /// * `options`        - Rendering options.
    pub fn from_call_graph(
        nodes_arena: &[Node],
        defined: &FxHashSet<NodeId>,
        defines_edges: &FxHashMap<NodeId, FxHashSet<NodeId>>,
        uses_edges: &FxHashMap<NodeId, FxHashSet<NodeId>>,
        options: &VisualOptions,
        interner: &Interner,
    ) -> Self {
        // 1. Collect defined nodes sorted by (namespace, name).
        let mut sorted_ids: Vec<NodeId> = defined.iter().copied().collect();
        sorted_ids.sort_by(|&a, &b| {
            let na = &nodes_arena[a];
            let nb = &nodes_arena[b];
            let ns_a = na.namespace.map(|s| interner.resolve(s));
            let ns_b = nb.namespace.map(|s| interner.resolve(s));
            let name_a = interner.resolve(na.name);
            let name_b = interner.resolve(nb.name);
            (ns_a, name_a).cmp(&(ns_b, name_b))
        });

        // 2. Count distinct filenames for the colorizer.
        let filenames: FxHashSet<Option<String>> = sorted_ids
            .iter()
            .map(|&id| nodes_arena[id].filename.clone())
            .collect();
        let mut colorizer = Colorizer::new(filenames.len() + 1, options.colored);

        // 3. Build VisualNodes and record a mapping from NodeId → index into
        //    the root graph's flat node list.  We will place references (by
        //    index) into subgraphs later.
        let mut all_nodes: Vec<VisualNode> = Vec::with_capacity(sorted_ids.len());
        let mut id_to_vis_idx: FxHashMap<NodeId, usize> = FxHashMap::default();

        // For grouping: namespace → list of vis-node indices.
        let mut ns_to_indices: BTreeMap<String, Vec<usize>> = BTreeMap::new();

        let labeler: Box<dyn Fn(&Node) -> String> = if options.annotated {
            if options.grouped {
                Box::new(|n: &Node| {
                    let name = interner.resolve(n.name);
                    if get_level(n, interner) >= 1
                        && let (Some(fname), Some(line)) = (&n.filename, n.line)
                    {
                        return format!("{name}\\n({}:{})", fname, line);
                    }
                    name.to_owned()
                })
            } else {
                Box::new(|n: &Node| {
                    let name = interner.resolve(n.name);
                    if get_level(n, interner) >= 1 {
                        if let (Some(fname), Some(line)) = (&n.filename, n.line) {
                            let ns = n.namespace.map(|s| interner.resolve(s)).unwrap_or("");
                            return format!(
                                "{name}\\n\\n({}:{},\\n{} in {})",
                                fname, line, n.flavor, ns
                            );
                        }
                        let ns = n.namespace.map(|s| interner.resolve(s)).unwrap_or("");
                        return format!("{name}\\n\\n({} in {})", n.flavor, ns);
                    }
                    name.to_owned()
                })
            }
        } else {
            Box::new(|n: &Node| n.get_short_name(interner).to_string())
        };

        for &node_id in &sorted_ids {
            let node = &nodes_arena[node_id];
            let (group, fill_color, text_color) = colorizer.make_colors(node, interner);
            let safe_id = make_safe_label(&node.get_name(interner));
            let label = labeler(node);

            let vis_idx = all_nodes.len();
            all_nodes.push(VisualNode {
                id: safe_id,
                label,
                flavor: node.flavor.to_string(),
                fill_color,
                text_color,
                group,
            });
            id_to_vis_idx.insert(node_id, vis_idx);

            let ns_key = node
                .namespace
                .map(|s| interner.resolve(s).to_owned())
                .unwrap_or_default();
            ns_to_indices.entry(ns_key).or_default().push(vis_idx);
        }

        // 4. Build subgraphs when grouping is enabled.
        let mut subgraphs: Vec<VisualGraph> = Vec::new();
        let mut root_node_indices: Vec<usize> = Vec::new();

        if options.grouped {
            for (ns, indices) in &ns_to_indices {
                let sg_id = make_safe_label(ns);
                let sg = VisualGraph {
                    id: sg_id,
                    label: ns.clone(),
                    nodes: Vec::new(), // will be filled below via swap
                    edges: Vec::new(),
                    subgraphs: Vec::new(),
                    grouped: false,
                };
                // We record the subgraph; the actual node data lives in `all_nodes`.
                // We'll assemble the final structure after edges.
                let _ = (sg, indices);
                subgraphs.push(VisualGraph {
                    id: make_safe_label(ns),
                    label: ns.clone(),
                    nodes: Vec::new(),
                    edges: Vec::new(),
                    subgraphs: Vec::new(),
                    grouped: false,
                });
            }
        } else {
            // All nodes go directly into root.
            root_node_indices.extend(0..all_nodes.len());
        }

        // 5. Build edges (only between defined nodes).
        let mut edges: Vec<VisualEdge> = Vec::new();

        if options.draw_defines {
            let color = "#838b8b".to_string();
            for (&src, targets) in defines_edges {
                if !defined.contains(&src) {
                    continue;
                }
                let Some(&src_vi) = id_to_vis_idx.get(&src) else {
                    continue;
                };
                for &tgt in targets {
                    if !defined.contains(&tgt) {
                        continue;
                    }
                    let Some(&tgt_vi) = id_to_vis_idx.get(&tgt) else {
                        continue;
                    };
                    edges.push(VisualEdge {
                        source_idx: src_vi,
                        target_idx: tgt_vi,
                        flavor: "defines".to_string(),
                        color: color.clone(),
                    });
                }
            }
        }

        if options.draw_uses {
            let color = "#000000".to_string();
            for (&src, targets) in uses_edges {
                if !defined.contains(&src) {
                    continue;
                }
                let Some(&src_vi) = id_to_vis_idx.get(&src) else {
                    continue;
                };
                for &tgt in targets {
                    if !defined.contains(&tgt) {
                        continue;
                    }
                    let Some(&tgt_vi) = id_to_vis_idx.get(&tgt) else {
                        continue;
                    };
                    edges.push(VisualEdge {
                        source_idx: src_vi,
                        target_idx: tgt_vi,
                        flavor: "uses".to_string(),
                        color: color.clone(),
                    });
                }
            }
        }

        // 6. Assemble the final graph.  When grouped, distribute owned nodes
        //    into the correct subgraph.  We need to move nodes out of the
        //    flat vec into per-subgraph vecs.  To keep indices stable (edges
        //    reference them) we do *not* move; instead each VisualNode is
        //    duplicated into its subgraph and the root `nodes` holds the
        //    full flat list used by edges.
        //
        //    Writers should iterate subgraphs for node rendering but use the
        //    root `nodes` vec for edge source/target lookup.

        if options.grouped {
            // `ns_to_indices` is BTreeMap, subgraphs were pushed in the same
            // iteration order.
            for (sg, (_ns, indices)) in subgraphs.iter_mut().zip(ns_to_indices.iter()) {
                for &vi in indices {
                    // Clone the VisualNode into the subgraph for rendering.
                    let n = &all_nodes[vi];
                    sg.nodes.push(VisualNode {
                        id: n.id.clone(),
                        label: n.label.clone(),
                        flavor: n.flavor.clone(),
                        fill_color: n.fill_color.clone(),
                        text_color: n.text_color.clone(),
                        group: n.group,
                    });
                }
            }
        }

        let root_nodes = if options.grouped {
            // Root `nodes` keeps the flat list for edge index lookup.
            all_nodes
        } else {
            all_nodes
        };

        VisualGraph {
            id: "G".to_string(),
            label: String::new(),
            nodes: root_nodes,
            edges,
            subgraphs,
            grouped: options.grouped,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intern::Interner;
    use crate::node::Flavor;

    #[test]
    fn test_hls_to_rgb_achromatic() {
        let (r, g, b) = hls_to_rgb(0.0, 0.5, 0.0);
        assert!((r - 0.5).abs() < 1e-9);
        assert!((g - 0.5).abs() < 1e-9);
        assert!((b - 0.5).abs() < 1e-9);
    }

    #[test]
    fn test_hls_to_rgb_red() {
        let (r, g, b) = hls_to_rgb(0.0, 0.5, 1.0);
        assert!((r - 1.0).abs() < 1e-9);
        assert!(g.abs() < 1e-9);
        assert!(b.abs() < 1e-9);
    }

    #[test]
    fn test_rgba_hex() {
        assert_eq!(rgba_hex(1.0, 0.0, 0.0, 0.5), "#ff00007f");
    }

    #[test]
    fn test_rgb_hex() {
        assert_eq!(rgb_hex(0.0, 1.0, 0.0), "#00ff00");
    }

    #[test]
    fn test_make_safe_label() {
        assert_eq!(make_safe_label("my.graph.node"), "my__graphX__nodeX");
        assert_eq!(make_safe_label("digraph"), "digraphXX");
        assert_eq!(make_safe_label("foo*bar"), "foobar");
        assert_eq!(make_safe_label("my-package.mod"), "my_package__mod");
    }

    #[test]
    fn test_colorizer_uncolored() {
        let mut interner = Interner::new();
        let ns = interner.intern("ns");
        let name = interner.intern("f");
        let fqn = interner.intern("ns.f");
        let mut c = Colorizer::new(3, false);
        let node = Node::new(Some(ns), name, fqn, Flavor::Function);
        let (_, fill, text) = c.make_colors(&node, &interner);
        assert_eq!(fill, rgba_hex(1.0, 1.0, 1.0, 0.7));
        assert_eq!(text, "#000000");
    }

    #[test]
    fn test_colorizer_wraps() {
        let mut interner = Interner::new();
        let ns = interner.intern("ns");
        let a = interner.intern("a");
        let b = interner.intern("b");
        let c_name = interner.intern("c");
        let fqn_a = interner.intern("ns.a");
        let fqn_b = interner.intern("ns.b");
        let fqn_c = interner.intern("ns.c");
        let mut c = Colorizer::new(2, true);
        let n1 = Node::new(Some(ns), a, fqn_a, Flavor::Function).with_location("file1.py", 1);
        let n2 = Node::new(Some(ns), b, fqn_b, Flavor::Function).with_location("file2.py", 1);
        let n3 = Node::new(Some(ns), c_name, fqn_c, Flavor::Function).with_location("file3.py", 1);
        let (i1, _, _) = c.make_colors(&n1, &interner);
        let (i2, _, _) = c.make_colors(&n2, &interner);
        let (i3, _, _) = c.make_colors(&n3, &interner);
        assert_eq!(i1, 0);
        assert_eq!(i2, 1);
        assert_eq!(i3, 0); // wrapped
    }

    #[test]
    fn test_from_call_graph_basic() {
        let mut interner = Interner::new();
        let pkg = interner.intern("pkg");
        let a = interner.intern("A");
        let b = interner.intern("B");
        let fqn_a = interner.intern("pkg.A");
        let fqn_b = interner.intern("pkg.B");
        let nodes_arena = vec![
            Node::new(Some(pkg), a, fqn_a, Flavor::Class).with_location("pkg.py", 1),
            Node::new(Some(pkg), b, fqn_b, Flavor::Function).with_location("pkg.py", 10),
        ];
        let mut defined = FxHashSet::default();
        defined.insert(0);
        defined.insert(1);

        let mut uses_edges = FxHashMap::default();
        uses_edges.entry(0).or_insert_with(FxHashSet::default).insert(1);

        let options = VisualOptions {
            draw_defines: false,
            draw_uses: true,
            colored: true,
            grouped: false,
            annotated: false,
        };

        let vg = VisualGraph::from_call_graph(
            &nodes_arena,
            &defined,
            &FxHashMap::default(),
            &uses_edges,
            &options,
            &interner,
        );

        assert_eq!(vg.nodes.len(), 2);
        assert_eq!(vg.edges.len(), 1);
        assert_eq!(vg.edges[0].flavor, "uses");
        assert_eq!(vg.subgraphs.len(), 0);
    }

    #[test]
    fn test_from_call_graph_grouped() {
        let mut interner = Interner::new();
        let pkg = interner.intern("pkg");
        let other = interner.intern("other");
        let a = interner.intern("A");
        let b = interner.intern("B");
        let fqn_a = interner.intern("pkg.A");
        let fqn_b = interner.intern("other.B");
        let nodes_arena = vec![
            Node::new(Some(pkg), a, fqn_a, Flavor::Class).with_location("pkg.py", 1),
            Node::new(Some(other), b, fqn_b, Flavor::Function).with_location("other.py", 5),
        ];
        let mut defined = FxHashSet::default();
        defined.insert(0);
        defined.insert(1);

        let options = VisualOptions {
            draw_defines: false,
            draw_uses: false,
            colored: false,
            grouped: true,
            annotated: false,
        };

        let vg = VisualGraph::from_call_graph(
            &nodes_arena,
            &defined,
            &FxHashMap::default(),
            &FxHashMap::default(),
            &options,
            &interner,
        );

        assert!(vg.grouped);
        assert_eq!(vg.subgraphs.len(), 2);
        // Each subgraph should have exactly one node.
        assert_eq!(vg.subgraphs[0].nodes.len(), 1);
        assert_eq!(vg.subgraphs[1].nodes.len(), 1);
    }

    #[test]
    fn test_from_call_graph_grouped_annotated_labels_include_locations() {
        let mut interner = Interner::new();
        let pkg = interner.intern("pkg");
        let a = interner.intern("A");
        let fqn_a = interner.intern("pkg.A");
        let nodes_arena =
            vec![Node::new(Some(pkg), a, fqn_a, Flavor::Class).with_location("pkg.py", 7)];
        let defined = FxHashSet::from_iter([0]);

        let options = VisualOptions {
            draw_defines: false,
            draw_uses: false,
            colored: false,
            grouped: true,
            annotated: true,
        };

        let vg = VisualGraph::from_call_graph(
            &nodes_arena,
            &defined,
            &FxHashMap::default(),
            &FxHashMap::default(),
            &options,
            &interner,
        );

        let label = &vg.subgraphs[0].nodes[0].label;
        assert!(label.contains("A"));
        assert!(label.contains("pkg.py:7"));
    }
}
