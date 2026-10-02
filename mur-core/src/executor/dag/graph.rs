use super::*;

// ── Graph types ─────────────────────────────────────────────────────────────

#[derive(Debug)]
pub(super) struct DagNode {
    pub(super) step: ProcedureStep,
    /// Topological rank (0 = root, N = max ancestors to a root).
    pub(super) rank: usize,
}

#[derive(Debug)]
#[allow(dead_code)]
pub(super) struct DagGraph {
    pub(super) nodes: Vec<DagNode>,
    /// Mapping from step id → index in `nodes`.
    pub(super) id_to_idx: HashMap<String, usize>,
}

/// Validate a step list (resolvable `depends_on`, no cycles) without executing.
/// Used by fleet router-planning to reject an invalid plan and fall back.
pub(crate) fn validate_steps(steps: &[ProcedureStep]) -> Result<()> {
    build_dag(steps).map(|_| ())
}

/// Build the DAG: assign ids, validate depends_on, detect cycles, compute ranks.
pub(super) fn build_dag(steps: &[ProcedureStep]) -> Result<DagGraph> {
    // Assign default ids to steps without one.
    let steps: Vec<ProcedureStep> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| ProcedureStep {
            id: s.id.clone().or_else(|| Some(format!("s{i}"))),
            ..s.clone()
        })
        .collect();

    // Build id→index map.
    let id_to_idx: HashMap<String, usize> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.clone().unwrap(), i))
        .collect();

    // Validate depends_on — every referenced id must exist.
    for s in &steps {
        let sid = s.id.as_deref().unwrap_or("");
        for dep in &s.depends_on {
            if !id_to_idx.contains_key(dep) {
                anyhow::bail!(
                    "step `{sid}` depends_on unknown step `{dep}` — available ids: {:?}",
                    id_to_idx.keys().collect::<Vec<_>>()
                );
            }
        }
    }

    // Topo-sort via Kahn: compute in-degree.
    let n = steps.len();
    let mut in_degree = vec![0usize; n];
    let mut adj: Vec<Vec<usize>> = vec![vec![]; n];
    for s in steps.iter() {
        let i = id_to_idx[&s.id.clone().unwrap()];
        for dep in &s.depends_on {
            let d = id_to_idx[dep];
            adj[d].push(i);
            in_degree[i] += 1;
        }
    }

    // Kahn: start with all in-degree=0.
    let mut queue: Vec<usize> = (0..n).filter(|i| in_degree[*i] == 0).collect();
    let mut topo = Vec::with_capacity(n);
    while let Some(i) = queue.pop() {
        topo.push(i);
        for &next in &adj[i] {
            in_degree[next] -= 1;
            if in_degree[next] == 0 {
                queue.push(next);
            }
        }
    }

    if topo.len() != n {
        // Some steps weren't reachable → cycle.
        let unreachable: Vec<String> = (0..n)
            .filter(|i| !topo.contains(i))
            .map(|i| steps[i].id.clone().unwrap())
            .collect();
        anyhow::bail!(
            "cycle detected in workflow DAG — unreachable steps: {:?}",
            unreachable
        );
    }

    // Assign ranks: rank[i] = 0 + max(rank[dep] + 1) over depends_on.
    let mut rank = vec![0usize; n];
    for &i in &topo {
        for dep in &steps[i].depends_on {
            let d = id_to_idx[dep];
            rank[i] = rank[i].max(rank[d] + 1);
        }
    }

    let nodes: Vec<DagNode> = steps
        .into_iter()
        .enumerate()
        .map(|(i, step)| DagNode {
            step,
            rank: rank[i],
        })
        .collect();

    Ok(DagGraph { nodes, id_to_idx })
}
