use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DeclId {
    Global(String),
    Section(String),
}

pub type DepGraph = HashMap<DeclId, HashSet<DeclId>>;

/// Kahn's algorithm topological sort.
/// Returns Ok(order) where earlier items have no unmet deps,
/// or Err(cycle_nodes) listing nodes that couldn't be resolved.
pub fn topological_sort(graph: &DepGraph) -> Result<Vec<DeclId>, Vec<DeclId>> {
    // Proper implementation: in_degree[node] = number of deps of node
    let mut in_deg: HashMap<DeclId, usize> = HashMap::new();
    for (node, deps) in graph {
        let e = in_deg.entry(node.clone()).or_insert(0);
        *e += deps.len();
        for dep in deps {
            in_deg.entry(dep.clone()).or_insert(0);
        }
    }

    // Reverse adjacency: dep → list of nodes that need dep to be scheduled first
    let mut rev_adj: HashMap<DeclId, Vec<DeclId>> = HashMap::new();
    for (node, deps) in graph {
        for dep in deps {
            rev_adj.entry(dep.clone()).or_default().push(node.clone());
        }
    }

    let mut queue: VecDeque<DeclId> = in_deg
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(k, _)| k.clone())
        .collect();

    let mut result = Vec::new();

    while let Some(node) = queue.pop_front() {
        result.push(node.clone());
        if let Some(dependents) = rev_adj.get(&node) {
            for dependent in dependents {
                let d = in_deg.get_mut(dependent).unwrap();
                *d -= 1;
                if *d == 0 {
                    queue.push_back(dependent.clone());
                }
            }
        }
    }

    if result.len() == in_deg.len() {
        Ok(result)
    } else {
        let cycle_nodes = in_deg
            .into_iter()
            .filter(|(_, d)| *d > 0)
            .map(|(k, _)| k)
            .collect();
        Err(cycle_nodes)
    }
}

/// Cross-file cycle detection for `import asPartOf`. `stack` is the chain of
/// files currently being expanded (outermost first); returns the cycle
/// (stack-suffix + the repeated candidate) if `candidate` is already on it.
pub fn find_cycle_in_stack(
    stack: &[std::path::PathBuf],
    candidate: &std::path::Path,
) -> Option<Vec<std::path::PathBuf>> {
    let pos = stack.iter().position(|p| p.as_path() == candidate)?;
    let mut cycle: Vec<std::path::PathBuf> = stack[pos..].to_vec();
    cycle.push(candidate.to_path_buf());
    Some(cycle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn find_cycle_in_stack_detects_reentry() {
        let stack = vec![PathBuf::from("/a.spar"), PathBuf::from("/b.spar")];
        let cycle = find_cycle_in_stack(&stack, std::path::Path::new("/a.spar"));
        assert!(cycle.is_some());
        let cycle = cycle.unwrap();
        assert_eq!(cycle, vec![PathBuf::from("/a.spar"), PathBuf::from("/b.spar"), PathBuf::from("/a.spar")]);
    }

    #[test]
    fn find_cycle_in_stack_none_for_new_path() {
        let stack = vec![PathBuf::from("/a.spar")];
        assert!(find_cycle_in_stack(&stack, std::path::Path::new("/b.spar")).is_none());
    }
}
