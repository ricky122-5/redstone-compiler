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

#[derive(Clone)]
pub struct Router {
    /// How contested each cell has proved across routing attempts.
    ///
    /// This is the "history" half of negotiated congestion. Rip-up perturbs one
    /// connection's neighbourhood at a time and is blind to the fact that the
    /// same few corridors get fought over by everything - so an early net takes
    /// a corridor, later nets fail around it, and re-routing them changes
    /// nothing because the corridor is still the only way through. Charging for
    /// a cell in proportion to how often it has been contested makes later
    /// attempts spread out instead of queueing for the same ground.
    ///
    /// Empty by default, so a single-pass placement behaves exactly as before.
    history: HashMap<Pos, i32>,
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
    /// Cells beside a lever's support block, mapped to that lever. A powered
    /// lever strongly powers its support, and the support energises every dust
    /// cell beside it - so only the net that actually attaches to the lever may
    /// put wire in these cells.
    lever_hazard: HashMap<Pos, Pos>,
    /// The endpoints of the route currently being searched: its sources and
    /// its target. The same-net keepout exemption applies to these and nothing
    /// else. It used to apply to every `claimed` cell, which let a route run
    /// alongside a *relay* of its own net - relay ends are claimed - and bridge
    /// the relay's input to its output around the repeater. That ring is a
    /// self-sustaining loop: the same latch-in-combinational-logic disease as
    /// the add2 bug, one mechanism further along.
    active_endpoints: HashSet<Pos>,
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

/// Cheapest a slope step can be, and what the heuristic charges for each level
/// it still has to descend. Must not exceed the smallest value on `route`'s
/// slope ladder or the bound stops being admissible.
const SLOPE_FLOOR_DEFAULT: i32 = 14;
fn slope_floor() -> i32 {
    std::env::var("OHMC_SF").ok().and_then(|v| v.parse().ok()).unwrap_or(SLOPE_FLOOR_DEFAULT)
}

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
            lever_hazard: HashMap::new(),
            active_endpoints: HashSet::new(),
            claimed: HashSet::new(),
            reserved: HashMap::new(),
            blocked: HashSet::new(),
            scratch: HashSet::new(),
            history: HashMap::new(),
            max_expansions: 250_000,
        };
        for (&p, &b) in grid.iter() {
            if b != Block::Air {
                r.blocked.insert(p);
            }
            // Levers and torches energise every dust cell orthogonally beside
            // them, exactly as a wire does, so keepout has to see them too.
            // Marking only dust left a hole: a route could run right alongside
            // an input lever or a gate's torch and be driven to full strength by
            // it, with no wire anywhere between them.
            //
            // That is what broke `add.ohm`. One gate's input pad read 12 while
            // its own driver output 0, and following the dust levels back led to
            // a cell sitting beside an input lever fifteen levels up. Whole
            // high-order sum bits were wrong because the carry chain was being
            // fed by a lever it merely passed by.
            // A lever energises every dust cell orthogonally beside it, so no
            // wire may *occupy* one of those cells. Block them rather than
            // giving the lever an owner: ownership keeps routes two cells clear,
            // which walls in the lever row and stops `add` routing at all, while
            // blocking is the exact constraint - do not sit here.
            //
            // The intended tap is one of those neighbours and is already
            // blocked, like every placed cell; `claim` unblocks it when a route
            // legitimately starts there.
            //
            // This is what broke `add.ohm`: a gate's input pad read 12 while its
            // own driver output 0, and following the dust levels back ended at a
            // cell sitting beside an input lever fifteen levels above. The carry
            // chain was being fed by a lever it merely passed.
            if let Block::Lever { face, facing, .. } = b {
                for d in Dir::ALL {
                    r.blocked.insert(offset(p, d));
                }
                // The block the lever is attached to is *strongly* powered
                // whenever the lever is on, and a strongly powered block
                // energises every dust cell beside it. In add.ohm a route ran
                // one level down, orthogonally beside a lever's support block,
                // and read 15 whenever that unrelated input was on - correct
                // whenever the neighbouring lever happened to be off, which is
                // why half the sampled cases passed.
                //
                // These cells cannot simply be blocked: the lever's *own* net
                // legitimately descends right past its support, and hard
                // blocking the ring walls off the row and add stops routing at
                // all. So they go in a hazard map instead, and `placeable`
                // rejects them only for nets that do not attach to this lever.
                let support = match face {
                    crate::world::Face::Floor => crate::world::down(p),
                    crate::world::Face::Ceiling => up(p),
                    crate::world::Face::Wall => offset(p, facing.opposite()),
                };
                for c in Dir::ALL
                    .iter()
                    .map(|&d| offset(support, d))
                    .chain([crate::world::down(support)])
                {
                    if c != p {
                        r.lever_hazard.insert(c, p);
                    }
                }
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
    /// Can `net` stamp a relay (dust, repeater, dust along +Z) at `pos`?
    ///
    /// Relays are written with direct `grid.set`, not routed, so nothing else
    /// ever asks whether the site is clear. That is how `add.ohm` ended up
    /// wrong: a relay's input dust was stamped diagonally adjacent to another
    /// net's descending wire, joining net 41 to net 42 - the placement passed
    /// every geometric audit because each wire was individually legal, and the
    /// junction was created by the stamp that never looked.
    pub fn relay_site_clear(&self, grid: &Grid, pos: Pos, net: NetId) -> bool {
        let inp = (pos.0, pos.1, pos.2 - 1);
        let out = (pos.0, pos.1, pos.2 + 1);
        // The dust ends get the full wire-placement rule.
        if !self.placeable(grid, inp, net) || !self.placeable(grid, out, net) {
            return false;
        }
        // The repeater cell: free, not reserved, and nothing foreign beside it.
        if self.blocked.contains(&pos) || self.scratch.contains(&pos) || !grid.is_free(pos) {
            return false;
        }
        if matches!(self.reserved.get(&pos), Some(&r) if r != net) {
            return false;
        }
        for d in Dir::ALL {
            let n = offset(pos, d);
            if self.owned_by_other(n, net)
                || self.owned_by_other(up(n), net)
                || self.owned_by_other(down(n), net)
            {
                return false;
            }
        }
        // The signal has to be able to leave *and* to arrive. A site whose own
        // three cells are clear can still be walled in just past either end:
        // walled past the output, the next stage fails with nowhere to start;
        // walled past the input, the route *into* the relay fails instead, and
        // that is the harder failure to read - the router burns its whole budget
        // getting within a few blocks of a target it can never touch.
        //
        // Only the exit was checked, so the entrance side was chosen blind. In
        // `tick` that is the whole ballgame: relays crowd into a shared band, a
        // site's three cells are clear because it is the last hole in a wall,
        // and the route in cannot reach it. Insist on both.
        let room = |c: Pos| {
            Dir::ALL
                .iter()
                .flat_map(|&d| {
                    let n = offset(c, d);
                    [n, up(n), down(n)]
                })
                .filter(|&q| q != pos && self.placeable(grid, q, net))
                .count()
        };
        room(out) >= 2 && room(inp) >= 2
    }

    /// How much elbow room a relay site has, or `None` if it is not usable at
    /// all.
    ///
    /// [`Self::relay_site_clear`] answers yes-or-no, and taking the first yes is
    /// how chains strangle each other: a site with the bare minimum of two
    /// approaches is fine until the route *into* it uses one of them, and then
    /// the next stage has nowhere to start. The caller can compare candidates
    /// instead of accepting the first.
    pub fn relay_site_room(&self, grid: &Grid, pos: Pos, net: NetId) -> Option<usize> {
        if !self.relay_site_clear(grid, pos, net) {
            return None;
        }
        let inp = (pos.0, pos.1, pos.2 - 1);
        let out = (pos.0, pos.1, pos.2 + 1);
        let room = |c: Pos| {
            Dir::ALL
                .iter()
                .flat_map(|&d| {
                    let n = offset(c, d);
                    [n, up(n), down(n)]
                })
                .filter(|&q| q != pos && self.placeable(grid, q, net))
                .count()
        };
        Some(room(inp).min(room(out)))
    }

    /// Remove everything `net` routed, freeing the space for someone else.
    ///
    /// Endpoints stay: gate outputs, feed stubs and relay ends are structure,
    /// not routing, and the connection will want them again when it is redone.
    /// Returns the cells released.
    pub fn rip(&mut self, grid: &mut Grid, net: NetId) -> Vec<Pos> {
        let doomed: Vec<Pos> = self
            .owner
            .iter()
            .filter(|(p, &o)| o == net && !self.claimed.contains(p))
            .map(|(&p, _)| p)
            .collect();
        for &p in &doomed {
            self.owner.remove(&p);
            grid.clear(p);
            self.blocked.remove(&p);
        }
        doomed
    }

    /// Nets with wire within `r` of `p`, nearest first. These are the ones
    /// worth ripping when a route cannot reach `p`.
    pub fn crowders(&self, p: Pos, r: i32, exclude: NetId) -> Vec<NetId> {
        let mut by_dist: Vec<(i32, NetId)> = Vec::new();
        for (&c, &o) in &self.owner {
            if o == exclude || o == PREPLACED {
                continue;
            }
            let d = (c.0 - p.0).abs() + (c.1 - p.1).abs() + (c.2 - p.2).abs();
            if d <= r {
                by_dist.push((d, o));
            }
        }
        by_dist.sort();
        let mut out = Vec::new();
        for (_, n) in by_dist {
            if !out.contains(&n) {
                out.push(n);
            }
        }
        out
    }

    /// Charge a neighbourhood for having failed to admit a route.
    ///
    /// Called between passes, on the target and sources of a connection that
    /// could not be placed. The penalty decays with distance so the blame
    /// falls hardest where the search actually stalled.
    pub fn blame(&mut self, centre: Pos, radius: i32, weight: i32) {
        for dx in -radius..=radius {
            for dy in -radius..=radius {
                for dz in -radius..=radius {
                    let d = dx.abs() + dy.abs() + dz.abs();
                    if d > radius {
                        continue;
                    }
                    let p = (centre.0 + dx, centre.1 + dy, centre.2 + dz);
                    *self.history.entry(p).or_insert(0) += weight * (radius - d + 1) / (radius + 1);
                }
            }
        }
    }

    /// Carry accumulated contention into a fresh router for the next pass.
    pub fn inherit_history(&mut self, from: &Router) {
        self.history = from.history.clone();
    }

    pub fn history_len(&self) -> usize {
        self.history.len()
    }

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
        // Beside a lever's support block: allowed only for the net that
        // attaches to that lever, identified by owning a cell next to it.
        if let Some(&lev) = self.lever_hazard.get(&q) {
            let attached = Dir::ALL
                .iter()
                .map(|&d| offset(lev, d))
                .chain([up(lev), down(lev)])
                .any(|n| matches!(self.owner.get(&n), Some(&o) if o == net));
            if !attached {
                return false;
            }
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
                if self.owner.contains_key(&c) && !self.active_endpoints.contains(&c) {
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

    /// Lower bound on the remaining *cost*, not on the remaining steps.
    ///
    /// A slope step covers one horizontal and one vertical block at once, so at
    /// least `vert` of the remaining steps must be slopes and at least
    /// `horiz - vert` of them must be flat. Pricing them at what they actually
    /// cost - `slope_cost` and 10 - gives a much tighter bound than counting
    /// steps and multiplying by the cheapest one.
    ///
    /// The vertical price is a fixed floor, not the caller's current
    /// `slope_cost`. Using the live value looks tighter and destroys the retry
    /// ladder: [`Self::route`] escalates `slope_cost` to push a route towards
    /// flat runs that a repeater can sit on, and a heuristic that escalates with
    /// it cancels the push exactly, leaving the search just as happy with a pure
    /// staircase at a slope cost of 400 as at 14. Every combinational design
    /// stopped placing, all with "no flat run to hold a repeater". Charging the
    /// floor keeps the bound admissible and leaves the ladder its teeth.
    ///
    /// Tightness is not the point, though; the *shape* is. A slope costs more
    /// than a flat step, so a search that does not charge for the descent it
    /// still owes will always put it off, running flat towards the target and
    /// trying to fall at the end. That is precisely how `tick` failed: the
    /// router arrived a couple of blocks from a gate's feed with six levels
    /// still to drop and three blocks of room to drop them in, and dust falls
    /// one level per block travelled. Charging the descent up front makes an
    /// early descent and a late one cost the same, so the search stops deferring
    /// it.
    fn heuristic(a: Pos, b: Pos) -> i32 {
        let horiz = (a.0 - b.0).abs() + (a.2 - b.2).abs();
        let vert = (a.1 - b.1).abs();
        vert * slope_floor() + (horiz - vert).max(0) * 10
    }

    /// Find a path from an existing wire node to a target cell.
    ///
    /// `weight` inflates the heuristic in tenths: 10 leaves it exact, 13 makes
    /// it 1.3x. At 10 the search is exact A*, and in open space that is a
    /// disaster: every
    /// monotone path to the target then has the same f-score, so the search has
    /// no reason to prefer any of them and enumerates the lot. `tick` failed a
    /// 111-block hop through a volume a dump showed was *completely empty*,
    /// having spent 250,000 expansions on ties - a failure that reads exactly
    /// like congestion and is its opposite. Above 10 the ties break toward the
    /// target and the search runs greedily where nothing is in the way, at the
    /// price of paths that may be longer than optimal.
    ///
    /// Greedy is not free, though, and it cannot simply be turned on: inside the
    /// D latch a greedy route takes a wasteful path through a macro that has no
    /// room to waste, walls in the next route's source, and the macro stops
    /// building at all. So the caller ladders it - see [`Self::route`].
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
        weight: i32,
        budget: usize,
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
            heap.push(Frontier {
                priority: -Self::heuristic(src, to) * weight / 10,
                cost: 0,
                pos: src,
                dir: 4,
            });
        }

        let mut expansions = 0usize;
        // How close the search ever got, so a failure can say whether it was
        // walled in near the target or simply wandering. "Gave up after N
        // expansions" does not distinguish those, and they need opposite fixes.
        let mut closest = i32::MAX;
        let mut closest_at = to;
        while let Some(Frontier { cost, pos, dir, .. }) = heap.pop() {
            let h = Self::heuristic(pos, to);
            if h < closest {
                closest = h;
                closest_at = pos;
            }
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
            if expansions > budget {
                return Err(format!("router gave up after {expansions} expansions (closest approach {closest} at {closest_at:?})"));
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
                // Contested ground costs more. Zero unless a previous pass
                // recorded a failure here, so this is inert on a single pass.
                if let Some(&h) = self.history.get(&q) {
                    step += h;
                }
                let next = cost + step;
                let key = (q, qdir);
                if next < *best.get(&key).unwrap_or(&i32::MAX) {
                    best.insert(key, next);
                    came.insert(key, (pos, dir));
                    heap.push(Frontier {
                        priority: -(next + Self::heuristic(q, to) * weight / 10),
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
        // Deterministic column order.
        //
        // This built a HashMap of columns and reported whichever conflict its
        // iterator reached first. Rust seeds that hasher per process, so the
        // cell `route` barred in `scratch` before retrying differed run to run:
        // two compiles of `count.ohm` took different retry paths and failed in
        // different places. Unreproducible routing failures are the expensive
        // kind to debug.
        //
        // Sorting the keys fixes that while keeping the original choice of
        // cell - the upper of the closest conflicting pair. That choice is
        // load-bearing: reporting the *lower* one instead (the obvious rewrite,
        // walking the path in order) barred the other end of every conflict and
        // cost `three_input_logic_lays_out_and_runs`, a thirteen-connection
        // design that had always placed.
        let mut columns: HashMap<(i32, i32), Vec<i32>> = HashMap::new();
        for &p in path {
            columns.entry((p.0, p.2)).or_default().push(p.1);
        }
        let mut keys: Vec<(i32, i32)> = columns.keys().copied().collect();
        keys.sort_unstable();
        for k in keys {
            let ys = columns.get_mut(&k).expect("key came from this map");
            ys.sort_unstable();
            for w in ys.windows(2) {
                if w[1] - w[0] < 3 {
                    return Some((k.0, w[1], k.1));
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
        self.active_endpoints.clear();
        self.active_endpoints.extend(sources.iter().copied());
        self.active_endpoints.insert(to);
        // A mild ladder. The penalty for a slope step escalates on retry to
        // push a route towards the flat runs a repeater can sit on - but past a
        // point it stops helping and starts hurting, because a target that can
        // only be reached by descending becomes unreachable when descending
        // costs forty times a flat step. The search then wanders instead of
        // arriving, and reports having got within a few blocks of a target with
        // open approaches, which reads like congestion and is nothing of the
        // kind.
        //
        // This was [14, 40, 120, 400]. Measured against the sequential ladder in
        // `examples/seq_settle.rs`, the gentler spread places designs the steep
        // one could not, and costs nothing on any design that already worked.
        let slopes = [14, 20, 28, 40];
        let mut last_err = String::from("no attempt made");
        // The retry ladder is what makes a *failing* route expensive: up to
        // twenty-four searches of a quarter-million expansions each, all thrown
        // away. That is the right trade when a route can be found, and pure
        // waste when it cannot - so a diagnostic run can cap it and get through
        // a design several times faster. Measuring `gcd`'s failure profile at
        // the full ladder took hours and never finished.
        let attempts: usize =
            std::env::var("OHMC_ATTEMPTS").ok().and_then(|v| v.parse().ok()).unwrap_or(24);
        for attempt in 0..attempts {
            let div: usize = std::env::var("OHMC_SDIV").ok().and_then(|v| v.parse().ok()).unwrap_or(6);
            let slope_cost = slopes[(attempt / div).min(slopes.len() - 1)];
            // Cycle the heuristic weight, exact first, rather than escalating
            // it one way.
            //
            // The two failure modes want opposite searches, and neither is a
            // strictly harder version of the other. A short route inside a packed
            // macro needs an exact search: a greedy path there drives straight at
            // the target, dead-ends, and on the way wastes space the next route
            // needs - turn greedy on globally and the D latch stops building at
            // all. A long route across open ground needs a greedy one: an exact
            // search drowns in equally-good alternatives, and `tick` failed a
            // 111-block hop through a volume a dump showed was completely empty.
            //
            // So each attempt tries a different regime while `scratch` keeps
            // growing underneath, and a route only fails if every regime fails
            // with every barred cell. Exact goes first so tight routes are found
            // immediately and never pay for a greedy detour, and it gets a
            // smaller budget so a hopeless exact search is abandoned quickly
            // instead of burning the full quarter-million on ties.
            let wmode: usize = std::env::var("OHMC_WMODE").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
            let (weight, budget) = match (wmode, attempt % 3) {
                (1, _) => (10, self.max_expansions),
                (2, _) => (13, self.max_expansions),
                (3, _) => (18, self.max_expansions),
                (_, 0) => (10, 80_000),
                (_, 1) => (18, self.max_expansions),
                (_, _) => (13, self.max_expansions),
            };
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
            match self.find(grid, net, sources, to, local, slope_cost, weight, budget) {
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
