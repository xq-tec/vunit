// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! A directed graph of dependencies with deterministic topological sorting.
//!
//! A port of `dependency_graph.py`. Unlike `VUnit`, ties in the topological order are broken by
//! insertion order instead of by sorting the nodes, and only sorting reports cycles.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::hash::Hash;

use rustc_hash::FxHashMap;
use rustc_hash::FxHashSet;
use thiserror::Error;

/// A dependency graph over nodes of type `N`.
///
/// An edge from `dependency` to `dependent` means that `dependent` depends on `dependency`.
#[derive(Debug, Clone)]
pub struct DependencyGraph<N> {
    nodes: Vec<N>,
    indices: FxHashMap<N, usize>,
    /// Dependents of each node, in insertion order.
    forward: Vec<Vec<usize>>,
    /// Dependencies of each node, in insertion order.
    backward: Vec<Vec<usize>>,
    edges: FxHashSet<(usize, usize)>,
}

/// A dependency cycle; the path starts and ends with the same node.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("circular dependency")]
pub struct CircularDependency<N> {
    /// The nodes of the cycle, each depending on the previous one, with the first node repeated
    /// at the end.
    pub path: Vec<N>,
}

impl<N> Default for DependencyGraph<N> {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            indices: FxHashMap::default(),
            forward: Vec::new(),
            backward: Vec::new(),
            edges: FxHashSet::default(),
        }
    }
}

impl<N: Copy + Eq + Hash> DependencyGraph<N> {
    /// Creates an empty graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a node; adding a node twice has no effect.
    pub fn add_node(&mut self, node: N) {
        self.index_of(node);
    }

    fn index_of(&mut self, node: N) -> usize {
        *self.indices.entry(node).or_insert_with(|| {
            self.nodes.push(node);
            self.forward.push(Vec::new());
            self.backward.push(Vec::new());
            self.nodes.len() - 1
        })
    }

    /// Records that `dependent` depends on `dependency`, adding missing nodes.
    ///
    /// Returns `false` if the edge already existed.
    pub fn add_dependency(&mut self, dependency: N, dependent: N) -> bool {
        let from = self.index_of(dependency);
        let to = self.index_of(dependent);
        if !self.edges.insert((from, to)) {
            return false;
        }
        self.forward[from].push(to);
        self.backward[to].push(from);
        true
    }

    /// The nodes in insertion order.
    pub fn nodes(&self) -> &[N] {
        &self.nodes
    }

    /// Whether the graph contains `node`.
    pub fn contains(&self, node: N) -> bool {
        self.indices.contains_key(&node)
    }

    /// Sorts the nodes so that every node comes after its dependencies.
    ///
    /// Among the nodes whose dependencies are all placed, the one added first comes next.
    ///
    /// # Errors
    ///
    /// Returns one of the cycles if the graph has any.
    pub fn toposort(&self) -> Result<Vec<N>, CircularDependency<N>> {
        let mut missing: Vec<usize> = self.backward.iter().map(Vec::len).collect();
        let mut ready: BinaryHeap<Reverse<usize>> = missing
            .iter()
            .enumerate()
            .filter(|&(_, &count)| count == 0)
            .map(|(index, _)| Reverse(index))
            .collect();
        let mut sorted = Vec::with_capacity(self.nodes.len());
        while let Some(Reverse(index)) = ready.pop() {
            sorted.push(self.nodes[index]);
            for &dependent in &self.forward[index] {
                missing[dependent] -= 1;
                if missing[dependent] == 0 {
                    ready.push(Reverse(dependent));
                }
            }
        }
        if sorted.len() == self.nodes.len() {
            Ok(sorted)
        } else {
            Err(self.find_cycle(&missing))
        }
    }

    /// Finds a cycle among the nodes that toposort couldn't place.
    ///
    /// Each unplaced node has an unplaced dependency, so walking dependencies from one of them
    /// must reach a node twice.
    fn find_cycle(&self, missing: &[usize]) -> CircularDependency<N> {
        let unplaced = |index: usize| missing[index] > 0;
        let mut walk = Vec::new();
        let mut position_in_walk = FxHashMap::default();
        let mut current = (0..self.nodes.len()).find(|&index| unplaced(index));
        while let Some(index) = current {
            if let Some(&start) = position_in_walk.get(&index) {
                // The walk goes against the edges; reverse it so each node depends on the previous.
                let mut cycle: Vec<usize> = walk[start..].to_vec();
                cycle.reverse();
                let first = cycle
                    .iter()
                    .enumerate()
                    .min_by_key(|&(_, &node)| node)
                    .map_or(0, |(position, _)| position);
                cycle.rotate_left(first);
                cycle.push(cycle[0]);
                return CircularDependency {
                    path: cycle.into_iter().map(|node| self.nodes[node]).collect(),
                };
            }
            position_in_walk.insert(index, walk.len());
            walk.push(index);
            current = self.backward[index]
                .iter()
                .copied()
                .find(|&dependency| unplaced(dependency));
        }
        CircularDependency { path: Vec::new() }
    }

    /// Returns `nodes` and everything that depends on them, directly or indirectly.
    pub fn dependents(&self, nodes: impl IntoIterator<Item = N>) -> FxHashSet<N> {
        self.reachable(nodes, &self.forward)
    }

    /// Returns `nodes` and everything they depend on, directly or indirectly.
    pub fn dependencies(&self, nodes: impl IntoIterator<Item = N>) -> FxHashSet<N> {
        self.reachable(nodes, &self.backward)
    }

    /// Returns the direct dependencies of `node`, in insertion order.
    pub fn direct_dependencies(&self, node: N) -> impl Iterator<Item = N> + '_ {
        self.indices
            .get(&node)
            .map(|&index| self.backward[index].as_slice())
            .unwrap_or_default()
            .iter()
            .map(|&index| self.nodes[index])
    }

    fn reachable(&self, nodes: impl IntoIterator<Item = N>, edges: &[Vec<usize>]) -> FxHashSet<N> {
        let mut visited = FxHashSet::default();
        let mut pending: Vec<usize> = nodes
            .into_iter()
            .filter_map(|node| self.indices.get(&node).copied())
            .collect();
        while let Some(index) = pending.pop() {
            if visited.insert(index) {
                pending.extend(edges[index].iter().copied());
            }
        }
        visited.into_iter().map(|index| self.nodes[index]).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(
        nodes: &[&'static str],
        dependencies: &[(&'static str, &'static str)],
    ) -> DependencyGraph<&'static str> {
        let mut graph = DependencyGraph::new();
        for &node in nodes {
            graph.add_node(node);
        }
        for &(dependency, dependent) in dependencies {
            graph.add_dependency(dependency, dependent);
        }
        graph
    }

    fn check_order(result: &[&str], dependencies: &[(&str, &str)]) {
        let position = |node| result.iter().position(|&other| other == node).unwrap();
        for &(dependency, dependent) in dependencies {
            assert!(
                position(dependency) < position(dependent),
                "{dependency} is not before {dependent}"
            );
        }
    }

    fn set(nodes: &[&'static str]) -> FxHashSet<&'static str> {
        nodes.iter().copied().collect()
    }

    #[test]
    fn empty_graph_sorts_to_nothing() {
        assert!(DependencyGraph::<u32>::new().toposort().unwrap().is_empty());
    }

    #[test]
    fn independent_nodes_keep_insertion_order() {
        let nodes = ["c", "a", "d", "b"];
        assert_eq!(graph(&nodes, &[]).toposort().unwrap(), nodes);
    }

    #[test]
    fn sorts_in_topological_order() {
        let dependencies = [("a", "b"), ("a", "c"), ("b", "d"), ("e", "f")];
        let result = graph(&["a", "b", "c", "d", "e", "f"], &dependencies)
            .toposort()
            .unwrap();
        check_order(&result, &dependencies);
    }

    #[test]
    fn ties_are_broken_by_insertion_order() {
        let result = graph(&["d", "c", "b", "a"], &[("a", "d")])
            .toposort()
            .unwrap();
        assert_eq!(result, ["c", "b", "a", "d"]);
    }

    #[test]
    fn error_on_self_dependency() {
        let error = graph(
            &["a", "b", "c", "d"],
            &[("a", "b"), ("a", "c"), ("b", "d"), ("d", "d")],
        )
        .toposort()
        .unwrap_err();
        assert_eq!(error.path, ["d", "d"]);
    }

    #[test]
    fn error_on_long_circular_dependency() {
        let error = graph(
            &["a", "b", "c", "d"],
            &[("a", "b"), ("a", "c"), ("b", "d"), ("d", "a")],
        )
        .toposort()
        .unwrap_err();
        assert_eq!(error.path, ["a", "b", "d", "a"]);
    }

    #[test]
    fn resorts_after_additions() {
        let mut graph = graph(
            &["a", "b", "c", "d", "e", "f"],
            &[("a", "b"), ("a", "c"), ("b", "d"), ("e", "f")],
        );
        graph.toposort().unwrap();
        graph.add_node("g");
        graph.add_dependency("b", "g");
        let result = graph.toposort().unwrap();
        check_order(
            &result,
            &[("a", "b"), ("a", "c"), ("b", "d"), ("e", "f"), ("b", "g")],
        );
    }

    #[test]
    fn duplicate_edges_are_reported() {
        let mut graph = graph(&["a", "b"], &[]);
        assert!(graph.add_dependency("a", "b"));
        assert!(!graph.add_dependency("a", "b"));
    }

    #[test]
    fn direct_dependencies() {
        let graph = graph(&["a", "b", "c", "d"], &[("a", "b"), ("a", "c")]);
        assert_eq!(graph.direct_dependencies("b").count(), 1);
        assert_eq!(graph.direct_dependencies("c").collect::<Vec<_>>(), ["a"]);
        assert_eq!(graph.direct_dependencies("d").count(), 0);
        assert_eq!(graph.direct_dependencies("x").count(), 0);
    }

    #[test]
    fn dependents() {
        let graph = graph(
            &["a", "b", "c", "d", "e"],
            &[("a", "b"), ("b", "c"), ("c", "d"), ("e", "d")],
        );
        assert_eq!(graph.dependents(["a"]), set(&["a", "b", "c", "d"]));
        assert_eq!(graph.dependents(["e"]), set(&["d", "e"]));
    }

    #[test]
    fn dependencies() {
        let graph = graph(
            &["a", "b", "c", "d", "e"],
            &[("b", "a"), ("c", "b"), ("d", "c"), ("d", "e")],
        );
        assert_eq!(graph.dependencies(["a"]), set(&["a", "b", "c", "d"]));
        assert_eq!(graph.dependencies(["e"]), set(&["d", "e"]));
    }

    #[test]
    fn reachability_terminates_on_cycles() {
        let graph = graph(&["a", "b", "c"], &[("a", "b"), ("b", "c"), ("c", "a")]);
        assert_eq!(graph.dependents(["a"]), set(&["a", "b", "c"]));
        assert_eq!(graph.dependencies(["a"]), set(&["a", "b", "c"]));
    }
}
