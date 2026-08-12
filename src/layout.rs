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
/// Largest vertical drop a single route may attempt.
///
/// Dust falls one block of Y per block of horizontal travel, and it cannot carry
/// a repeater on the way down because a repeater cannot sit on a slope. So a
/// descent of N costs N blocks of the 15-block signal budget with no chance to
/// refresh: past roughly the budget itself, a one-shot descent is impossible on
/// physics alone, and the only paths that exist switch back over themselves and
/// break the slope. Anything deeper gets broken into relay stages.
const MAX_DROP: i32 = 12;
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

/// Footprint of one flip-flop macro plus clearance, measured by
/// `examples/regbank_probe.rs`. Two macros at this pitch were verified to hold
/// independent values on a shared clock.
const FLOP_PITCH_X: i32 = 74;
const FLOP_PITCH_Z: i32 = 160;

/// Stamp `n` flip-flops in a bank based at `base`.
pub fn place_register_bank(g: &mut Grid, base: Pos, n: usize) -> Result<RegisterBank, String> {
    let (bx, by, bz) = base;
    // Near-square, so neither spine has to span the whole bank.
    let cols = (n as f64).sqrt().ceil().max(1.0) as usize;
    let mut flops = Vec::with_capacity(n);
    for i in 0..n {
        let (cx, cz) = (i % cols, i / cols);
        let at = (bx + cx as i32 * FLOP_PITCH_X, by, bz + cz as i32 * FLOP_PITCH_Z);
        flops.push(stamp_dff(g, at).map_err(|e| format!("register {i}: {e}"))?);
    }
    Ok(RegisterBank { flops })
}

/// Place and route a purely combinational netlist.
///
/// Returns an error if the netlist contains state: flip-flops need a sequential
/// floorplan (a clock spine and flip-flop macro cells) that this does not
/// attempt yet.
pub fn build(net: &Netlist) -> Result<Layout, String> {
    if !net.dffs.is_empty() {
        return Err(format!(
            "netlist has {} flip-flop(s); only combinational designs can be placed",
            net.dffs.len()
        ));
    }
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
        row.sort_by_key(|&g| {
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
            // Scaled mean, so ties break deterministically on gate id.
            let bary = if xs.is_empty() {
                i64::MAX / 2
            } else {
                xs.iter().sum::<i64>() * 1000 / xs.len() as i64
            };
            (bary, g)
        });
        let mut x = 0;
        for g in row {
            let k = net.operands(g).len().max(1);
            let cell = stamp_nor(&mut grid, (x, -l * LEVEL_H, GATE_Z), k)?;
            x += cell.width + GATE_GAP;
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
    let bounds = (
        (-8, -(max_level + 1) * LEVEL_H - 8, GATE_Z - depth),
        (span, lever_y + 4, GATE_Z + depth),
    );

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
    for (s, out) in drivers {
        let mut cells = vec![out];
        router.claim(out, s);
        for t in 1..=SPINE_LEN {
            let p = (out.0, out.1, out.2 + t);
            if !grid.is_free(p) || !grid.is_free((p.0, p.1 - 1, p.2)) {
                break;
            }
            grid.set((p.0, p.1 - 1, p.2), Block::Solid(Material::Wire))?;
            grid.set(p, Block::Dust { power: 0 })?;
            router.claim(p, s);
            cells.push(p);
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
    let stage_band = max_stages * RISER_PITCH + 8;

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
    for &(g, j, src, _) in &work {
        let feed = stub_entry[&(g, j)];
        let sources = spine
            .get(&src)
            .ok_or_else(|| format!("signal {src} has no driver"))?;
        // Worst case the branch leaves from the far end of the spine, so
        // charge the planner for the whole thing.
        let decay = sources.len() as i32 - 1;

        // Break a deep drop into stages, each landing on a relay.
        let top = sources[0].1;
        let drop = top - feed.1;
        let stages = if drop > MAX_DROP { (drop + MAX_DROP - 1) / MAX_DROP } else { 1 };

        let mut from: Vec<Pos> = sources.clone();
        let mut carry = decay;
        // Relay chains step from the driver toward the sink, so every hop is
        // short in X as well as Y. Parking the whole chain at either end just
        // moves the long hop to the other side: with it at the destination the
        // first stage spans the build, with it at the source the last one does.
        let src_x = sources[0].0;
        let chain_x: Vec<i32> = (1..stages)
            .map(|s| src_x + (feed.0 - src_x) * s / stages)
            .collect();
        // A Z band per chain, since two chains can share a column.
        let slot = if stages > 1 {
            let key = *chain_x.first().unwrap_or(&feed.0);
            let s = riser_slot.entry(key).or_insert(0);
            let v = *s;
            *s += 1;
            v
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
            let rz = RISER_Z0 + slot * stage_band + (stage - 1) * RISER_PITCH;
            let ry = top - stage * (drop / stages);
            let (rin, rout) = stamp_relay(&mut grid, (rx, ry, rz))?;
            router.claim(rin, src);
            router.claim(rout, src);
            router.block((rx, ry, rz));
            router
                .route(&mut grid, src, &from, rin, bounds, Material::Wire, carry)
                .map_err(|e| {
                    format!("relay stage {stage} for net {src} into gate {g} input {j}: {e}")
                })?;
            from = vec![rout];
            carry = 0;
        }
        router
            .route(&mut grid, src, &from, feed, bounds, Material::Wire, carry)
            .map_err(|e| {
                format!(
                    "routing net {src} into gate {g} input {j}: {e}\n\
                     note: {routed} of {total_conns} connections routed before this one failed"
                )
            })?;
        routed += 1;
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

    #[test]
    fn sequential_netlists_are_rejected() {
        let mut net = Netlist::new();
        let (_, q) = net.add_dff("r");
        net.outputs.push(("q".into(), vec![q]));
        let err = match build(&net) { Ok(_) => panic!("expected rejection"), Err(e) => e };
        assert!(err.contains("flip-flop"), "{err}");
    }
}
