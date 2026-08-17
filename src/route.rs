//! A 3D maze router for redstone dust.
//!
//! # Why a router at all
//!
//! The previous floorplan avoided routing entirely by giving every connection a
//! private row and column, which is collision-free but cannot express a
//! crossing. Real netlists need crossings, and the only place to put one in
//! Minecraft is a different Y layer. This router searches free space in three
//! dimensions, so crossings happen naturally by going over or under.
//!
//! # The wire model
//!
//! A *wire node* at `p` means dust at `p` sitting on a solid block at `p - Y`.
//! Two wire nodes conduct when they are orthogonally adjacent at the same Y, or
//! when they differ by one in Y and one in X or Z (a dust slope). A slope only
//! forms if the block directly above the lower dust is non-opaque, so every
//! node reserves the cell above itself and the router never fills it.
//!
//! # Keepout
//!
//! Dust shorts to anything it touches, so two different nets may not have wire
//! nodes that are adjacent or in a slope relationship. Rather than detect that
//! after the fact, the router refuses to place a node next to another net's
//! wire. That makes shorts structurally impossible instead of merely unlikely,
//! at the cost of routing density.

use crate::tech::MAX_RUN;
use crate::world::{down, offset, up, Block, Dir, Grid, Material, Pos};
use std::collections::{BinaryHeap, HashMap, HashSet};

/// Identifies which net owns a wire node. Distinct nets may never touch.
pub type NetId = u32;

/// Owner assigned to dust that was already in the grid when the router was
/// created - gate input pads and the like. It belongs to no route, but it must
/// still participate in keepout: a route running alongside a gate's pad would
/// short straight into that gate's input.
pub const PREPLACED: NetId = u32::MAX;

pub struct Router {
    /// Dust cells already placed, and which net owns them.
    owner: HashMap<Pos, NetId>,
    /// Cells no dust may ever occupy: cell bodies, and the clearance above
    /// every wire node.
    blocked: HashSet<Pos>,
    /// Cells set aside for one net's exclusive use. A feed stub entry has only
    /// about three legal approaches, and nothing otherwise stops a passing net
    /// from taking all of them - which walls the target in completely and sends
    /// the router off to burn its whole search budget looking for a way in.
    reserved: HashMap<Pos, NetId>,
    /// Endpoints deliberately attached to: gate outputs and feed stubs. Same-net
    /// keepout has to let a wire touch these, because connecting to them is the
    /// whole point; everywhere else a net must stay clear of itself.
    claimed: HashSet<Pos>,
    /// Cells temporarily barred while retrying a single route. Cleared between
    /// routes; this is what lets rip-up-and-retry escape a bad path shape.
    scratch: HashSet<Pos>,
    /// Search limits, to keep a hopeless route from running away.
    pub max_expansions: usize,
}

/// A route that has been found but not yet committed to the grid.
pub struct Path {
    pub nodes: Vec<Pos>,
}

/// Direction index for the search state: 0..4 are the compass directions and 4
/// means "no direction yet" at the start node.
type DirIdx = u8;

fn dir_index(d: Dir) -> DirIdx {
    match d {
        Dir::North => 0,
        Dir::South => 1,
        Dir::West => 2,
        Dir::East => 3,
    }
}

/// Penalty for changing direction. Without it, A* treats all flat moves alike,
/// so a diagonal route degenerates into a staircase of alternating X and Z
/// steps. That path is the same length but has no three collinear nodes, and a
/// repeater needs exactly that - so an unpenalised route can be impossible to
/// keep alive over distance.
const TURN_COST: i32 = 25;

#[derive(PartialEq, Eq)]
struct Frontier {
    /// Negated so `BinaryHeap` (a max-heap) pops the lowest cost first.
    priority: i32,
    cost: i32,
    pos: Pos,
    dir: DirIdx,
}

impl Ord for Frontier {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.priority
            .cmp(&other.priority)
            .then_with(|| self.pos.cmp(&other.pos))
            .then_with(|| self.dir.cmp(&other.dir))
    }
}
impl PartialOrd for Frontier {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Router {
    /// Seed the router from a grid that already contains placed cells.
    ///
    /// Everything present becomes an obstacle. Existing dust is additionally
    /// attributed to [`PREPLACED`] so that keepout applies to it: a route
    /// allowed to run alongside a gate's input pad would short into that gate.
    pub fn from_grid(grid: &Grid) -> Router {
        let mut r = Router {
            owner: HashMap::new(),
            claimed: HashSet::new(),
            reserved: HashMap::new(),
            blocked: HashSet::new(),
            scratch: HashSet::new(),
            max_expansions: 250_000,
        };
        for (&p, &b) in grid.iter() {
            if b != Block::Air {
                r.blocked.insert(p);
            }
            if matches!(b, Block::Dust { .. }) {
                // Give existing dust an owner so keepout keeps routes away from
                // it. `claim` overrides this for cell outputs and feed stubs,
                // which routes are supposed to attach to.
                r.owner.insert(p, PREPLACED);
                // Dust needs a full block under it. Block that cell even when it
                // is currently empty, or a route is free to run through it and
                // leave a repeater there - and dust resting on a repeater has no
                // support, so in game it pops off the moment it is placed while
                // this project's simulator happily models it as wire.
                //
                // That is not hypothetical: it is what killed the D latch's
                // fan-out leg to `r_gate`, which was correct in simulation and
                // dead in Minecraft.
                r.blocked.insert(crate::world::down(p));
            }
        }
        r
    }

    /// Which net owns each placed dust cell.
    ///
    /// Exposed so an audit can ask the one question keepout cannot answer from
    /// the grid alone: are two *different* nets electrically adjacent? Dust
    /// joins to dust, so two legs that merely pass close are one net in game.
    pub fn owners(&self) -> &HashMap<Pos, NetId> {
        &self.owner
    }

    /// Set `p` aside so only `net` may route through it.
    pub fn reserve(&mut self, p: Pos, net: NetId) {
        self.reserved.entry(p).or_insert(net);
    }

    /// Forbid any route from using `p`.
    pub fn block(&mut self, p: Pos) {
        self.blocked.insert(p);
    }

    /// Declare that the dust at `p` belongs to `net` (e.g. a cell's output or
    /// an input feed), so routes for that net may attach to it.
    pub fn claim(&mut self, p: Pos, net: NetId) {
        self.owner.insert(p, net);
        self.claimed.insert(p);
        self.blocked.remove(&p);
    }

    fn owned_by_other(&self, p: Pos, net: NetId) -> bool {
        matches!(self.owner.get(&p), Some(&o) if o != net)
    }

    /// Can `net` put a wire node at `q`?
    fn placeable(&self, grid: &Grid, q: Pos, net: NetId) -> bool {
        // `blocked` wins over ownership: a cell can belong to this net and still
        // be off limits, e.g. the repeater at the end of a feed stub.
        if self.blocked.contains(&q) || self.scratch.contains(&q) {
            return false;
        }
        if matches!(self.reserved.get(&q), Some(&r) if r != net) {
            return false;
        }
        // No cell is ever shared, not even between two fanout branches of the
        // same net. Sharing looks harmless electrically, but each branch plans
        // its repeaters independently, so the second one tries to drop a
        // repeater onto dust the first already placed. Keeping branches disjoint
        // costs area and buys a much simpler invariant.
        if self.owner.contains_key(&q) {
            return false;
        }
        if !grid.is_free(q) {
            return false;
        }
        // Substrate must be placeable solid, and must not be someone's dust.
        let sub = down(q);
        if self.owner.contains_key(&sub) {
            return false;
        }
        if !grid.is_free(sub) && !grid.get(sub).is_opaque() {
            return false;
        }
        // Clearance above, so slopes into this node can form.
        if !grid.is_free(up(q)) || self.owner.contains_key(&up(q)) {
            return false;
        }
        // Keepout: no *other* net's wire orthogonally adjacent or in a slope
        // relationship. Either would short the two nets together.
        //
        // A net must also stay clear of itself, everywhere except the endpoints
        // it is meant to attach to. Two fanout branches of one net running side
        // by side look harmless - same signal either way - but where one passes
        // beside the other's repeater it bridges that repeater's output back to
        // its input, and the net becomes a self-sustaining loop. That is a latch
        // in the middle of combinational logic, and it is what made `add2`
        // answer correctly from power-up and wrongly once it had been driven
        // into the second stable state.
        for d in Dir::ALL {
            let n = offset(q, d);
            for c in [n, up(n), down(n)] {
                if self.owned_by_other(c, net) {
                    return false;
                }
                if self.owner.contains_key(&c) && !self.claimed.contains(&c) {
                    return false;
                }
            }
        }
        // Same column, within two levels: the two nodes' substrate and
        // clearance cells would fight over the same block.
        for dy in [-2i32, -1, 1, 2] {
            if self.owner.contains_key(&(q.0, q.1 + dy, q.2)) {
                return false;
            }
        }
        true
    }

    /// Nodes reachable from `p` in one dust step: same level, or one up/down.
    fn neighbors(&self, p: Pos) -> Vec<(Pos, DirIdx)> {
        let mut out = Vec::with_capacity(12);
        for d in Dir::ALL {
            let n = offset(p, d);
            let i = dir_index(d);
            out.push((n, i));
            out.push((up(n), i));
            out.push((down(n), i));
        }
        out
    }

    /// Lower bound on remaining steps. A slope step covers one horizontal and
    /// one vertical block at once, so the bound is the larger of the two.
    fn heuristic(a: Pos, b: Pos) -> i32 {
        let horiz = (a.0 - b.0).abs() + (a.2 - b.2).abs();
        let vert = (a.1 - b.1).abs();
        horiz.max(vert)
    }

    /// Find a path from an existing wire node to a target cell.
    ///
    /// `slope_cost` prices a step that changes Y. Raising it makes the router
    /// prefer flat runs, which matters because **a repeater cannot sit on a
    /// slope**. A pure staircase descent has nowhere to refresh the signal, so
    /// a long one dies in transit; penalising slopes forces flat landings that
    /// [`Self::commit`] can put repeaters on.
    pub fn find(
        &self,
        grid: &Grid,
        net: NetId,
        sources: &[Pos],
        to: Pos,
        bounds: (Pos, Pos),
        slope_cost: i32,
    ) -> Result<Path, String> {
        let inside = |p: Pos| {
            p.0 >= bounds.0 .0
                && p.0 <= bounds.1 .0
                && p.1 >= bounds.0 .1
                && p.1 <= bounds.1 .1
                && p.2 >= bounds.0 .2
                && p.2 <= bounds.1 .2
        };

        // The search state is (position, incoming direction) so turns can be
        // priced; without that, routes zigzag and cannot hold repeaters.
        let mut came: HashMap<(Pos, DirIdx), (Pos, DirIdx)> = HashMap::new();
        let mut best: HashMap<(Pos, DirIdx), i32> = HashMap::new();
        let mut heap = BinaryHeap::new();
        // Every source is an equally valid place to leave from. Seeding all of
        // them lets a high-fanout driver spread its branches instead of forcing
        // every one through a single cell, which is the main cause of
        // congestion right at the driver.
        for &src in sources {
            best.insert((src, 4), 0);
            heap.push(Frontier { priority: -Self::heuristic(src, to), cost: 0, pos: src, dir: 4 });
        }

        let mut expansions = 0usize;
        while let Some(Frontier { cost, pos, dir, .. }) = heap.pop() {
            if pos == to {
                let mut nodes = vec![to];
                let mut cur = (to, dir);
                while let Some(&prev) = came.get(&cur) {
                    nodes.push(prev.0);
                    cur = prev;
                }
                nodes.reverse();
                return Ok(Path { nodes });
            }
            if cost > *best.get(&(pos, dir)).unwrap_or(&i32::MAX) {
                continue;
            }
            expansions += 1;
            if expansions > self.max_expansions {
                return Err(format!("router gave up after {expansions} expansions"));
            }

            for (q, qdir) in self.neighbors(pos) {
                if !inside(q) {
                    continue;
                }
                // The destination is pre-claimed by this net, so it is exempt
                // from the no-sharing rule; everything else must be virgin.
                let ok = if q == to {
                    !self.blocked.contains(&q) && !self.scratch.contains(&q)
                } else {
                    self.placeable(grid, q, net)
                };
                if !ok {
                    continue;
                }
                // Level moves are cheapest; slopes and turns are discouraged.
                let mut step = if q.1 == pos.1 { 10 } else { slope_cost };
                if dir != 4 && dir != qdir {
                    step += TURN_COST;
                }
                let next = cost + step;
                let key = (q, qdir);
                if next < *best.get(&key).unwrap_or(&i32::MAX) {
                    best.insert(key, next);
                    came.insert(key, (pos, dir));
                    heap.push(Frontier {
                        priority: -(next + Self::heuristic(q, to) * 10),
                        cost: next,
                        pos: q,
                        dir: qdir,
                    });
                }
            }
        }
        Err(format!("no route to {to:?} from any of {} source(s)", sources.len()))
    }

    /// A path must not collide with itself.
    ///
    /// Each wire node claims three cells in its column: the dust, the substrate
    /// below it, and the slope clearance above it. So two nodes in the same
    /// `(x, z)` column must differ in Y by at least **three**. The subtle case is
    /// a gap of exactly two: the upper node's substrate lands in the lower
    /// node's clearance cell, silently breaking the slope that was supposed to
    /// connect into it. The A* explores states rather than whole paths, so it
    /// cannot see this; catching it here lets `route` retry with a different
    /// shape instead of emitting a circuit that is subtly disconnected.
    fn first_conflict(path: &[Pos]) -> Option<Pos> {
        let mut columns: HashMap<(i32, i32), Vec<i32>> = HashMap::new();
        for &p in path {
            columns.entry((p.0, p.2)).or_default().push(p.1);
        }
        for (&(x, z), ys) in columns.iter_mut() {
            ys.sort_unstable();
            for w in ys.windows(2) {
                if w[1] - w[0] < 3 {
                    return Some((x, w[1], z));
                }
            }
        }

        // A path must not touch itself.
        //
        // Keepout only rejects adjacency to *other* nets, and a path's own
        // cells are not in `owner` while it is being searched, so nothing
        // stopped a wire running alongside itself one cell over. Where it does
        // that around one of its own repeaters, the parallel run bridges the
        // repeater's output back to its input and the net becomes a
        // self-sustaining loop - a latch in the middle of combinational logic.
        //
        // That is what made `add2` history-dependent: correct from power-up,
        // and stuck in a second stable state once it had been driven there.
        let index: HashMap<Pos, usize> = path.iter().enumerate().map(|(i, &p)| (p, i)).collect();
        for (i, &p) in path.iter().enumerate() {
            for d in Dir::ALL {
                let n = offset(p, d);
                // Dust joins across a one-block step as well as flat.
                for c in [n, up(n), down(n)] {
                    if let Some(&j) = index.get(&c) {
                        // Consecutive nodes are meant to touch; anything else is
                        // the wire shorting to a different part of itself.
                        if i.abs_diff(j) > 1 {
                            return Some(c);
                        }
                    }
                }
            }
        }
        None
    }

    /// Is `i` a legal repeater site? A repeater conducts along one axis only,
    /// so it needs the node before and after it to be collinear and at the same
    /// Y - i.e. it must sit in the middle of a straight, flat run.
    fn repeater_site(path: &[Pos], i: usize) -> bool {
        if i == 0 || i + 1 >= path.len() {
            return false;
        }
        let (prev, p, next) = (path[i - 1], path[i], path[i + 1]);
        p.1 == prev.1
            && p.1 == next.1
            && (next.0 - p.0, next.2 - p.2) == (p.0 - prev.0, p.2 - prev.2)
    }

    /// Choose repeater positions along a path, or explain why it cannot be done.
    ///
    /// Walks the route and, whenever the signal is about to decay past the
    /// budget, backtracks to the *last* legal site seen. Greedy-latest keeps the
    /// repeater count minimal while guaranteeing the budget is never exceeded.
    fn plan_repeaters(path: &[Pos], initial_decay: i32) -> Result<Vec<usize>, String> {
        let mut sites = Vec::new();
        // The route may start partway along a driver's output spine, which has
        // already eaten into the signal budget.
        let mut since = initial_decay;
        let mut last_site: Option<usize> = None;
        for i in 1..path.len() {
            since += 1;
            if Self::repeater_site(path, i) {
                last_site = Some(i);
            }
            if since >= MAX_RUN {
                let site = last_site.ok_or_else(|| {
                    format!("route ran {since} blocks with no flat run to hold a repeater")
                })?;
                sites.push(site);
                since = (i - site) as i32;
                last_site = None;
                if since >= MAX_RUN {
                    return Err("repeater sites are too far apart".to_string());
                }
            }
        }
        Ok(sites)
    }

    /// Commit a path: place substrate and dust, insert the planned repeaters,
    /// and reserve keepout around the whole run.
    pub fn commit(
        &mut self,
        grid: &mut Grid,
        net: NetId,
        path: &Path,
        material: Material,
        initial_decay: i32,
    ) -> Result<(), String> {
        let sites = Self::plan_repeaters(&path.nodes, initial_decay)?;
        // `nodes[0]` is the existing driver dust; everything after is new.
        for i in 1..path.nodes.len() {
            let p = path.nodes[i];
            let prev = path.nodes[i - 1];
            // Reuse any opaque block already there (a cell roof, another
            // route's substrate) rather than trying to replace it.
            if grid.is_free(down(p)) {
                grid.set(down(p), Block::Solid(material))?;
            } else if !grid.get(down(p)).is_opaque() {
                // Dust needs a full block under it. The search rejects cells
                // whose support is neither free nor opaque, but the *target* is
                // exempt from that check, so a target whose support cell was
                // taken by an earlier route - by a repeater, typically - used to
                // get dust laid on nothing. In game that dust pops off the
                // instant it is placed and the wire silently does not exist,
                // while this project's simulator models it as ordinary wire and
                // reports the circuit working. Fail loudly instead.
                return Err(format!(
                    "no support for dust at {p:?}: {:?} sits below it",
                    grid.get(down(p))
                ));
            }
            if sites.contains(&i) {
                let dir = if p.0 > prev.0 {
                    Dir::West
                } else if p.0 < prev.0 {
                    Dir::East
                } else if p.2 > prev.2 {
                    Dir::North
                } else {
                    Dir::South
                };
                grid.set(p, Block::Repeater { facing: dir, delay: 1, powered: false })?;
            } else {
                grid.set(p, Block::Dust { power: 0 })?;
            }
            self.owner.insert(p, net);
            self.blocked.insert(up(p));
        }
        Ok(())
    }

    /// Route a net, retrying until the path is physically buildable.
    ///
    /// Two things can make an otherwise valid A* result unusable: the path may
    /// collide with itself (see [`Self::first_conflict`]), or it may be a pure
    /// staircase with nowhere to put a repeater. Neither is visible from inside
    /// the search, which explores states rather than whole paths.
    ///
    /// So we do what real routers do: find a path, check it, and on failure bar
    /// the offending cell and search again. Escalating the slope penalty in
    /// parallel pushes the router towards flat runs, which is what repeaters
    /// need. This is rip-up-and-retry at the granularity of a single cell.
    pub fn route(
        &mut self,
        grid: &mut Grid,
        net: NetId,
        sources: &[Pos],
        to: Pos,
        bounds: (Pos, Pos),
        material: Material,
        initial_decay: i32,
    ) -> Result<(), String> {
        let from = sources[0];
        self.scratch.clear();
        let slopes = [14, 40, 120, 400];
        let mut last_err = String::from("no attempt made");
        for attempt in 0..24 {
            let slope_cost = slopes[(attempt / 6).min(slopes.len() - 1)];
            // Search a box around the two endpoints rather than the whole
            // build. Most routes are local, and bounding the volume is the
            // difference between thousands of expansions and hundreds of
            // thousands. The margin grows on retry so a congested route can
            // still detour widely.
            // The margin must be at least the vertical drop. Dust descends one
            // block of Y per block of horizontal travel, so a route that falls
            // N levels needs N blocks of horizontal room. Give it less and the
            // only way down is a switchback, which lands a wire two levels above
            // its own substrate and breaks the slope (see `first_conflict`).
            let drop = (from.1 - to.1).abs();
            let margin = 10 + drop + attempt as i32 * 5;
            let local = (
                (
                    (from.0.min(to.0) - margin).max(bounds.0 .0),
                    (from.1.min(to.1) - margin).max(bounds.0 .1),
                    (from.2.min(to.2) - margin).max(bounds.0 .2),
                ),
                (
                    (from.0.max(to.0) + margin).min(bounds.1 .0),
                    (from.1.max(to.1) + margin).min(bounds.1 .1),
                    (from.2.max(to.2) + margin).min(bounds.1 .2),
                ),
            );
            match self.find(grid, net, sources, to, local, slope_cost) {
                Ok(path) => {
                    if let Some(bad) = Self::first_conflict(&path.nodes) {
                        last_err = "path collides with itself".to_string();
                        self.scratch.insert(bad);
                        continue;
                    }
                    match Self::plan_repeaters(&path.nodes, initial_decay) {
                        Ok(_) => {
                            self.scratch.clear();
                            return self.commit(grid, net, &path, material, initial_decay);
                        }
                        Err(e) => {
                            last_err = e;
                            // Bar a midpoint so the next search takes a
                            // different shape with somewhere flat to refresh.
                            if path.nodes.len() > 2 {
                                self.scratch.insert(path.nodes[path.nodes.len() / 2]);
                            }
                        }
                    }
                }
                Err(e) => last_err = e,
            }
        }
        self.scratch.clear();
        Err(format!("{last_err}; {}", self.diagnose(grid, net, sources, to)))
    }

    /// Why did a route fail? Distinguishes "the target is walled in" from "the
    /// search space is too big" - opposite problems, indistinguishable from the
    /// generic message, and the difference between fixing this in one step and
    /// guessing for an afternoon.
    fn diagnose(&self, grid: &Grid, net: NetId, sources: &[Pos], to: Pos) -> String {
        let open_at = |p: Pos| {
            let mut n = 0;
            for d in Dir::ALL {
                for q in [offset(p, d), up(offset(p, d)), down(offset(p, d))] {
                    if self.placeable(grid, q, net) {
                        n += 1;
                    }
                }
            }
            n
        };
        let src_open: usize = sources.iter().map(|&s| open_at(s)).sum();
        let dst_open = open_at(to);
        let from = sources[0];
        let span = (from.0 - to.0).abs() + (from.1 - to.1).abs() + (from.2 - to.2).abs();
        format!(
            "target {to:?} has {dst_open}/12 open approaches, \
             {} source(s) have {src_open} open exits, manhattan span {span}",
            sources.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redstone::Sim;
    use crate::world::Face;

    fn bounds() -> (Pos, Pos) {
        ((-40, -20, -40), (40, 20, 40))
    }

    /// Place a lever driving a single dust node the router can start from.
    fn source(g: &mut Grid, r: &mut Router, p: Pos, net: NetId) -> Pos {
        g.force(down(p), Block::Solid(Material::Wire));
        g.force(p, Block::Dust { power: 0 });
        let lever = (p.0, p.1, p.2 - 1);
        g.force(down(lever), Block::Solid(Material::PortIn));
        g.force(lever, Block::Lever { face: Face::Floor, facing: Dir::North, powered: false });
        r.claim(p, net);
        lever
    }

    #[test]
    fn routes_a_straight_line_and_carries_signal() {
        let mut g = Grid::new();
        let mut r = Router::from_grid(&g);
        let lever = source(&mut g, &mut r, (0, 0, 0), 1);
        r.route(&mut g, 1, &[(0, 0, 0)], (0, 0, 10), bounds(), Material::Wire, 0).unwrap();

        let mut sim = Sim::new(&g);
        sim.set_lever(lever, true);
        let (_, stable) = sim.run_until_stable(200);
        assert!(stable);
        assert!(sim.field().dust_at((0, 0, 10)) > 0, "signal must reach the far end");
    }

    /// Longer than the dust budget: the router has to insert repeaters.
    #[test]
    fn long_route_is_repeated() {
        let mut g = Grid::new();
        let mut r = Router::from_grid(&g);
        let lever = source(&mut g, &mut r, (0, 0, 0), 1);
        r.route(&mut g, 1, &[(0, 0, 0)], (0, 0, 35), bounds(), Material::Wire, 0).unwrap();

        let mut sim = Sim::new(&g);
        sim.set_lever(lever, true);
        let (_, stable) = sim.run_until_stable(400);
        assert!(stable);
        assert!(sim.field().dust_at((0, 0, 35)) > 0, "35-block route must survive");
    }

    /// The whole point: two nets whose straight paths would intersect must both
    /// route, and must stay electrically separate.
    #[test]
    fn crossing_nets_do_not_short() {
        let mut g = Grid::new();
        let mut r = Router::from_grid(&g);
        // Net 1 runs west to east, net 2 runs north to south, crossing at (5,_,5).
        let l1 = source(&mut g, &mut r, (0, 0, 5), 1);
        r.route(&mut g, 1, &[(0, 0, 5)], (12, 0, 5), bounds(), Material::Wire, 0).unwrap();

        g.force((5, -1, 0), Block::Solid(Material::Wire));
        g.force((5, 0, 0), Block::Dust { power: 0 });
        let l2 = (5, 0, -1);
        g.force(down(l2), Block::Solid(Material::PortIn));
        g.force(l2, Block::Lever { face: Face::Floor, facing: Dir::North, powered: false });
        r.claim((5, 0, 0), 2);
        r.route(&mut g, 2, &[(5, 0, 0)], (5, 0, 12), bounds(), Material::Wire, 0)
            .expect("second net must find a way over or under the first");

        let mut sim = Sim::new(&g);
        for (a, b) in [(false, false), (true, false), (false, true), (true, true)] {
            sim.set_lever(l1, a);
            sim.set_lever(l2, b);
            let (_, stable) = sim.run_until_stable(400);
            assert!(stable, "must settle for ({a}, {b})");
            let f = sim.field();
            assert_eq!(f.dust_at((12, 0, 5)) > 0, a, "net 1 must carry only its own signal");
            assert_eq!(f.dust_at((5, 0, 12)) > 0, b, "net 2 must carry only its own signal");
        }
    }

    #[test]
    fn reports_failure_when_boxed_in() {
        let mut g = Grid::new();
        // Seal the destination inside solid blocks.
        for dx in -1..=1i32 {
            for dy in -1..=1i32 {
                for dz in -1..=1i32 {
                    if (dx, dy, dz) != (0, 0, 0) {
                        g.force((10 + dx, dy, 10 + dz), Block::Solid(Material::Shield));
                    }
                }
            }
        }
        let mut r = Router::from_grid(&g);
        source(&mut g, &mut r, (0, 0, 0), 1);
        let err = r
            .route(&mut g, 1, &[(0, 0, 0)], (10, 0, 10), bounds(), Material::Wire, 0)
            .unwrap_err();
        assert!(err.contains("no route") || err.contains("gave up"), "{err}");
    }
}
