//! Placement and routing: gate netlist -> a 3D block world.
//!
//! # Floorplan
//!
//! Logic is levelised, and level `l` sits at `y = -l * LEVEL_H`. Every gate in
//! a level is laid out along X at a fixed Z, so a level is one long row.
//!
//! `LEVEL_H` is the whole point. An earlier version packed levels one block
//! apart so that a gate's output plane lined up exactly with the next level's
//! input plane, making all routing purely horizontal. That was collision-free
//! but could not express a *crossing*, and real netlists are full of them.
//! Spacing levels apart leaves free Y layers between them, which is where
//! crossings go. Minecraft allows 384 blocks of height and a level needs four,
//! so the vertical budget is not a real constraint.
//!
//! Everything between and around the level rows is free space that
//! [`crate::route`] searches with A*. Keepout in the router guarantees two nets
//! can never touch, so shorts are structurally impossible rather than merely
//! unlikely.

use crate::netlist::{Netlist, Sig, Src};
use crate::route::Router;
use crate::tech::{stamp_dff, stamp_lamp, stamp_lever, stamp_nor, DffPorts, NorCell};
use crate::world::{Block, Dir, Grid, Material, Pos};
use std::collections::HashMap;

/// Vertical pitch between logic levels. A cell body needs 4 (`y-1 ..= y+2`),
/// so this leaves `LEVEL_H - 4` free Y layers between rows for crossings.
const LEVEL_H: i32 = 6;
/// Z of every gate row. Cells occupy `z-1 ..= z+2`, leaving the rest of Z open
/// for routing.
const GATE_Z: i32 = 0;
/// Spare X columns between adjacent gates in a row.
const GATE_GAP: i32 = 6;
/// Length of the private approach lane in front of each gate input.
const STUB_LEN: i32 = 4;
/// Length of the output spine trailing each driver. A high-fanout gate would
/// otherwise have to push every branch through the single cell of dust beside
/// its torch, which congests immediately; a spine gives branches several places
/// to leave from.
const SPINE_LEN: i32 = 5;
/// Longest an output spine may grow for a high-fanout net.
const SPINE_MAX: i32 = 24;
/// How often a driver's output spine is refreshed by a repeater.
///
/// A spine used to be plain dust however long it was, and the fan-out scaling
/// makes it up to 24 cells. Dust dies after 15, so the far half of a wide
/// driver's spine was simply dark - and worse, the router was told to assume a
/// branch might leave from the far end, which charged the repeater planner the
/// whole spine length before the route had laid a single block. With a spine
/// longer than the 13-block dust budget that is unsatisfiable by construction:
/// `tick` failed on connection 113 with "route ran 21 blocks with no flat run to
/// hold a repeater", and no amount of search tuning could have helped, because
/// the budget was gone before the search started.
///
/// Refreshing every sixth cell caps the worst-case tap at five blocks of decay
/// whatever the spine's length, which leaves a route most of its budget and
/// makes every tap live.
const SPINE_REFRESH: i32 = 6;

/// How much more horizontal room than vertical drop a single hop must have.
///
/// Dust descends one block of Y per block travelled, so the bare requirement is
/// one horizontal block per level. That bare test - stage only when the drop
/// *exceeds* the run - is what a six-flip-flop design failed on: a connection
/// with an eleven-level drop and twelve blocks of horizontal room was left as
/// one hop, and twelve blocks is a 92% grade. A staircase that steep has no flat
/// cell for a repeater to sit on, so `plan_repeaters` refuses every path the
/// search finds and the router burns its whole budget being told no. The
/// failure reports open approaches and a search that got within a few blocks,
/// which reads like congestion and is nothing of the kind.
///
/// The real requirement includes the flats: one per level, plus one every
/// thirteen blocks to refresh the signal, plus room to approach the target from
/// a sensible direction. Demanding twice the drop is a blunt way to say that,
/// and it is on the right side of the tradeoff - an unnecessary relay costs two
/// repeaters, while a missing one costs the whole route.
const STEEP_SLACK: i32 = 2;
/// Largest vertical drop a single route may attempt.
///
/// Dust falls one block of Y per block of horizontal travel, and it cannot carry
/// a repeater on the way down because a repeater cannot sit on a slope. So a
/// descent of N costs N blocks of the 15-block signal budget with no chance to
/// refresh: past roughly the budget itself, a one-shot descent is impossible on
/// physics alone, and the only paths that exist switch back over themselves and
/// break the slope. Anything deeper gets broken into relay stages.
const MAX_DROP: i32 = 12;
/// How far one routed hop may reach horizontally before the connection is
/// broken over another relay.
const MAX_HOP: i32 = 48;

/// How many Z bands relays are spread over before wrapping.
const RELAY_BANDS: i32 = 6;
/// Z between adjacent relay bands.
const RELAY_BAND_GAP: i32 = 10;

/// Z of the first relay stage, south of every gate spine.
const RISER_Z0: i32 = 14;
/// Z advance per relay stage. Must exceed MAX_DROP so each stage can descend
/// monotonically instead of doubling back.
const RISER_PITCH: i32 = 12;

pub struct Layout {
    pub grid: Grid,
    /// Input levers, LSB first, per port.
    pub input_levers: Vec<(String, Vec<Pos>)>,
    /// Output lamps, LSB first, per port.
    pub output_lamps: Vec<(String, Vec<Pos>)>,
    pub levels: usize,
    pub gates: usize,
    /// Clock, inverted clock and reset levers. Empty for a combinational build.
    ///
    /// The clock is supplied as two externally driven phases rather than one
    /// clock and an on-board inverter. A per-flop inverter is 20 gates and a
    /// second thing to get wrong; two levers are free and let the harness hold
    /// the non-overlap explicitly.
    pub clk_lever: Option<Pos>,
    pub clk_n_lever: Option<Pos>,
    pub rst_lever: Option<Pos>,
    pub flops: usize,
    /// Every flip-flop's ports, so a harness can drive and read the bank
    /// directly instead of inferring where its pads ended up.
    pub flop_ports: Vec<crate::tech::DffPorts>,
    /// Where each NOR gate's signal ended up: its output dust and its input
    /// pads. Exposed so a placed circuit can be diffed against the netlist gate
    /// by gate - the only way to find *which* gate first disagrees, rather than
    /// only that the outputs are wrong.
    pub gate_cells: HashMap<Sig, (Pos, Vec<Pos>)>,
    /// The router's final ownership map: which signal each wire cell carries.
    /// The audit that finds a wire joined to the wrong driver needs exactly
    /// this - the cell where ownership changes from one net to another is the
    /// join.
    pub wire_owner: HashMap<Pos, u32>,
}

struct Placed {
    cell: NorCell,
}

/// Stamp a relay: a repeater with dust either side.
///
/// A relay is what makes a long descent possible. It restores the signal to
/// full strength, so each stage gets its own budget, and it breaks the drop
/// into pieces small enough that a monotonic path exists.
fn stamp_relay(grid: &mut Grid, pos: Pos) -> Result<(Pos, Pos), String> {
    let inp = (pos.0, pos.1, pos.2 - 1);
    let out = (pos.0, pos.1, pos.2 + 1);
    for p in [inp, pos, out] {
        // Reuse whatever solid block is already there. Relays are stamped while
        // routing is in progress, so an earlier route may already have laid
        // substrate through this column.
        let sub = (p.0, p.1 - 1, p.2);
        if grid.is_free(sub) {
            grid.set(sub, Block::Solid(Material::Clock))?;
        } else if !grid.get(sub).is_opaque() {
            return Err(format!("relay at {pos:?} has no solid footing at {sub:?}"));
        }
    }
    grid.set(inp, Block::Dust { power: 0 })?;
    // Reads from the north, drives south: stages always run in +Z.
    grid.set(pos, Block::Repeater { facing: Dir::North, delay: 1, powered: false })?;
    grid.set(out, Block::Dust { power: 0 })?;
    Ok((inp, out))
}

/// Route a connection in relay stages, so no single hop has to be long, steep
/// or both.
///
/// The gate work list has staged its connections since relays existed; the
/// register bank's own wiring - D, both clock phases and reset - did not, and
/// went out as one unbroken hop each. Those are the *longest* nets in the
/// build by a wide margin: a bank sits behind the gate array in Z and a flop's
/// D pad is on its far north face, so `tick` asked the router for a single
/// 307-block climb and it gave up having got 40 blocks in. This is the same
/// staging, factored out.
#[allow(clippy::too_many_arguments)]
fn staged_route(
    grid: &mut Grid,
    router: &mut Router,
    net_id: Sig,
    sources: &[Pos],
    target: Pos,
    bounds: (Pos, Pos),
    material: Material,
    decay: i32,
    chain: &mut usize,
) -> Result<(), String> {
    // Measure from the *nearest* source, not the first one.
    //
    // Every cell of a spine or trunk is an equally valid place to leave from,
    // and the router picks whichever suits it. Planning from `sources[0]` means
    // planning from the far end: once the control trunks ran the length of the
    // bank, a reset route to a flip-flop a few blocks off the trunk was measured
    // as spanning the whole bank and broken into four stages it did not need,
    // each of which then had to find room inside the register bank itself.
    let anchor = *sources
        .iter()
        .min_by_key(|s| (s.0 - target.0).abs() + (s.1 - target.1).abs() + (s.2 - target.2).abs())
        .unwrap_or(&sources[0]);
    let top = anchor.1;
    // Signed, because bank wiring *climbs*: a gate's output is below the flop's
    // input pad, so D runs upward where every gate connection runs down.
    let rise = target.1 - top;
    let span_x = (target.0 - anchor.0).abs();
    let span_h = span_x + (target.2 - anchor.2).abs();

    let vstages = (rise.abs() + MAX_DROP - 1) / MAX_DROP;
    let hstages = (span_h + MAX_HOP - 1) / MAX_HOP;
    // A descent needs at least as much horizontal room as it has depth, since
    // dust falls one block of Y per block travelled. Endpoints close together
    // but far apart vertically do not provide it; staging through a relay out in
    // open space does.
    let steep = if rise.abs() * STEEP_SLACK > span_h { 2 } else { 1 };
    let stages = vstages.max(hstages).max(steep).max(1);

    let mut from: Vec<Pos> = sources.to_vec();
    let mut carry = decay;
    for stage in 1..stages {
        let rx = anchor.0 + (target.0 - anchor.0) * stage / stages;
        let ry = top + rise * stage / stages;
        // Z is interpolated between the endpoints, like X and Y - not parked in
        // the riser field.
        //
        // The gate work list puts its relays at a fixed `RISER_Z0 + band`, which
        // works because every one of its connections runs from a spine just
        // south of the gate array to a stub just north of it, so the riser field
        // is genuinely on the way. The register bank is the opposite case: it
        // sits *behind* the gate array in Z, so a clock chain heading for a flop
        // 200 blocks north was staged through relays 50 blocks south, and stage
        // 3 had nowhere to go. Interpolating keeps every hop on the line between
        // the two ends whichever way that line runs.
        let band = (*chain % RELAY_BANDS as usize) as i32;
        let rz = anchor.2 + (target.2 - anchor.2) * stage / stages + band * 3;
        // Search outward in Z in both directions. The nominal site can land
        // inside a flip-flop macro, which is 55 blocks deep, and a one-sided
        // scan cannot always get clear of one.
        let dzs = (0..32).flat_map(|k: i32| if k == 0 { vec![0] } else { vec![2 * k, -2 * k] });
        // Take the first *comfortable* site, falling back to the roomiest of the
        // first several usable ones.
        //
        // Taking the first merely-usable site is how reset - twenty-two chains
        // leaving one lever for eleven flip-flops - strangled itself at stage 2:
        // a site with the bare minimum of two approaches is fine until the route
        // into it takes one, and then the next stage starts walled in. Taking
        // the roomiest instead overcorrects, because the candidate list is
        // ordered by distance from the nominal site and open space is furthest
        // from the traffic: clk_n went out to the cramped western edge of the
        // build, past its own lever, and stage 1 could not reach it. Near and
        // adequate beats far and spacious.
        const COMFY: usize = 6;
        let mut best: Option<(usize, Pos)> = None;
        let mut seen = 0;
        for (dx, dy, dz) in dzs.flat_map(|dz| {
            [0, 2, -2, 3, -3]
                .into_iter()
                .flat_map(move |dy| [0, 2, -2, 4, -4, 6, -6, 9, -9, 12, -12].map(move |dx| (dx, dy, dz)))
        }) {
            let c = (rx + dx, ry + dy, rz + dz);
            if let Some(r) = router.relay_site_room(grid, c, net_id) {
                if r >= COMFY {
                    best = Some((r, c));
                    break;
                }
                if best.is_none_or(|(b, _)| r > b) {
                    best = Some((r, c));
                }
                seen += 1;
                if seen >= 24 {
                    break;
                }
            }
        }
        let site = best
            .map(|(_, c)| c)
            .ok_or_else(|| format!("no clear relay site near ({rx}, {ry}, {rz}) for net {net_id}"))?;
        let (rin, rout) = stamp_relay(grid, site)?;
        router.claim(rin, net_id);
        router.claim(rout, net_id);
        router.block(site);
        router
            .route(grid, net_id, &from, rin, bounds, material, carry)
            .map_err(|e| format!("stage {stage} of {stages}: {e}"))?;
        from = vec![rout];
        carry = 0;
    }
    *chain += 1;
    router.route(grid, net_id, &from, target, bounds, material, carry)
}

/// Assign every combinational signal a logic level: leaves at 0, each NOR one
/// past its deepest operand.
fn levelize(net: &Netlist, roots: &[Sig]) -> Vec<i32> {
    let mut level = vec![0i32; net.sigs.len()];
    for s in net.topo_order(roots) {
        if matches!(net.src(s), Src::Nor(_)) {
            let d = net.operands(s).iter().map(|&o| level[o as usize]).max().unwrap_or(0);
            level[s as usize] = d + 1;
        }
    }
    level
}

/// A bank of flip-flops, tiled so the placer can hand out one per netlist
/// register.
///
/// Tiled in both X and Z rather than a single row: a flip-flop is 66x149, so 42
/// of them side by side would be over 3000 blocks wide and every clock wire
/// would cross the whole floorplan. A roughly square bank keeps the clock and
/// reset spines short, which matters because both fan out to every register.
pub struct RegisterBank {
    pub flops: Vec<DffPorts>,
}

/// Pitch of the register bank.
///
/// This clears the flip-flop's measured extent plus a margin, and the margin is
/// not optional: the measurement is of a macro stamped into an empty grid, and
/// the D latch wires itself with the maze router, so with a neighbour inside its
/// bounds the router will spill into it. Tiling at the bare extent collided
/// registers 1 and 5.
///
/// The numbers track `examples/flop_size.rs`. A flip-flop was 68 x 113 and is
/// now 35 x 55: the staircase inside the D latch was pitched for hand-placed
/// wiring, and swept against the flip-flop's own behavioural test it tightens
/// to a quarter of the area. That is the whole reason a sequential design can be
/// placed at all - bounds cover everything placed, so an oversized bank is extra
/// distance every Q has to cross to reach the logic, and `tick` routed 3 of 141
/// connections at the old size.
const FLOP_PITCH_X: i32 = 47;
const FLOP_PITCH_Z: i32 = 66;

/// Stamp `n` flip-flops in a bank based at `base`.
pub fn place_register_bank(g: &mut Grid, base: Pos, n: usize) -> Result<RegisterBank, String> {
    let (bx, by, bz) = base;
    // Near-square, so neither spine has to span the whole bank.
    //
    // Rows cost Q reach: the row behind the front one sits another pitch back in
    // Z and its Q must cross all of it. Columns cost width: eleven flops in a
    // single row is a bank many times wider than the gate array it feeds, and
    // `tick` then stalls on a single enormous relay hop. Square is the better of
    // the two, and at the old flip-flop size it was still not enough - the real
    // fix was upstream, in how much space one bit of state takes.
    let cols = (n as f64).sqrt().ceil().max(1.0) as usize;
    let mut flops = Vec::with_capacity(n);
    for i in 0..n {
        let (cx, cz) = (i % cols, i / cols);
        let at = (bx + cx as i32 * FLOP_PITCH_X, by, bz + cz as i32 * FLOP_PITCH_Z);
        flops.push(stamp_dff(g, at).map_err(|e| format!("register {i}: {e}"))?);
    }
    Ok(RegisterBank { flops })
}

/// # Known: `add.ohm` places but computes the wrong answer
///
/// The 8-bit adder places cleanly - 24662 blocks, no unsupported dust, no
/// history dependence - and gets 44 of 64 sampled inputs wrong. This was
/// invisible until now: `tools/mc-validate.sh` refuses designs with more than
/// six input levers and this one has sixteen, so `add.ohm` had never been
/// checked against anything. `examples/two_pass.rs` samples the input space and
/// is the first thing able to check it at all.
///
/// The errors say where to look. Every difference is a power of two or a sum of
/// them - 16, 32, 64, 128 - so whole output *bits* are wrong rather than the
/// arithmetic being off, and they are the high-order bits, which sit at the end
/// of the longest carry chains. Most deltas are negative: a bit that should be
/// one reads zero. That is a signal dying, not a logic error, and the suspect is
/// repeater insertion on long routes rather than anything in the netlist - which
/// `--truth` confirms is right for the failing cases.
///
/// Place and route a purely combinational netlist.
///
/// Returns an error if the netlist contains state: flip-flops need a sequential
/// floorplan (a clock spine and flip-flop macro cells) that this does not
/// attempt yet.
pub fn build(net: &Netlist) -> Result<Layout, String> {
    let roots = net.roots();
    let level = levelize(net, &roots);
    let order = net.topo_order(&roots);

    let gates: Vec<Sig> = order
        .iter()
        .copied()
        .filter(|&s| matches!(net.src(s), Src::Nor(_)))
        .collect();
    let max_level = gates.iter().map(|&g| level[g as usize]).max().unwrap_or(0);

    let mut grid = Grid::new();
    let mut placed: HashMap<Sig, Placed> = HashMap::new();

    // Leaves first: levers on their own row above level 0. They are the
    // drivers every level-1 gate is placed relative to.
    let mut source_of: HashMap<Sig, Pos> = HashMap::new();
    let mut leaves: Vec<Sig> = order
        .iter()
        .copied()
        .filter(|&s| matches!(net.src(s), Src::Input { .. } | Src::Reset | Src::One | Src::Zero))
        .collect();
    leaves.sort();
    let lever_y = LEVEL_H;
    let mut widest = 1;
    for (i, &s) in leaves.iter().enumerate() {
        // Levers sit three apart so their dust taps cannot touch.
        let x = i as i32 * 3;
        let pos = (x, lever_y, GATE_Z - 4);
        let mat = if matches!(net.src(s), Src::One | Src::Zero) {
            Material::Clock
        } else {
            Material::PortIn
        };
        let on = matches!(net.src(s), Src::One);
        stamp_lever(&mut grid, pos, mat, on)?;
        // A dust node beside the lever gives the router something to start from.
        let tap = (x, lever_y, GATE_Z - 3);
        grid.set((tap.0, tap.1 - 1, tap.2), Block::Solid(mat))?;
        grid.set(tap, Block::Dust { power: 0 })?;
        source_of.insert(s, tap);
        widest = widest.max(x + 3);
    }

    // The register bank, and the control levers that drive it.
    //
    // Flip-flop Q outputs are *sources* exactly like input levers: the
    // combinational cone reads them, and `net.roots()` already includes every
    // flip-flop's D, so levelisation covers the whole sequential cone without
    // any special casing. All that is missing is somewhere for Q to come from
    // and somewhere for D to go.
    //
    // The bank sits in negative Z, behind the input levers.
    //
    // That region is unclaimed - gates occupy the GATE_Z plane and relays run
    // out in +Z - and, more importantly, it puts the *right end* of each flop
    // next to the logic. A flip-flop is about 150 blocks deep with Q at its far
    // +Z face, so a bank placed beyond the riser field leaves Q some 250 blocks
    // from the gates it feeds and the router simply cannot reach that far.
    // Growing the bank backwards from the gate plane makes the last row's Q
    // land a dozen blocks away instead.
    let mut bank_flops: Vec<crate::tech::DffPorts> = Vec::new();
    let (mut clk_lever, mut clk_n_lever, mut rst_lever) = (None, None, None);
    if !net.dffs.is_empty() {
        let rows = {
            let cols = (net.dffs.len() as f64).sqrt().ceil().max(1.0) as usize;
            net.dffs.len().div_ceil(cols) as i32
        };
        // Offset by where Q actually sits inside a flop, not by the tiling
        // pitch. A flip-flop is deep and Q is at its far +Z face, so using the
        // pitch leaves Q tens of blocks further from the logic than it needs to
        // be - and the router's reach is the binding constraint here.
        let q_off = {
            let mut probe = Grid::new();
            stamp_dff(&mut probe, (0, 0, 0)).map_err(|e| format!("measuring flop: {e}"))?.q.2
        };
        let bank_z = GATE_Z - 12 - q_off - (rows - 1) * FLOP_PITCH_Z;
        let bank = place_register_bank(&mut grid, (0, lever_y, bank_z), net.dffs.len())?;
        bank_flops = bank.flops;

        // Control levers, well clear of the data levers.
        let ctrl_x = -6;
        for (i, slot) in [&mut clk_lever, &mut clk_n_lever, &mut rst_lever].into_iter().enumerate() {
            let pos = (ctrl_x - i as i32 * 3, lever_y, GATE_Z - 4);
            stamp_lever(&mut grid, pos, Material::Clock, false)?;
            // Tap on the *north* side of the lever, towards the register bank.
            // On the south side the distribution trunk's very first cell is the
            // lever itself, so the trunk stopped at length zero and every clock
            // and reset route was left trying to leave from a single dust cell.
            let tap = (pos.0, pos.1, pos.2 - 1);
            grid.set((tap.0, tap.1 - 1, tap.2), Block::Solid(Material::Clock))?;
            grid.set(tap, Block::Dust { power: 0 })?;
            *slot = Some(tap);
        }

        // Q is a source for the cone that reads it.
        for i in 0..net.dffs.len() {
            let q_sig = net
                .dff_q(i as u32)
                .ok_or_else(|| format!("flip-flop {i} has no Q signal"))?;
            // The boundary port, not the raw Q. Routing out of the macro body
            // leaves the search four open exits and it never escapes; from the
            // port it has thirty-nine.
            source_of.insert(q_sig, bank_flops[i].q_port);
        }
    }

    // Gates: one row per level, ordered within the row by the average X of the
    // drivers feeding them.
    //
    // This is the "place" half of place-and-route, and skipping it is expensive.
    // Packing gates in topological order scatters connected cells across the
    // whole row, so every wire is long - which costs search time *and* fills
    // space that later nets need. Barycenter ordering is the standard cheap
    // heuristic: put each gate near whatever drives it. Because a level only
    // depends on shallower levels, every driver is already placed by the time
    // its consumers are ordered.
    let mut by_level: HashMap<i32, Vec<Sig>> = HashMap::new();
    for &g in &gates {
        by_level.entry(level[g as usize]).or_default().push(g);
    }
    for l in 1..=max_level {
        let Some(mut row) = by_level.remove(&l) else { continue };
        // Where each gate would *like* to sit: the mean X of whatever drives
        // it. Gates with no placed driver get the left edge rather than a
        // sentinel in the middle of the row.
        let want_x = |g: Sig, placed: &HashMap<Sig, Placed>| -> i64 {
            let xs: Vec<i64> = net
                .operands(g)
                .iter()
                .filter_map(|&s| {
                    placed
                        .get(&s)
                        .map(|p: &Placed| p.cell.out.0)
                        .or_else(|| source_of.get(&s).map(|p| p.0))
                })
                .map(|x| x as i64)
                .collect();
            if xs.is_empty() {
                i64::MIN / 2
            } else {
                xs.iter().sum::<i64>() / xs.len() as i64
            }
        };
        row.sort_by_key(|&g| (want_x(g, &placed), g));

        // Place each gate *at* its barycenter, not merely in barycenter order.
        //
        // Ordering alone is not placement. Every row used to be packed from
        // x = 0, so a level with three gates hugged the origin however far away
        // its drivers were - and in `tick` that put a gate at x = 0 fed by a
        // spine at x = 163, a connection crossing the entire build diagonally.
        // With 141 such connections the router never got past the fifth.
        //
        // Sorted by desired X, a single left-to-right sweep places each gate at
        // its wish or at the first free spot after its predecessor, whichever is
        // further right. That is the classic linear-placement sweep: it keeps
        // the ordering, never overlaps, and collapses to the old behaviour when
        // every wish is at the origin.
        let mut x = i32::MIN;
        for g in row {
            let wish = want_x(g, &placed);
            let at = if wish == i64::MIN / 2 { x.max(0) } else { (wish as i32).max(x) };
            let at = at.max(0);
            let k = net.operands(g).len().max(1);
            let cell = stamp_nor(&mut grid, (at, -l * LEVEL_H, GATE_Z), k)?;
            x = at + cell.width + GATE_GAP;
            widest = widest.max(x);
            placed.insert(g, Placed { cell });
        }
    }

    // The routing volume: the gate rows plus generous free space around them.
    // Room for the riser field: one private column per connection, plus enough
    // Z for the deepest relay chain.
    let max_stages = ((max_level + 2) * LEVEL_H / MAX_DROP) + 2;
    let total_conns: i32 = gates.iter().map(|&g| net.operands(g).len() as i32).sum();
    let span = (widest + 8 + 3 * total_conns.max(1)).max(32);
    let depth = (gates.len() as i32).max(8) * 2 + 24 + RISER_Z0 + max_stages * RISER_PITCH;
    // Bounds must cover everything already placed, not just the gate array.
    //
    // They used to be derived from the gate array's own width and depth. A
    // register bank breaks that: a flip-flop macro is 66 blocks wide, so a
    // bank's Q sits at x=66 while a small design's gate array is only a few
    // wide - the source was outside the region the router was allowed to
    // search, and every sequential placement failed with "no route" on an
    // almost empty grid.
    let bounds = {
        let (lo, hi) = grid.bounds().unwrap_or(((0, 0, 0), (0, 0, 0)));
        (
            (
                (-8).min(lo.0 - 8),
                (-(max_level + 1) * LEVEL_H - 8).min(lo.1 - 8),
                (GATE_Z - depth).min(lo.2 - 8),
            ),
            (
                span.max(hi.0 + 8),
                (lever_y + 4).max(hi.1 + 8),
                (GATE_Z + depth).max(hi.2 + 8),
            ),
        )
    };

    // Lamps are stamped before the router exists so their blocks are reserved.
    // Otherwise a route lays substrate straight through where a lamp will go.
    //
    // Each lamp sits just below the gate that drives it, rather than in a tidy
    // row at the bottom of the build. A shared bottom row reads better but puts
    // the lamp an arbitrary distance beneath its driver, and that descent is
    // exactly the one-shot drop that dust cannot make - it was the last thing
    // standing between `add.ohm` and a complete route. The compiler reports
    // every lamp's coordinates anyway, so nothing downstream needs them aligned.
    let mut lamp_pad: Vec<(String, Vec<(Sig, Pos, Pos)>)> = Vec::new();
    {
        let mut lamp_x = 0;
        for (name, bits) in &net.outputs {
            let mut v = Vec::new();
            for &b in bits {
                let drv_y = placed.get(&b).map(|p| p.cell.out.1).unwrap_or(lever_y);
                // Far enough below and along that the descent has room. Wire
                // nodes in one column must differ by three in Y, so a short hop
                // with a small drop has nowhere to put its middle and the path
                // ends up colliding with itself.
                let y = drv_y - 4;
                let pad = (lamp_x, y, GATE_Z + 14);
                let lamp = (lamp_x, y - 1, GATE_Z + 14);
                stamp_lamp(&mut grid, lamp)?;
                v.push((b, pad, lamp));
                lamp_x += 3;
            }
            lamp_pad.push((name.clone(), v));
        }
    }

    let mut router = Router::from_grid(&grid);
    // Each signal is its own net. Its driver gets an output spine: a short dust
    // run any of whose cells a branch may leave from.
    let mut spine: HashMap<Sig, Vec<Pos>> = HashMap::new();
    let mut drivers: Vec<(Sig, Pos)> = placed.iter().map(|(&s, p)| (s, p.cell.out)).collect();
    drivers.extend(source_of.iter().map(|(&s, &p)| (s, p)));
    drivers.sort();
    // How many connections each signal has to serve. A spine is the only place
    // a branch may leave from, so a net with eight consumers needs more of it
    // than one with two - and when it runs out, every later branch reports zero
    // open exits at the source and the connection simply cannot start. That is
    // the wall `alu` hits: one high-fanout net whose spine is surrounded by the
    // consumers that already left.
    let mut fanout: HashMap<Sig, usize> = HashMap::new();
    for &g in &gates {
        for &src in net.operands(g) {
            *fanout.entry(src).or_default() += 1;
        }
    }
    for (s, out) in drivers {
        let mut cells = vec![out];
        router.claim(out, s);
        // Two spine cells per consumer, since a branch leaving one cell takes
        // the room beside it too, clamped so a huge fanout cannot run across
        // the whole floorplan.
        let want = (fanout.get(&s).copied().unwrap_or(1) as i32 * 2).clamp(SPINE_LEN, SPINE_MAX);
        for t in 1..=want {
            let p = (out.0, out.1, out.2 + t);
            if !grid.is_free(p) || !grid.is_free((p.0, p.1 - 1, p.2)) {
                break;
            }
            grid.set((p.0, p.1 - 1, p.2), Block::Solid(Material::Wire))?;
            router.claim(p, s);
            if t % SPINE_REFRESH == 0 {
                // A repeater reads from the side it faces, and the spine runs
                // +Z, so it faces north back towards the driver. It is not a
                // branch point - a route cannot leave from a repeater - so it is
                // claimed but kept out of the source list.
                grid.set(p, Block::Repeater { facing: Dir::North, delay: 1, powered: false })?;
            } else {
                grid.set(p, Block::Dust { power: 0 })?;
                cells.push(p);
            }
        }
        spine.insert(s, cells);
    }

    // Lamps must be kept clear of unrelated wiring. A powered block lights an
    // adjacent lamp, so a route that merely passes nearby - laying substrate
    // that some other signal energises - turns an output on regardless of what
    // the circuit computed. Reserving a shell around each lamp for its own net
    // is what makes a multi-bit result mean anything.
    for (_, entries) in &lamp_pad {
        for &(b, _, lamp) in entries {
            for dx in -2..=2i32 {
                for dy in -2..=2i32 {
                    for dz in -2..=2i32 {
                        router.reserve((lamp.0 + dx, lamp.1 + dy, lamp.2 + dz), b);
                    }
                }
            }
        }
    }

    // Give every gate input a private approach stub running north from its feed.
    //
    // Without this, the route serving one feed travels along the shared lane in
    // front of the cell, and its keepout walls off the neighbouring feed - so a
    // two-input gate becomes unroutable. Pre-placing the stub and claiming it
    // for the driving net means each feed has a guaranteed private lane, and the
    // router only has to reach the lane's far end, where there is open space.
    let mut stub_entry: HashMap<(Sig, usize), Pos> = HashMap::new();
    for &g in &gates {
        for (j, &src) in net.operands(g).iter().enumerate() {
            let f = placed[&g].cell.feeds[j];
            // Dust lane, then a repeater at its far end. The repeater makes the
            // stub a fresh 15-strength source, so the arriving route's signal
            // budget and the stub's are independent - otherwise a route that
            // only just made it would die in the last few blocks.
            for t in 0..=STUB_LEN + 1 {
                let p = (f.0, f.1, f.2 - t);
                if grid.is_free(p) {
                    grid.set((p.0, p.1 - 1, p.2), Block::Solid(Material::Wire))?;
                    if t == STUB_LEN {
                        grid.set(
                            p,
                            Block::Repeater { facing: Dir::North, delay: 1, powered: false },
                        )?;
                    } else {
                        grid.set(p, Block::Dust { power: 0 })?;
                    }
                }
                router.claim(p, src);
                // Fence the lane so no other net can run alongside it.
                router.block((p.0 - 1, p.1, p.2));
                router.block((p.0 + 1, p.1, p.2));
            }
            // The repeater is not a wire node; routes terminate just past it.
            router.block((f.0, f.1, f.2 - STUB_LEN));
            // Keep a landing zone around the lane entrance for this net alone.
            // Measured: without it a passing net takes every approach and the
            // target ends up with 0 of 12 usable neighbours, which sends the
            // router off to spend its entire budget looking for a way in.
            let entry = (f.0, f.1, f.2 - STUB_LEN - 1);
            for dx in -1..=1i32 {
                for dy in -2..=2i32 {
                    for dz in -3..=1i32 {
                        router.reserve((entry.0 + dx, entry.1 + dy, entry.2 + dz), src);
                    }
                }
            }
            stub_entry.insert((g, j), (f.0, f.1, f.2 - STUB_LEN - 1));
        }
    }

    // Relay chains live in their destination's own column, so each column needs
    // its own allocator: two gates at the same X on different levels would
    // otherwise stack their chains on top of each other.
    let mut riser_slot: HashMap<i32, i32> = HashMap::new();
    let _ = max_stages;

    // Route every fan-in connection, hardest first.
    //
    // Order matters more than it looks. Routing in level order does the short
    // local nets first, and their keepout then walls off the long ones that had
    // far fewer options to begin with - so the hardest net is attempted last,
    // into the most crowded space. Sorting by descending difficulty gives the
    // constrained nets first pick, which is the cheap half of what a real
    // congestion-negotiating router does.
    //
    // Difficulty is estimated as the driver-to-sink distance, dominated by the
    // vertical drop since that is what forces relay staging.
    let mut work: Vec<(Sig, usize, Sig, i64)> = Vec::new();
    for &g in &gates {
        for (j, &src) in net.operands(g).iter().enumerate() {
            let feed = stub_entry[&(g, j)];
            let from = spine
                .get(&src)
                .and_then(|v| v.first().copied())
                .ok_or_else(|| format!("signal {src} has no driver"))?;
            let dist = (from.0 - feed.0).abs() as i64
                + (from.2 - feed.2).abs() as i64
                + 3 * (from.1 - feed.1).abs() as i64;
            work.push((g, j, src, dist));
        }
    }
    // Descending difficulty; gate and input index break ties so builds stay
    // byte-identical across runs.
    work.sort_by_key(|&(g, j, _, dist)| (std::cmp::Reverse(dist), g, j));

    let total_conns = work.len();
    let mut routed = 0usize;
    // A queue rather than a plain iteration, so a connection that cannot reach
    // its feed can rip up whatever is in the way and put those connections back
    // to be done again.
    //
    // Without it routing is first-come-first-served: an early net takes the
    // space and a later one - often the one that had far fewer options to begin
    // with - simply has nowhere to go. The failure looks like a search budget
    // problem and is not; raising the budget from 250k to 900k on `alu` changed
    // nothing, because the target genuinely had no open approach left.
    let mut queue: std::collections::VecDeque<(Sig, usize, Sig)> =
        work.iter().map(|&(g, j, src, _)| (g, j, src)).collect();
    // Rips per connection, so two that keep evicting each other give up rather
    // than trade the same space forever.
    let mut rips: HashMap<(Sig, usize), u32> = HashMap::new();
    // One index per staged connection, so the relay band allocator has something
    // per-chain to key on.
    let mut chain_seq: usize = 0;
    let mut done: Vec<(Sig, usize, Sig)> = Vec::new();
    while let Some((g, j, src)) = queue.pop_front() {
        let feed = stub_entry[&(g, j)];
        let sources = spine
            .get(&src)
            .ok_or_else(|| format!("signal {src} has no driver"))?;
        // Worst case a branch leaves from the cell just before a refresh
        // repeater, which is `SPINE_REFRESH - 1` blocks of decay - not the whole
        // spine, which is what this used to charge and which a long spine cannot
        // pay. See `SPINE_REFRESH`.
        let decay = SPINE_REFRESH - 1;

        // Break a deep drop into stages, each landing on a relay.
        let top = sources[0].1;
        let drop = top - feed.1;
        // Stage on horizontal reach as well as vertical drop.
        //
        // Keying only on drop leaves a connection that is long but shallow to
        // be routed in one A* shot, and the search - direction-aware, with turn
        // penalties - exhausts its budget rather than arriving. A register
        // bank's Q is exactly that shape: the flip-flop macro is 66 wide and Q
        // exits at its far side, so the run to the logic is 66 blocks of X with
        // a 5-block drop, and no sequential design could place at all.
        let span_x = (feed.0 - sources[0].0).abs();
        let vstages = if drop > MAX_DROP { (drop + MAX_DROP - 1) / MAX_DROP } else { 1 };
        let hstages = if span_x > MAX_HOP { (span_x + MAX_HOP - 1) / MAX_HOP } else { 1 };
        // Stage a *steep* connection too, however short it is.
        //
        // Dust descends one block of Y per block travelled horizontally, so a
        // drop needs at least as much horizontal room as it has depth. Nothing
        // guaranteed that: a gate barycentred directly beneath the flip-flop
        // driving it sat four blocks away in Z with eleven levels to fall, and
        // the only way down was a switchback that lands a wire above its own
        // substrate. The router cannot build that and cannot say so - it reports
        // a search that got within five blocks and gave up.
        //
        // Better placement makes this *more* common, not less, because putting a
        // gate near its driver is exactly what removes the horizontal run the
        // descent was using. One relay fixes it.
        let span_h = span_x + (feed.2 - sources[0].2).abs();
        let steep = if drop * STEEP_SLACK > span_h { 2 } else { 1 };
        let stages = vstages.max(hstages).max(steep);
        // `OHMC_TRACE=1` prints the plan for every connection. Routing failures
        // report the search's view - open approaches, expansions - which says
        // nothing about *why* the connection was shaped the way it was; the
        // plan is what usually turns out to be wrong.
        if std::env::var("OHMC_TRACE").is_ok() {
            eprintln!(
                "conn g{g} in{j} <- net{src}: src0={:?} nsrc={} feed={feed:?} drop={drop} spanx={span_x} spanh={span_h} stages={stages}",
                sources[0],
                sources.len()
            );
        }

        let mut from: Vec<Pos> = sources.clone();
        let mut carry = decay;
        let mut stage_err: Option<String> = None;
        // Relay chains step from the driver toward the sink, so every hop is
        // short in X as well as Y. Parking the whole chain at either end just
        // moves the long hop to the other side: with it at the destination the
        // first stage spans the build, with it at the source the last one does.
        let src_x = sources[0].0;
        let chain_x: Vec<i32> = (1..stages)
            .map(|s| src_x + (feed.0 - src_x) * s / stages)
            .collect();
        // Chains are separated by *where they already are*, not by handing each
        // one its own Z band.
        //
        // Successive stages of one chain differ in X and Y because both are
        // interpolated from source to sink, and two different chains only
        // collide if they share both. The old allocator gave every chain a band
        // and marched it further in Z each stage, so the strip's depth grew
        // with the design: alu's first chain started at z=437, and its final
        // hop then had to cross the whole excursion. A small per-chain jitter
        // breaks the remaining ties, and `relay_site_clear` moves anything that
        // still lands on a neighbour.
        let chain = chain_seq;
        if stages > 1 {
            chain_seq += 1;
        }
        let _slot = if stages > 1 {
            let key = *chain_x.first().unwrap_or(&feed.0);
            let s = riser_slot.entry(key).or_insert(0);
            let v = *s;
            *s += 1;
            v % 4
        } else {
            0
        };
        for stage in 1..stages {
            // Put the relay chain in the destination's own column. Parking it in
            // a shared riser field far away made the *final* hop span the whole
            // build - which is why routing failed on the very first connection,
            // with an empty grid and no congestion at all. Descending above the
            // gate that consumes the signal keeps every hop short.
            //
            // Inputs of one gate are two columns apart, so their chains are
            // separated in Z instead, keeping them clear of each other's keepout.
            let rx = chain_x[(stage - 1) as usize];
            let ry = top - stage * (drop / stages);
            // Z band by the level the relay lands on.
            //
            // A chain descends through levels, so its stages land on different
            // ones and separate in Z for free; two chains on the same level
            // separate in X, which is interpolated toward their own sinks. That
            // gives every stage room without handing each chain a private band
            // whose depth grows with the design - the old scheme put alu's
            // first chain at z=437 and left its final hop crossing the whole
            // excursion. Bands wrap, so Z stays bounded however deep the design.
            // Band by *chain*, not by stage index.
            //
            // Stage-index banding put every chain's first relay in band 0, and
            // stage 1 is the commonest stage there is - most connections are two
            // or three stages. So one band took the great majority of the relays
            // in the design while five sat nearly empty.
            //
            // Banding by chain spreads first stages evenly instead. Stages within
            // one chain still separate from each other, in X and Y, because both
            // are interpolated from source to sink - which is what the stage-index
            // scheme was really buying, and it is free.
            //
            // This is not the old per-chain allocator that put alu's first chain
            // at z = 437: bands wrap, so the field's depth is fixed by
            // RELAY_BANDS however many connections the design has.
            let band = (chain % RELAY_BANDS as usize) as i32;
            let rz = RISER_Z0 + band * RELAY_BAND_GAP + (stage % 2) * 4;
            // stamp_relay writes with grid.set and never consults keepout, so
            // the site has to be checked here or the relay can land touching
            // another net's wire - which electrically joins the two nets and is
            // exactly what made add.ohm compute wrong answers while passing
            // every audit that only looked at routed cells. Probe candidates
            // near the nominal site and take the first clear one.
            // Search widely: chains no longer reserve their own Z, so a site
            // genuinely has to be found rather than assumed.
            // Candidates vary in Y as well as X and Z. `ry` is an interpolation,
            // not a requirement: a relay a couple of blocks above or below the
            // nominal descent restores the signal just as well, and the next
            // stage's route absorbs the difference. Pinning it exactly meant
            // every chain with the same drop competed for one plane.
            let site = (0..48)
                .flat_map(|dz| {
                    [0, 2, -2, 3, -3].into_iter().flat_map(move |dy| {
                        [0, 2, -2, 4, -4, 6, -6, 9, -9, 12, -12].map(move |dx| (dx, dy, dz))
                    })
                })
                .map(|(dx, dy, dz)| (rx + dx, ry + dy, rz + dz))
                .find(|&c| router.relay_site_clear(&grid, c, src))
                .ok_or_else(|| {
                    format!(
                        "no clear relay site near ({rx}, {ry}, {rz}) for net {src}                          into gate {g} input {j}"
                    )
                })?;
            let (rin, rout) = stamp_relay(&mut grid, site)?;
            router.claim(rin, src);
            router.claim(rout, src);
            router.block(site);
            if let Err(e) = router.route(&mut grid, src, &from, rin, bounds, Material::Wire, carry)
            {
                stage_err = Some(format!(
                    "relay stage {stage} for net {src} into gate {g} input {j}: {e}"
                ));
                break;
            }
            from = vec![rout];
            carry = 0;
        }
        // A relay hop can fail for the same reason the final one can: a site
        // that had room when it was chosen is walled in by the time the next
        // stage routes to it. Rip-up covered only the last hop, so those
        // failures were fatal even though the space was recoverable.
        if let Some(e) = stage_err {
            let tries = rips.entry((g, j)).or_insert(0);
            *tries += 1;
            if *tries > 6 {
                return Err(format!("{e}\nnote: {routed} of {total_conns} routed"));
            }
            // Look at both ends. A connection stalls just as often because its
            // own driver is boxed in - the router reports three open exits at a
            // gate's output spine - as because the destination is, and ripping
            // only around the target leaves that untouched.
            let mut victims = router.crowders(feed, 24, src);
            for v in router.crowders(sources[0], 20, src) {
                if !victims.contains(&v) {
                    victims.push(v);
                }
            }
            if victims.is_empty() {
                return Err(format!("{e}\nnote: {routed} of {total_conns} routed"));
            }
            for v in victims.into_iter().take(4) {
                router.rip(&mut grid, v);
                let (again, keep): (Vec<_>, Vec<_>) = done.iter().partition(|&&(_, _, s)| s == v);
                done = keep;
                routed -= again.len();
                queue.extend(again);
            }
            queue.push_front((g, j, src));
            continue;
        }
        match router.route(&mut grid, src, &from, feed, bounds, Material::Wire, carry) {
            Ok(()) => {
                routed += 1;
                done.push((g, j, src));
            }
            Err(e) => {
                let tries = rips.entry((g, j)).or_insert(0);
                *tries += 1;
                if *tries > 6 {
                    return Err(format!(
                        "routing net {src} into gate {g} input {j}: {e}\n\
                         note: {routed} of {total_conns} connections routed, and ripping \
                         up the neighbours {} times did not free a path",
                        *tries - 1
                    ));
                }
                // Rip the nearest few nets crowding the target and try again.
                // Their connections go back on the queue to be redone.
                let mut victims = router.crowders(feed, 24, src);
                for v in router.crowders(sources[0], 20, src) {
                    if !victims.contains(&v) {
                        victims.push(v);
                    }
                }
                let mut freed = 0;
                for v in victims.into_iter().take(3) {
                    router.rip(&mut grid, v);
                    freed += 1;
                    let (again, keep): (Vec<_>, Vec<_>) =
                        done.iter().partition(|&&(_, _, s)| s == v);
                    done = keep;
                    routed -= again.len();
                    queue.extend(again);
                }
                if freed == 0 {
                    return Err(format!(
                        "routing net {src} into gate {g} input {j}: {e}\n\
                         note: {routed} of {total_conns} connections routed, and nothing \
                         was near enough to rip up"
                    ));
                }
                queue.push_front((g, j, src));
            }
        }
    }

    // Outputs: route each bit out to the lamp reserved for it.
    let mut output_lamps: Vec<(String, Vec<Pos>)> = Vec::new();
    for (name, entries) in &lamp_pad {
        let mut lamps = Vec::new();
        for &(b, pad, lamp) in entries {
            let sources = &spine[&b];
            let decay = sources.len() as i32 - 1;
            router
                .route(&mut grid, b, sources, pad, bounds, Material::PortOut, decay)
                .map_err(|e| format!("routing output `{name}` to its lamp: {e}"))?;
            lamps.push(lamp);
        }
        output_lamps.push((name.clone(), lamps));
    }

    // Sequential nets: every flip-flop's D, and the shared control lines.
    //
    // These are routed last, after all gate fan-in, because they are the
    // longest nets in the build and the router's difficulty ordering only
    // applies within the gate work list. Doing them here at least means they
    // route into a grid whose local congestion is already known.
    if !bank_flops.is_empty() {
        // A control lever drives every flop, so give each one a spine to branch
        // from rather than routing many nets out of a single dust cell.
        // The trunk runs *north*, the length of the bank, and is refreshed.
        //
        // It used to be five cells of dust running south, away from the bank -
        // and every one of the clock, inverted-clock and reset routes had to
        // leave from those five cells. Eleven flip-flops is forty-four such
        // routes; the first few take the spine's exits and the rest report "no
        // route from any of 6 sources" with the search exhausted rather than out
        // of budget. Five cells of dust also die after fifteen blocks, so most
        // of a bank two hundred deep was out of reach even in principle.
        //
        // Running the length of the bank instead gives every flip-flop a tap a
        // few blocks from its own pads, which is what makes these the *easy*
        // routes rather than the hardest ones in the build.
        let ctrl_spine = |grid: &mut Grid, router: &mut Router, tap: Pos, net_id: Sig, reach: i32| {
            let mut cells = vec![tap];
            router.claim(tap, net_id);
            for t in 1..=reach {
                let p = (tap.0, tap.1, tap.2 - t);
                if !grid.is_free(p) || !grid.is_free((p.0, p.1 - 1, p.2)) {
                    break;
                }
                let _ = grid.set((p.0, p.1 - 1, p.2), Block::Solid(Material::Clock));
                router.claim(p, net_id);
                // A repeater faces the side it reads from, and this trunk runs
                // -Z, so it faces south back towards the lever. Repeater cells
                // are not branch points, so they stay out of the source list.
                if t % SPINE_REFRESH == 0 {
                    let _ = grid.set(
                        p,
                        Block::Repeater { facing: Dir::South, delay: 1, powered: false },
                    );
                } else {
                    let _ = grid.set(p, Block::Dust { power: 0 });
                    cells.push(p);
                }
            }
            cells
        };

        // Long enough to run past the deepest flip-flop's pads.
        let reach = bank_flops
            .iter()
            .flat_map(|f| {
                f.clk_feeds.iter().chain(f.clk_n_feeds.iter()).chain(f.clr_feeds.iter())
            })
            .map(|p| p.2)
            .min()
            .map(|zmin| (clk_lever.unwrap().2 - zmin + 8).max(SPINE_LEN))
            .unwrap_or(SPINE_LEN);

        // Control nets get ids past the end of the signal space so they cannot
        // collide with a real signal's keepout.
        let base_net = net.sigs.len() as Sig;
        let clk_src = ctrl_spine(&mut grid, &mut router, clk_lever.unwrap(), base_net, reach);
        let clkn_src = ctrl_spine(&mut grid, &mut router, clk_n_lever.unwrap(), base_net + 1, reach);
        let rst_src = ctrl_spine(&mut grid, &mut router, rst_lever.unwrap(), base_net + 2, reach);

        for (i, f) in bank_flops.iter().enumerate() {
            // D: from whatever the netlist says drives this register.
            let d_sig = net.dffs[i].d;
            let sources = spine
                .get(&d_sig)
                .cloned()
                .or_else(|| source_of.get(&d_sig).map(|&p| vec![p]))
                .ok_or_else(|| format!("flip-flop {i} D signal {d_sig} has no driver"))?;
            // A spine tap costs at most one refresh interval; a bare Q port is a
            // repeater and costs nothing.
            let decay = if sources.len() > 1 { SPINE_REFRESH - 1 } else { 0 };
            router.claim(f.d_feeds[0], d_sig);
            staged_route(
                &mut grid,
                &mut router,
                d_sig,
                &sources,
                f.d_feeds[0],
                bounds,
                Material::Wire,
                decay,
                &mut chain_seq,
            )
            .map_err(|e| format!("routing D into flip-flop {i}: {e}"))?;

            for (label, src, dst, id) in [
                ("clk", &clk_src, f.clk_feeds[0], base_net),
                ("clk_n", &clkn_src, f.clk_n_feeds[0], base_net + 1),
            ] {
                router.claim(dst, id);
                staged_route(
                    &mut grid,
                    &mut router,
                    id,
                    src,
                    dst,
                    bounds,
                    Material::Clock,
                    SPINE_REFRESH - 1,
                    &mut chain_seq,
                )
                .map_err(|e| format!("routing {label} into flip-flop {i}: {e}"))?;
            }
            for (k, &clr) in f.clr_feeds.iter().enumerate() {
                router.claim(clr, base_net + 2);
                staged_route(
                    &mut grid,
                    &mut router,
                    base_net + 2,
                    &rst_src,
                    clr,
                    bounds,
                    Material::Clock,
                    SPINE_REFRESH - 1,
                    &mut chain_seq,
                )
                .map_err(|e| format!("routing reset {k} into flip-flop {i}: {e}"))?;
            }
        }
    }

    // Group input levers by port for the caller.
    let mut by_port: HashMap<u32, Vec<(u32, Pos)>> = HashMap::new();
    for (&s, &pos) in &source_of {
        if let Src::Input { port, bit } = *net.src(s) {
            // Report the lever itself, not the dust tap the router starts from.
            by_port.entry(port).or_default().push((bit, (pos.0, pos.1, pos.2 - 1)));
        }
    }
    let mut ports: Vec<u32> = by_port.keys().copied().collect();
    ports.sort();
    let mut input_levers = Vec::new();
    for p in ports {
        let mut v = by_port.remove(&p).unwrap();
        v.sort_by_key(|&(b, _)| b);
        input_levers.push((format!("port{p}"), v.into_iter().map(|(_, p)| p).collect()));
    }

    Ok(Layout {
        grid,
        input_levers,
        output_lamps,
        levels: max_level as usize + 1,
        gates: gates.len(),
        clk_lever: clk_lever.map(|t| (t.0, t.1, t.2 + 1)),
        clk_n_lever: clk_n_lever.map(|t| (t.0, t.1, t.2 + 1)),
        rst_lever: rst_lever.map(|t| (t.0, t.1, t.2 + 1)),
        flops: bank_flops.len(),
        flop_ports: bank_flops,
        gate_cells: placed
            .iter()
            .map(|(&sig, p)| (sig, (p.cell.out, p.cell.feeds.clone())))
            .collect(),
        wire_owner: router.owners().clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netlist::Netlist;
    use crate::redstone::Sim;

    /// Build a tiny netlist, place it, and check the redstone computes the
    /// same function the gates do.
    fn check(build_fn: impl Fn(&mut Netlist, &[Sig]) -> Sig, arity: u32) {
        let mut net = Netlist::new();
        let ins: Vec<Sig> = (0..arity).map(|i| net.input(0, i)).collect();
        let out = build_fn(&mut net, &ins);
        net.outputs.push(("q".into(), vec![out]));

        let layout = build(&net).expect("layout");
        let mut sim = Sim::new(&layout.grid);
        let levers = &layout.input_levers[0].1;
        let lamp = layout.output_lamps[0].1[0];

        for v in 0..(1u32 << arity) {
            for (i, &l) in levers.iter().enumerate() {
                sim.set_lever(l, (v >> i) & 1 == 1);
            }
            let (_, stable) = sim.run_until_stable(400);
            assert!(stable, "did not settle for input {v:0width$b}", width = arity as usize);

            let gsim = crate::netlist::GateSim::new(&net);
            let expect = gsim.eval(&[v as u64], false)[out as usize];
            assert_eq!(sim.lamp_lit(lamp), expect, "input {v:0width$b}", width = arity as usize);
        }
    }

    #[test]
    fn inverter_lays_out_and_runs() {
        check(|n, i| n.not(i[0]), 1);
    }

    // Known gap: multi-level nets have to cross one another, and this
    // floorplan has no spare Y layer to cross in - gates consume the vertical
    // budget, so two routes eventually contend for the same block. Solving it
    // properly needs a real detailed router (layer assignment + rip-up and
    // retry). These stay as failing-by-design specifications of the target
    // behaviour rather than being deleted.
    /// The whole pipeline, end to end: Ohm source text is parsed, lowered,
    /// bit-blasted, placed as redstone, exported to a schematic, and finally
    /// the *blocks themselves* are simulated and checked against the gate-level
    /// model. Nothing here is mocked.
    #[test]
    fn ohm_source_compiles_to_working_redstone() {
        let src = "input u1 a; output u1 q; proc main() { q = !a; }";
        let design = crate::lower::lower_program(&crate::parser::parse(src).unwrap()).unwrap();
        let net = crate::bitblast::blast_combinational(&design).unwrap();
        let layout = build(&net).expect("placement");

        // The emitted schematic must be a well-formed, non-trivial artifact.
        let schem = crate::schem::Schematic::from_grid(&layout.grid);
        let bytes = schem.to_bytes().unwrap();
        assert_eq!(&bytes[..2], &[0x1f, 0x8b], "schematic is gzip");
        assert!(schem.palette.iter().any(|p| p.starts_with("minecraft:redstone_wall_torch")));
        assert!(schem.palette.iter().any(|p| p.starts_with("minecraft:repeater")));

        // And the redstone has to actually invert.
        let mut sim = Sim::new(&layout.grid);
        let lever = layout.input_levers[0].1[0];
        let lamp = layout.output_lamps[0].1[0];
        for a in [false, true] {
            sim.set_lever(lever, a);
            let (_, stable) = sim.run_until_stable(400);
            assert!(stable, "redstone did not settle for a={a}");
            assert_eq!(sim.lamp_lit(lamp), !a, "compiled circuit must invert (a={a})");
        }
    }

    #[test]
    fn two_input_gates_lay_out_and_run() {
        check(|n, i| n.and(i[0], i[1]), 2);
        check(|n, i| n.or(i[0], i[1]), 2);
        check(|n, i| n.xor(i[0], i[1]), 2);
    }

    // Known limit: a primary input consumed at logic level 4+ has to fall ~30
    // blocks in a single route. Dust descends one block of Y per block of
    // horizontal travel, so that route needs ~30 blocks of horizontal room and
    // must also interleave flat runs for repeaters, since a repeater cannot sit
    // on a slope. The router finds paths but they switch back over themselves,
    // which breaks the slope (see `route::first_conflict`), and cell-granularity
    // rip-up cannot explore enough shapes to escape.
    //
    // The fix is relay points: break a long descent into per-level hops with a
    // repeater at each, so no single route ever spans the whole depth. Left as a
    // failing specification rather than deleted.
    #[test]
    fn three_input_logic_lays_out_and_runs() {
        check(|n, i| n.maj3(i[0], i[1], i[2]), 3);
        check(|n, i| n.xor3(i[0], i[1], i[2]), 3);
    }

    /// A bank must tile without overlapping and each register must hold its own
    /// value on a shared clock and a shared reset.
    #[test]
    fn register_bank_holds_independent_values() {
        use crate::redstone::Sim;
        use crate::world::Face;

        fn drive(g: &mut Grid, feed: Pos) -> Pos {
            let (x, y, z) = feed;
            g.force((x, y - 1, z), Block::Solid(Material::Wire));
            g.force((x, y, z), Block::Dust { power: 0 });
            g.force((x, y - 1, z - 1), Block::Solid(Material::Wire));
            g.force((x, y, z - 1), Block::Dust { power: 0 });
            let l = (x, y, z - 2);
            g.force((x, y - 1, z - 2), Block::Solid(Material::PortIn));
            g.force(l, Block::Lever { face: Face::Floor, facing: Dir::North, powered: false });
            l
        }

        let mut g = Grid::new();
        // Three forces a second row, so the Z pitch is exercised too, not just X.
        let bank = place_register_bank(&mut g, (0, 0, 0), 3).unwrap();
        assert_eq!(bank.flops.len(), 3);

        let d: Vec<Vec<Pos>> = bank
            .flops
            .iter()
            .map(|f| f.d_feeds.iter().map(|&p| drive(&mut g, p)).collect())
            .collect();
        let clk: Vec<Pos> =
            bank.flops.iter().flat_map(|f| f.clk_feeds.clone()).map(|p| drive(&mut g, p)).collect();
        // Second clock phase. Every flip-flop takes both from outside rather
        // than inverting internally, so the bank needs two spines, not one.
        let clkn: Vec<Pos> =
            bank.flops.iter().flat_map(|f| f.clk_n_feeds.clone()).map(|p| drive(&mut g, p)).collect();
        let rst: Vec<Pos> =
            bank.flops.iter().flat_map(|f| f.clr_feeds.clone()).map(|p| drive(&mut g, p)).collect();

        let mut sim = Sim::new(&g);
        let set_all = |sim: &mut Sim, ls: &[Pos], v: bool| {
            for &l in ls {
                sim.set_lever(l, v);
            }
        };

        // Pulse reset with the clock low: the built state is not the stamped one.
        set_all(&mut sim, &clk, false);
        set_all(&mut sim, &clkn, true);
        set_all(&mut sim, &rst, true);
        assert!(sim.run_until_stable(20000).1, "reset did not settle");
        set_all(&mut sim, &rst, false);
        assert!(sim.run_until_stable(20000).1, "reset release did not settle");
        for (i, f) in bank.flops.iter().enumerate() {
            assert_eq!(sim.field().dust_at(f.q), 0, "register {i} not cleared by reset");
        }

        // Load a distinct pattern and clock it in on a shared clock.
        let pattern = [true, false, true];
        for (i, &v) in pattern.iter().enumerate() {
            set_all(&mut sim, &d[i], v);
        }
        set_all(&mut sim, &clk, true);
        set_all(&mut sim, &clkn, false);
        assert!(sim.run_until_stable(20000).1, "clock high did not settle");
        set_all(&mut sim, &clk, false);
        set_all(&mut sim, &clkn, true);
        assert!(sim.run_until_stable(20000).1, "clock low did not settle");
        for (i, &v) in pattern.iter().enumerate() {
            assert_eq!(
                sim.field().dust_at(bank.flops[i].q) > 0,
                v,
                "register {i} holds the wrong bit; the bank is coupling"
            );
        }
    }

    /// A combinational design must answer the same regardless of history.
    ///
    /// This is the property the in-game harness sweeps for, reduced to a unit
    /// test. `add2` failed it for the project's whole life: correct on an
    /// ascending sweep, wrong on eight of sixteen cases descending, because a
    /// routed net ran beside itself and bridged one of its own repeaters into a
    /// latch. Nothing else in the suite could see that - every other check
    /// evaluates a circuit once, from a fresh state, which is exactly the case
    /// that works.
    #[test]
    fn placed_logic_is_history_independent() {
        use crate::redstone::Sim;

        let src = std::fs::read_to_string("examples/add2.ohm").unwrap();
        let prog = crate::parser::parse(&src).unwrap();
        let design = crate::lower::lower_program(&prog).unwrap();
        let net = crate::bitblast::blast_combinational(&design).unwrap();
        let lay = build(&net).unwrap();

        let levers: Vec<Pos> = lay.input_levers.iter().flat_map(|(_, v)| v.clone()).collect();
        let lamps: Vec<Pos> = lay.output_lamps.iter().flat_map(|(_, v)| v.clone()).collect();
        let n = 1usize << levers.len();

        let mut sim = Sim::new(&lay.grid);
        let sweep = |sim: &mut Sim, order: Vec<usize>| -> Vec<(usize, usize)> {
            let mut out = Vec::new();
            for v in order {
                for (b, &l) in levers.iter().enumerate() {
                    sim.set_lever(l, (v >> b) & 1 == 1);
                }
                assert!(sim.run_until_stable(20000).1, "case {v} did not settle");
                let f = sim.field();
                let got = lamps
                    .iter()
                    .enumerate()
                    .fold(0usize, |a, (i, &p)| a | ((f.block_powered(p) as usize) << i));
                out.push((v, got));
            }
            out
        };

        // Truth from the gate netlist: the placed circuit must not merely be
        // consistent between passes, it must be *right*. History-independence
        // alone would pass a circuit that is deterministically wrong - which is
        // exactly what add.ohm was, for three distinct router bugs, until the
        // sampled sweep in examples/two_pass.rs existed to notice.
        let gs = crate::netlist::GateSim::new(&net);
        let expect = |v: usize| -> usize {
            let (mut vals, mut rest) = (Vec::new(), v as u64);
            for p in &design.inputs {
                vals.push(rest & ((1u64 << p.width) - 1));
                rest >>= p.width;
            }
            let mut got = 0usize;
            let mut bit = 0;
            for (name, sigs) in &net.outputs {
                let val = gs.read_output(name, &vals, false);
                for k in 0..sigs.len() {
                    got |= (((val >> k) & 1) as usize) << bit;
                    bit += 1;
                }
            }
            got
        };

        let up = sweep(&mut sim, (0..n).collect());
        let down = sweep(&mut sim, (0..n).rev().collect());
        let down: HashMap<usize, usize> = down.into_iter().collect();
        for (v, a) in up {
            assert_eq!(
                a, down[&v],
                "input {v} reads {a} on an ascending sweep and {} on a descending one; \
                 the placed circuit is holding state it should not",
                down[&v]
            );
            assert_eq!(a, expect(v), "input {v}: placed circuit disagrees with the netlist");
        }
    }

    /// A netlist with state places: a register bank, a clock and reset
    /// distributed to it, and Q wired back as a source the combinational cone
    /// reads.
    ///
    /// The whole sequential path: a register bank, a clock and reset
    /// distributed to it, and Q wired back as a source the combinational cone
    /// reads.
    ///
    /// The circuit is the smallest real one there is - a flop fed its own
    /// inverted output - and it cannot be placed at all without treating Q as
    /// both a source and a sink.
    #[test]
    fn sequential_netlists_place() {
        let mut net = Netlist::new();
        let (idx, q) = net.add_dff("r");
        // Feed the flop its own inverted output: the smallest real sequential
        // circuit there is, and one that cannot be placed without treating Q as
        // both a source and a sink.
        let nq = net.nor(&[q]);
        net.set_dff_d(idx, nq);
        net.outputs.push(("q".into(), vec![q]));

        let layout = build(&net).expect("sequential netlist must place");
        assert_eq!(layout.flops, 1, "the register bank should hold one flop");
        assert!(layout.clk_lever.is_some(), "a clocked design needs a clock lever");
        assert!(layout.rst_lever.is_some(), "a clocked design needs a reset lever");
        assert!(!layout.grid.is_empty(), "placement produced no blocks");
    }
}
