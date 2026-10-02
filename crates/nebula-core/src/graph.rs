//! Core graph representation.
//!
//! Design for scale: the canonical form is a flat, undirected **edge list**
//! (`Vec<[u32; 2]>`), which is exactly what the GPU spring-layout and the edge
//! renderer consume. Algorithms that need neighbor iteration build a **CSR**
//! (compressed sparse row) adjacency on demand; CSR construction is parallelized.
//!
//! Node ids are `u32` (up to ~4.29B nodes — covers the 1B target). Edge indices
//! are `u64` because a billion-node graph with a modest average degree easily
//! exceeds 4B directed entries.

use std::sync::OnceLock;

pub type NodeId = u32;

/// An undirected graph stored as a flat edge list, with a lazily-built CSR.
#[derive(Clone)]
pub struct Graph {
    num_nodes: u64,
    /// Canonical undirected edge list — each edge stored once.
    edges: Vec<[NodeId; 2]>,
    /// Cached CSR adjacency (built on demand). Directed doubling of `edges`.
    csr: OnceLock<Csr>,
    directions: Vec<bool>,
    weights: Vec<f32>,
    outgoing: OnceLock<Csr>,
    incoming: OnceLock<Csr>,
    has_direction: bool,
    has_weight: bool,
}

/// Compressed-sparse-row adjacency. For an undirected graph each edge appears
/// twice (once per endpoint) so neighbor iteration is symmetric.
#[derive(Clone)]
pub struct Csr {
    /// `offsets[i]..offsets[i+1]` indexes into `targets` for node `i`.
    /// Length `num_nodes + 1`.
    pub offsets: Vec<u64>,
    /// Concatenated neighbor lists. Length = 2 * num_edges (undirected).
    pub targets: Vec<NodeId>,
    pub weights: Vec<f32>,
}

impl Csr {
    #[inline]
    pub fn neighbors(&self, node: NodeId) -> &[NodeId] {
        let s = self.offsets[node as usize] as usize;
        let e = self.offsets[node as usize + 1] as usize;
        &self.targets[s..e]
    }

    #[inline]
    pub fn degree(&self, node: NodeId) -> u32 {
        (self.offsets[node as usize + 1] - self.offsets[node as usize]) as u32
    }
}

impl Graph {
    pub fn new(num_nodes: u64, edges: Vec<[NodeId; 2]>) -> Self {
        Graph {
            num_nodes,
            edges,
            csr: OnceLock::new(),
            directions: Vec::new(),
            weights: Vec::new(),
            outgoing: OnceLock::new(),
            incoming: OnceLock::new(),
            has_direction: false,
            has_weight: false,
        }
    }

    /// An empty graph with `num_nodes` isolated vertices.
    pub fn empty(num_nodes: u64) -> Self {
        Self::new(num_nodes, Vec::new())
    }

    #[inline]
    pub fn num_nodes(&self) -> u64 {
        self.num_nodes
    }

    #[inline]
    pub fn num_edges(&self) -> u64 {
        self.edges.len() as u64
    }

    #[inline]
    pub fn edges(&self) -> &[[NodeId; 2]] {
        &self.edges
    }

    /// Flattened `[src0, dst0, src1, dst1, ...]` view for GPU upload.
    pub fn edges_flat(&self) -> &[u32] {
        bytemuck::cast_slice(&self.edges)
    }

    /// Build (or return cached) CSR adjacency. Parallel counting-sort style
    /// construction: O(N + E) work, scales across cores.
    pub fn csr(&self) -> &Csr {
        self.csr
            .get_or_init(|| build_csr(self.num_nodes, &self.edges))
    }

    pub fn csr_ref(&self) -> Option<&Csr> {
        self.csr.get()
    }

    pub fn with_metadata(
        num_nodes: u64,
        edges: Vec<[NodeId; 2]>,
        directions: Vec<bool>,
        weights: Vec<f32>,
    ) -> Self {
        assert_eq!(edges.len(), directions.len());
        assert_eq!(edges.len(), weights.len());
        let mut g = Self::new(num_nodes, edges);
        g.has_direction = directions.iter().any(|v| *v);
        g.has_weight = weights.iter().any(|v| *v != 1.0);
        g.directions = directions;
        g.weights = weights;
        g
    }

    pub fn outgoing(&self) -> &Csr {
        if !self.has_direction && !self.has_weight {
            return self.csr();
        }
        self.outgoing.get_or_init(|| {
            build_adjacency(
                self.num_nodes,
                &self.edges,
                &self.directions,
                &self.weights,
                false,
            )
        })
    }
    pub fn incoming(&self) -> &Csr {
        if !self.has_direction {
            return self.outgoing();
        }
        self.incoming.get_or_init(|| {
            build_adjacency(
                self.num_nodes,
                &self.edges,
                &self.directions,
                &self.weights,
                true,
            )
        })
    }

    /// Build a fresh CSR without caching it (useful when a consumer wants to own
    /// adjacency independently of the graph's lifetime, e.g. a layout engine).
    pub fn compute_csr(&self) -> Csr {
        build_csr(self.num_nodes, &self.edges)
    }

    pub fn ensure_csr(&self) {
        let _ = self.csr();
    }

    /// Per-node degree (undirected). Cheap even without CSR.
    pub fn degrees(&self) -> Vec<u32> {
        let mut deg = vec![0u32; self.num_nodes as usize];
        // Sequential to avoid atomics; degree counting is memory-bound anyway.
        for &[a, b] in &self.edges {
            deg[a as usize] = deg[a as usize].saturating_add(1);
            deg[b as usize] = deg[b as usize].saturating_add(1);
        }
        deg
    }
}

/// Parallel CSR construction from an undirected edge list.
fn build_csr(num_nodes: u64, edges: &[[NodeId; 2]]) -> Csr {
    build_adjacency(num_nodes, edges, &[], &[], false)
}

// One histogram, independent of thread count: bounded O(N + E) scratch memory.
fn build_adjacency(
    num_nodes: u64,
    edges: &[[NodeId; 2]],
    directed: &[bool],
    weights: &[f32],
    reverse: bool,
) -> Csr {
    let n = num_nodes as usize;
    let mut offsets = vec![0u64; n + 1];
    for (i, &[a, b]) in edges.iter().enumerate() {
        let (a, b) = if reverse { (b, a) } else { (a, b) };
        offsets[a as usize + 1] += 1;
        if !directed.get(i).copied().unwrap_or(false) {
            offsets[b as usize + 1] += 1;
        }
    }
    for i in 0..n {
        offsets[i + 1] += offsets[i];
    }
    let mut targets = vec![0; offsets[n] as usize];
    let mut arc_weights = if weights.is_empty() {
        Vec::new()
    } else {
        vec![1.0; targets.len()]
    };
    let mut cursor = offsets[..n].to_vec();
    for (i, &[a, b]) in edges.iter().enumerate() {
        let (a, b) = if reverse { (b, a) } else { (a, b) };
        let mut insert = |source: u32, target: u32| {
            let j = cursor[source as usize] as usize;
            targets[j] = target;
            if !arc_weights.is_empty() {
                arc_weights[j] = weights[i];
            }
            cursor[source as usize] += 1;
        };
        insert(a, b);
        if !directed.get(i).copied().unwrap_or(false) {
            insert(b, a);
        }
    }
    Csr {
        offsets,
        targets,
        weights: arc_weights,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csr_neighbors_symmetric() {
        // Triangle 0-1-2 plus a pendant 2-3.
        let mut g = Graph::new(4, vec![[0, 1], [1, 2], [2, 0], [2, 3]]);
        let csr = g.csr();
        let mut n0: Vec<_> = csr.neighbors(0).to_vec();
        n0.sort();
        assert_eq!(n0, vec![1, 2]);
        let mut n2: Vec<_> = csr.neighbors(2).to_vec();
        n2.sort();
        assert_eq!(n2, vec![0, 1, 3]);
        assert_eq!(csr.degree(2), 3);
        assert_eq!(csr.degree(3), 1);
    }

    #[test]
    fn degrees_match_csr() {
        let mut g = Graph::new(4, vec![[0, 1], [1, 2], [2, 0], [2, 3]]);
        let deg = g.degrees();
        let csr = g.csr();
        for i in 0..4u32 {
            assert_eq!(deg[i as usize], csr.degree(i));
        }
    }
}
