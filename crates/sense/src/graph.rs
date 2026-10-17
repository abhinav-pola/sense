//! Lays out a graph block's boxes and arrows: layered, in the manner of
//! Sugiyama, small enough to run on every render of a side-panel view.
//!
//! 1. Cycles are broken by reversing the edges a depth-first walk finds
//!    going back; they are still drawn pointing their own way.
//! 2. Each node goes on the layer one past its furthest predecessor.
//! 3. Edges that skip layers pass through a waypoint on each one between.
//! 4. Each layer is ordered by its neighbors' average position, sweeping
//!    down and up, to cut down crossings.
//! 5. Nodes move toward their neighbors without overlapping.
//!
//! Positions are in pixels, top left at the origin. "Main" is the axis edges
//! point along (down or right) and "cross" the other one.

use crate::view::Direction;

/// Space between neighbors on a layer.
const NODE_GAP: f32 = 16.;
/// Space between layers; more when edges carry labels.
const LAYER_GAP: f32 = 44.;
const LABELED_LAYER_GAP: f32 = 64.;
/// Space between the drawing and its edges.
pub const MARGIN: f32 = 4.;
/// How far a self loop reaches out of its node.
const LOOP_REACH: f32 = 14.;
/// How long waypoint passes take along the cross axis.
const WAYPOINT_WIDTH: f32 = 8.;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Route {
    /// From the source's edge to the target's, through any waypoints.
    /// Consecutive points join with a curve that leaves and enters along
    /// the main axis.
    pub points: Vec<Point>,
    /// A loop from a node back to itself: `points` are its start, the far
    /// point of the loop, and its end, joined by curves around the side.
    pub self_loop: bool,
    /// Where the edge's label goes.
    pub label_at: Point,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// Each node's top left corner, in the order given.
    pub nodes: Vec<Point>,
    /// One route per edge, in the order given.
    pub edges: Vec<Route>,
    pub width: f32,
    pub height: f32,
}

/// `sizes` are each node's width and height; `edges` index into them.
/// `labeled` leaves room between layers for edge labels.
pub fn layout(
    sizes: &[(f32, f32)],
    edges: &[(usize, usize)],
    direction: Direction,
    labeled: bool,
) -> Layout {
    let n = sizes.len();
    // Extent of each node along the cross and main axes.
    let (cross_size, main_size): (Vec<f32>, Vec<f32>) = sizes
        .iter()
        .map(|&(w, h)| match direction {
            Direction::Down => (w, h),
            Direction::Right => (h, w),
        })
        .unzip();

    // 1. Break cycles.
    let reversed = back_edges(n, edges);
    let dag: Vec<(usize, usize)> = edges
        .iter()
        .zip(&reversed)
        .filter(|((from, to), _)| from != to)
        .map(|(&(from, to), &back)| if back { (to, from) } else { (from, to) })
        .collect();

    // 2. Layers: longest path from the sources.
    let layer = longest_path_layers(n, &dag);
    let layer_count = layer.iter().max().map_or(0, |max| max + 1);

    // 3. Waypoints. Vertices 0..n are nodes; the rest are waypoints.
    let mut vertex_layer = layer.clone();
    let mut vertex_cross = cross_size.clone();
    // Per DAG edge, the vertex chain from its upper end to its lower end.
    let mut chains: Vec<Vec<usize>> = Vec::with_capacity(dag.len());
    for &(from, to) in &dag {
        let mut chain = vec![from];
        for l in layer[from] + 1..layer[to] {
            vertex_layer.push(l);
            vertex_cross.push(WAYPOINT_WIDTH);
            chain.push(vertex_layer.len() - 1);
        }
        chain.push(to);
        chains.push(chain);
    }
    let vertices = vertex_layer.len();
    let mut up: Vec<Vec<usize>> = vec![Vec::new(); vertices];
    let mut down: Vec<Vec<usize>> = vec![Vec::new(); vertices];
    for chain in &chains {
        for pair in chain.windows(2) {
            down[pair[0]].push(pair[1]);
            up[pair[1]].push(pair[0]);
        }
    }

    // 4. Order each layer.
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); layer_count];
    for v in 0..vertices {
        layers[vertex_layer[v]].push(v);
    }
    let mut rank = vec![0usize; vertices];
    let set_ranks = |layers: &Vec<Vec<usize>>, rank: &mut Vec<usize>| {
        for row in layers {
            for (ix, &v) in row.iter().enumerate() {
                rank[v] = ix;
            }
        }
    };
    set_ranks(&layers, &mut rank);
    let mut best = layers.clone();
    let mut best_crossings = crossings(&layers, &down, &rank);
    for sweep in 0..12 {
        let downward = sweep % 2 == 0;
        let order: Vec<usize> = if downward {
            (1..layer_count).collect()
        } else {
            (0..layer_count.saturating_sub(1)).rev().collect()
        };
        for l in order {
            let neighbors = if downward { &up } else { &down };
            let mut keyed: Vec<(f32, usize, usize)> = layers[l]
                .iter()
                .enumerate()
                .map(|(ix, &v)| {
                    let near = &neighbors[v];
                    let key = if near.is_empty() {
                        ix as f32
                    } else {
                        near.iter().map(|&u| rank[u] as f32).sum::<f32>() / near.len() as f32
                    };
                    (key, ix, v)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            layers[l] = keyed.into_iter().map(|(_, _, v)| v).collect();
            for (ix, &v) in layers[l].iter().enumerate() {
                rank[v] = ix;
            }
        }
        let count = crossings(&layers, &down, &rank);
        if count < best_crossings {
            best_crossings = count;
            best = layers.clone();
        }
    }
    let layers = best;
    set_ranks(&layers, &mut rank);

    // 5. Cross positions: centers, packed, then pulled toward neighbors.
    let mut center = vec![0f32; vertices];
    for row in &layers {
        let mut at = 0.;
        for &v in row {
            center[v] = at + vertex_cross[v] / 2.;
            at += vertex_cross[v] + NODE_GAP;
        }
    }
    let widest = layers
        .iter()
        .map(|row| row_extent(row, &vertex_cross))
        .fold(0., f32::max);
    for row in &layers {
        let offset = (widest - row_extent(row, &vertex_cross)) / 2.;
        for &v in row {
            center[v] += offset;
        }
    }
    for pass in 0..16 {
        let downward = pass % 2 == 0;
        let order: Vec<usize> = if downward {
            (0..layer_count).collect()
        } else {
            (0..layer_count).rev().collect()
        };
        for l in order {
            let row = &layers[l];
            let desired: Vec<f32> = row
                .iter()
                .map(|&v| {
                    let near: Vec<usize> = up[v].iter().chain(&down[v]).copied().collect();
                    if near.is_empty() {
                        center[v]
                    } else {
                        near.iter().map(|&u| center[u]).sum::<f32>() / near.len() as f32
                    }
                })
                .collect();
            let placed = separate(&desired, row, &vertex_cross);
            for (&v, at) in row.iter().zip(placed) {
                center[v] = at;
            }
        }
    }
    let min_cross = (0..vertices)
        .map(|v| center[v] - vertex_cross[v] / 2.)
        .fold(f32::INFINITY, f32::min);
    let has_loops = edges.iter().any(|(from, to)| from == to);
    for c in &mut center {
        *c += MARGIN - min_cross;
    }

    // Main positions: each layer as deep as its deepest node.
    let gap = if labeled {
        LABELED_LAYER_GAP
    } else {
        LAYER_GAP
    };
    let mut layer_start = vec![0f32; layer_count];
    let mut layer_depth = vec![0f32; layer_count];
    for v in 0..n {
        layer_depth[layer[v]] = layer_depth[layer[v]].max(main_size[v]);
    }
    let mut at = MARGIN;
    for l in 0..layer_count {
        layer_start[l] = at;
        at += layer_depth[l] + gap;
    }
    let main_extent = at - gap + MARGIN;
    // A node sits centered in its layer's depth.
    let main_top = |v: usize| -> f32 {
        if v < n {
            layer_start[layer[v]] + (layer_depth[layer[v]] - main_size[v]) / 2.
        } else {
            layer_start[vertex_layer[v]]
        }
    };
    let main_bottom = |v: usize| -> f32 {
        if v < n {
            main_top(v) + main_size[v]
        } else {
            layer_start[vertex_layer[v]] + layer_depth[vertex_layer[v]]
        }
    };

    // Ports: a node's edges spread along its side, ordered by where their
    // other end is, so arrows into one node don't stack on each other.
    let mut out_ports: Vec<Vec<(usize, f32)>> = vec![Vec::new(); n];
    let mut in_ports: Vec<Vec<(usize, f32)>> = vec![Vec::new(); n];
    for (ix, chain) in chains.iter().enumerate() {
        let (first, last) = (chain[0], chain[chain.len() - 1]);
        out_ports[first].push((ix, center[chain[1]]));
        in_ports[last].push((ix, center[chain[chain.len() - 2]]));
    }
    let port_of = |ports: &mut Vec<(usize, f32)>, node: usize| -> Vec<(usize, f32)> {
        ports.sort_by(|a, b| a.1.total_cmp(&b.1));
        let count = ports.len();
        let span = cross_size[node] * 0.6;
        ports
            .iter()
            .enumerate()
            .map(|(k, &(edge, _))| {
                let offset = if count <= 1 {
                    0.
                } else {
                    -span / 2. + span * k as f32 / (count - 1) as f32
                };
                (edge, center[node] + offset)
            })
            .collect()
    };
    let mut start_cross = vec![0f32; chains.len()];
    let mut end_cross = vec![0f32; chains.len()];
    for node in 0..n {
        for (edge, at) in port_of(&mut out_ports[node], node) {
            start_cross[edge] = at;
        }
        for (edge, at) in port_of(&mut in_ports[node], node) {
            end_cross[edge] = at;
        }
    }

    let point = |cross: f32, main: f32| match direction {
        Direction::Down => Point { x: cross, y: main },
        Direction::Right => Point { x: main, y: cross },
    };

    // Routes, in the edges' own order and direction.
    let mut dag_ix = 0;
    let mut routes = Vec::with_capacity(edges.len());
    for (&(from, to), &back) in edges.iter().zip(&reversed) {
        if from == to {
            // Out of the far cross side and back in, past the node's corner.
            let side = center[from] + cross_size[from] / 2.;
            let top = main_top(from);
            let depth = main_size[from];
            let start = point(side, top + depth * 0.3);
            let far = point(side + LOOP_REACH, top + depth * 0.5);
            let end = point(side, top + depth * 0.7);
            routes.push(Route {
                points: vec![start, far, end],
                self_loop: true,
                label_at: point(side + LOOP_REACH + 4., top + depth * 0.5),
            });
            continue;
        }
        let chain = &chains[dag_ix];
        let mut points = Vec::with_capacity(chain.len());
        points.push(point(start_cross[dag_ix], main_bottom(chain[0])));
        for &v in &chain[1..chain.len() - 1] {
            // A waypoint is a straight pass through its layer.
            points.push(point(center[v], main_top(v)));
            points.push(point(center[v], main_bottom(v)));
        }
        points.push(point(end_cross[dag_ix], main_top(chain[chain.len() - 1])));
        dag_ix += 1;
        if back {
            points.reverse();
        }
        // The label sits on the edge's middle gap between layers.
        let gaps = points.len() / 2;
        let a = points[(gaps / 2) * 2];
        let b = points[(gaps / 2) * 2 + 1];
        routes.push(Route {
            points,
            self_loop: false,
            label_at: Point {
                x: (a.x + b.x) / 2.,
                y: (a.y + b.y) / 2.,
            },
        });
    }

    let mut nodes = Vec::with_capacity(n);
    for v in 0..n {
        nodes.push(point(center[v] - cross_size[v] / 2., main_top(v)));
    }
    let mut cross_extent = (0..vertices)
        .map(|v| center[v] + vertex_cross[v] / 2.)
        .fold(0., f32::max)
        + MARGIN;
    if has_loops {
        cross_extent += LOOP_REACH + 4.;
    }
    let (width, height) = match direction {
        Direction::Down => (cross_extent, main_extent),
        Direction::Right => (main_extent, cross_extent),
    };
    Layout {
        nodes,
        edges: routes,
        width,
        height,
    }
}

/// Edges a depth-first walk in node order finds pointing back up its path.
fn back_edges(n: usize, edges: &[(usize, usize)]) -> Vec<bool> {
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (ix, &(from, _)) in edges.iter().enumerate() {
        out[from].push(ix);
    }
    // 0: unseen, 1: on the path, 2: done.
    let mut state = vec![0u8; n];
    let mut back = vec![false; edges.len()];
    for start in 0..n {
        if state[start] != 0 {
            continue;
        }
        // An explicit stack: (node, next out-edge to look at).
        let mut stack = vec![(start, 0usize)];
        state[start] = 1;
        while let Some(&mut (node, ref mut next)) = stack.last_mut() {
            if let Some(&edge) = out[node].get(*next) {
                *next += 1;
                let to = edges[edge].1;
                match state[to] {
                    0 => {
                        state[to] = 1;
                        stack.push((to, 0));
                    }
                    1 => back[edge] = true,
                    _ => {}
                }
            } else {
                state[node] = 2;
                stack.pop();
            }
        }
    }
    back
}

fn longest_path_layers(n: usize, dag: &[(usize, usize)]) -> Vec<usize> {
    let mut indegree = vec![0usize; n];
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(from, to) in dag {
        indegree[to] += 1;
        out[from].push(to);
    }
    let mut layer = vec![0usize; n];
    let mut ready: Vec<usize> = (0..n).rev().filter(|&v| indegree[v] == 0).collect();
    while let Some(v) = ready.pop() {
        for &to in &out[v] {
            layer[to] = layer[to].max(layer[v] + 1);
            indegree[to] -= 1;
            if indegree[to] == 0 {
                ready.push(to);
            }
        }
    }
    layer
}

/// Edge crossings between consecutive layers.
fn crossings(layers: &[Vec<usize>], down: &[Vec<usize>], rank: &[usize]) -> usize {
    let mut total = 0;
    for row in layers {
        let mut pairs: Vec<(usize, usize)> = Vec::new();
        for &v in row {
            for &w in &down[v] {
                pairs.push((rank[v], rank[w]));
            }
        }
        for i in 0..pairs.len() {
            for j in i + 1..pairs.len() {
                let (a, b) = (pairs[i], pairs[j]);
                if (a.0 < b.0 && a.1 > b.1) || (a.0 > b.0 && a.1 < b.1) {
                    total += 1;
                }
            }
        }
    }
    total
}

fn row_extent(row: &[usize], cross: &[f32]) -> f32 {
    row.iter().map(|&v| cross[v]).sum::<f32>() + NODE_GAP * row.len().saturating_sub(1) as f32
}

/// Centers as close to `desired` as a row allows: in order, never closer
/// than their sizes and the gap. Pushes right, then left, and meets in the
/// middle, then makes sure nothing overlaps.
fn separate(desired: &[f32], row: &[usize], cross: &[f32]) -> Vec<f32> {
    let k = row.len();
    if k == 0 {
        return Vec::new();
    }
    let min_gap = |i: usize| (cross[row[i - 1]] + cross[row[i]]) / 2. + NODE_GAP;
    let mut right = desired.to_vec();
    for i in 1..k {
        right[i] = right[i].max(right[i - 1] + min_gap(i));
    }
    let mut left = desired.to_vec();
    for i in (0..k - 1).rev() {
        left[i] = left[i].min(left[i + 1] - min_gap(i + 1));
    }
    let mut placed: Vec<f32> = right.iter().zip(&left).map(|(a, b)| (a + b) / 2.).collect();
    for i in 1..k {
        placed[i] = placed[i].max(placed[i - 1] + min_gap(i));
    }
    placed
}

#[cfg(test)]
mod tests {
    use super::{Direction, Layout, layout};

    fn boxes(layout: &Layout, sizes: &[(f32, f32)]) -> Vec<(f32, f32, f32, f32)> {
        layout
            .nodes
            .iter()
            .zip(sizes)
            .map(|(p, &(w, h))| (p.x, p.y, p.x + w, p.y + h))
            .collect()
    }

    fn assert_sound(layout: &Layout, sizes: &[(f32, f32)], edges: &[(usize, usize)]) {
        let rects = boxes(layout, sizes);
        for (i, a) in rects.iter().enumerate() {
            assert!(a.0 >= 0. && a.1 >= 0., "node {i} at {a:?}");
            assert!(
                a.2 <= layout.width && a.3 <= layout.height,
                "node {i} at {a:?} outside {}x{}",
                layout.width,
                layout.height
            );
            for (j, b) in rects.iter().enumerate().skip(i + 1) {
                let apart = a.2 <= b.0 || b.2 <= a.0 || a.3 <= b.1 || b.3 <= a.1;
                assert!(apart, "nodes {i} {a:?} and {j} {b:?} overlap");
            }
        }
        assert_eq!(layout.edges.len(), edges.len());
        for route in &layout.edges {
            assert!(route.points.len() >= 2);
            for p in &route.points {
                assert!(p.x.is_finite() && p.y.is_finite());
                assert!(
                    p.x <= layout.width + 0.5 && p.y <= layout.height + 0.5,
                    "{p:?}"
                );
            }
        }
    }

    #[test]
    fn a_chain_runs_down_one_column() {
        let sizes = [(80., 30.); 3];
        let edges = [(0, 1), (1, 2)];
        let laid = layout(&sizes, &edges, Direction::Down, false);
        assert_sound(&laid, &sizes, &edges);
        assert!(laid.nodes[0].y < laid.nodes[1].y && laid.nodes[1].y < laid.nodes[2].y);
        assert_eq!(laid.nodes[0].x, laid.nodes[1].x);
        // Each arrow leaves its source's bottom and ends on its target's top.
        let route = &laid.edges[0].points;
        assert_eq!(route[0].y, laid.nodes[0].y + 30.);
        assert_eq!(route[route.len() - 1].y, laid.nodes[1].y);
    }

    #[test]
    fn right_runs_across() {
        let sizes = [(80., 30.); 3];
        let edges = [(0, 1), (1, 2)];
        let laid = layout(&sizes, &edges, Direction::Right, false);
        assert_sound(&laid, &sizes, &edges);
        assert!(laid.nodes[0].x < laid.nodes[1].x && laid.nodes[1].x < laid.nodes[2].x);
        assert!(laid.width > laid.height);
    }

    #[test]
    fn a_long_edge_bends_around_the_layer_it_skips() {
        // 0 -> 1 -> 2 and 0 -> 2: the long edge gets waypoints.
        let sizes = [(80., 30.); 3];
        let edges = [(0, 1), (1, 2), (0, 2)];
        let laid = layout(&sizes, &edges, Direction::Down, false);
        assert_sound(&laid, &sizes, &edges);
        assert_eq!(laid.edges[0].points.len(), 2);
        assert_eq!(laid.edges[2].points.len(), 4);
        // Its waypoint stays clear of node 1.
        let pass = laid.edges[2].points[1];
        let (x0, x1) = (laid.nodes[1].x, laid.nodes[1].x + 80.);
        assert!(
            pass.x < x0 || pass.x > x1,
            "{pass:?} crosses node 1 at {x0}..{x1}"
        );
    }

    #[test]
    fn cycles_lay_out_and_keep_their_direction() {
        let sizes = [(80., 30.), (80., 30.), (80., 30.)];
        let edges = [(0, 1), (1, 2), (2, 0), (1, 1)];
        let laid = layout(&sizes, &edges, Direction::Down, true);
        assert_sound(&laid, &sizes, &edges);
        // The edge back from 2 to 0 starts at 2 and ends at 0.
        let back = &laid.edges[2].points;
        assert!(back[0].y <= laid.nodes[2].y + 30.0 && back[0].y >= laid.nodes[2].y);
        let end = back[back.len() - 1];
        assert!(end.y >= laid.nodes[0].y && end.y <= laid.nodes[0].y + 30.);
        assert!(laid.edges[3].self_loop);
    }

    #[test]
    fn a_fan_out_spreads_and_ports_differ() {
        let sizes = [(120., 30.), (60., 30.), (60., 30.), (60., 30.)];
        let edges = [(0, 1), (0, 2), (0, 3)];
        let laid = layout(&sizes, &edges, Direction::Down, false);
        assert_sound(&laid, &sizes, &edges);
        let starts: Vec<f32> = laid.edges.iter().map(|e| e.points[0].x).collect();
        assert!(
            starts[0] != starts[1] && starts[1] != starts[2],
            "{starts:?}"
        );
        // The parent sits over the middle child.
        let parent = laid.nodes[0].x + 60.;
        let middle = laid.nodes[2].x + 30.;
        assert!((parent - middle).abs() < 1., "{parent} vs {middle}");
    }

    #[test]
    fn ordering_removes_an_avoidable_crossing() {
        // a->d, b->c listed so the first order crosses.
        let sizes = [(60., 30.); 4];
        let edges = [(0, 3), (1, 2)];
        let laid = layout(&sizes, &edges, Direction::Down, false);
        assert_sound(&laid, &sizes, &edges);
        let (a, b, c, d) = (
            laid.nodes[0].x,
            laid.nodes[1].x,
            laid.nodes[2].x,
            laid.nodes[3].x,
        );
        assert_eq!(a < b, d < c, "a={a} b={b} c={c} d={d}");
    }

    #[test]
    fn disconnected_and_single_nodes_fit() {
        let sizes = [(50., 20.), (200., 60.), (10., 10.)];
        let laid = layout(&sizes, &[], Direction::Down, false);
        assert_sound(&laid, &sizes, &[]);
        let one = layout(&sizes[..1], &[], Direction::Right, false);
        assert_sound(&one, &sizes[..1], &[]);
    }

    #[test]
    fn a_larger_graph_stays_sound() {
        // A ladder with cross links, a back edge and a duplicate edge.
        let n = 24;
        let sizes: Vec<(f32, f32)> = (0..n).map(|i| (40. + (i % 5) as f32 * 20., 28.)).collect();
        let mut edges = Vec::new();
        for i in 0..n - 2 {
            edges.push((i, i + 2));
            if i % 3 == 0 {
                edges.push((i, i + 1));
            }
        }
        edges.push((n - 1, 0));
        edges.push((0, 2));
        for direction in [Direction::Down, Direction::Right] {
            let laid = layout(&sizes, &edges, direction, true);
            assert_sound(&laid, &sizes, &edges);
        }
    }
}
