use crate::depgraph::{DeclId, topological_sort};
use std::collections::{HashMap, HashSet};

fn g(name: &str) -> DeclId { DeclId::Global(name.to_string()) }
fn s(name: &str) -> DeclId { DeclId::Section(name.to_string()) }

fn make_graph(edges: &[(&str, &[&str])]) -> HashMap<DeclId, HashSet<DeclId>> {
    let mut map = HashMap::new();
    for (node_str, deps) in edges {
        let node = if node_str.chars().next().unwrap().is_uppercase() {
            s(node_str)
        } else {
            g(node_str)
        };
        let dep_set: HashSet<DeclId> = deps.iter().map(|d| {
            if d.chars().next().unwrap().is_uppercase() { s(d) } else { g(d) }
        }).collect();
        map.insert(node, dep_set);
    }
    map
}

#[test]
fn topo_sort_linear_chain() {
    // a depends on b, b depends on c; expected order: c, b, a
    let graph = make_graph(&[
        ("a", &["b"]),
        ("b", &["c"]),
        ("c", &[]),
    ]);
    let order = topological_sort(&graph).unwrap();
    let a = order.iter().position(|x| *x == g("a")).unwrap();
    let b = order.iter().position(|x| *x == g("b")).unwrap();
    let c = order.iter().position(|x| *x == g("c")).unwrap();
    assert!(c < b && b < a);
}

#[test]
fn topo_sort_no_deps() {
    let graph = make_graph(&[
        ("x", &[]),
        ("y", &[]),
    ]);
    let order = topological_sort(&graph).unwrap();
    assert_eq!(order.len(), 2);
}

#[test]
fn topo_sort_cycle_returns_err() {
    let graph = make_graph(&[
        ("a", &["b"]),
        ("b", &["a"]),
    ]);
    assert!(topological_sort(&graph).is_err());
}

#[test]
fn topo_sort_section_and_global_mixed() {
    let graph = make_graph(&[
        ("Server", &["port"]),   // Section depends on global
        ("port", &[]),
    ]);
    let order = topological_sort(&graph).unwrap();
    let port_pos = order.iter().position(|x| *x == g("port")).unwrap();
    let server_pos = order.iter().position(|x| *x == s("Server")).unwrap();
    assert!(port_pos < server_pos);
}
