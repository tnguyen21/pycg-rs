use std::collections::{HashMap, HashSet};

use crate::node::NodeId;

/// Compute the method resolution order (MRO) using C3 linearization.
pub(super) fn resolve_mro(
    class_base_nodes: &HashMap<NodeId, Vec<NodeId>>,
) -> HashMap<NodeId, Vec<NodeId>> {
    fn head(lst: &[NodeId]) -> Option<NodeId> {
        lst.first().copied()
    }

    fn tail(lst: &[NodeId]) -> Vec<NodeId> {
        if lst.len() > 1 {
            lst[1..].to_vec()
        } else {
            Vec::new()
        }
    }

    fn c3_find_good_head(heads: &[NodeId], tails: &[Vec<NodeId>]) -> Option<NodeId> {
        let flat_tails: Vec<NodeId> = tails.iter().flat_map(|t| t.iter().copied()).collect();
        heads.iter().find(|&&hd| !flat_tails.contains(&hd)).copied() // Cyclic dependency
    }

    fn c3_merge(lists: &mut [Vec<NodeId>]) -> Vec<NodeId> {
        let mut out = Vec::new();
        loop {
            let heads: Vec<NodeId> = lists
                .iter()
                .filter_map(|l| head(l))
                .collect();
            if heads.is_empty() {
                break;
            }
            let tails: Vec<Vec<NodeId>> = lists.iter().map(|l| tail(l)).collect();
            if let Some(hd) = c3_find_good_head(&heads, &tails) {
                out.push(hd);
                for list in lists.iter_mut() {
                    list.retain(|&x| x != hd);
                }
            } else {
                break; // Cyclic -- give up
            }
        }
        out
    }

    let mut mro = HashMap::new();
    let mut memo: HashMap<NodeId, Vec<NodeId>> = HashMap::new();

    fn c3_linearize(
        node: NodeId,
        class_base_nodes: &HashMap<NodeId, Vec<NodeId>>,
        memo: &mut HashMap<NodeId, Vec<NodeId>>,
        seen: &mut HashSet<NodeId>,
    ) -> Vec<NodeId> {
        seen.insert(node);
        if let Some(cached) = memo.get(&node) {
            return cached.clone();
        }

        let result = if !class_base_nodes.contains_key(&node)
            || class_base_nodes[&node].is_empty()
        {
            vec![node]
        } else {
            let mut lists = Vec::new();
            for &base in &class_base_nodes[&node] {
                if !seen.contains(&base) {
                    lists.push(c3_linearize(base, class_base_nodes, memo, seen));
                }
            }
            lists.push(class_base_nodes[&node].clone());
            let mut result = vec![node];
            result.extend(c3_merge(&mut lists));
            result
        };

        memo.insert(node, result.clone());
        result
    }

    for &cls in class_base_nodes.keys() {
        let mut seen = HashSet::new();
        let lin = c3_linearize(cls, class_base_nodes, &mut memo, &mut seen);
        mro.insert(cls, lin);
    }

    mro
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_mro_simple() {
        // A -> B -> C (linear chain)
        let mut bases = HashMap::new();
        bases.insert(0, vec![1]); // A inherits from B
        bases.insert(1, vec![2]); // B inherits from C
        bases.insert(2, vec![]); // C has no bases

        let mro = resolve_mro(&bases);
        assert_eq!(mro[&0], vec![0, 1, 2]);
        assert_eq!(mro[&1], vec![1, 2]);
        assert_eq!(mro[&2], vec![2]);
    }

    #[test]
    fn test_resolve_mro_diamond() {
        // D inherits from B, C; both B and C inherit from A
        let mut bases = HashMap::new();
        bases.insert(3, vec![1, 2]); // D -> B, C
        bases.insert(1, vec![0]); // B -> A
        bases.insert(2, vec![0]); // C -> A
        bases.insert(0, vec![]); // A

        let mro = resolve_mro(&bases);
        assert_eq!(mro[&3], vec![3, 1, 2, 0]);
    }
}
