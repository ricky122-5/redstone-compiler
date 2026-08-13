//! Technology mapping: the redstone cell library.
//!
//! Everything the compiler emits is built from one logic cell (a multi-input
//! NOR) plus wiring primitives. The geometry below is the load-bearing part of
//! the whole project, so each piece is justified against the rules in
//! [`crate::redstone`] and pinned by simulator tests at the bottom of this file.
//!
//! # The NOR cell
//!
//! Looking down the +Z axis, with `d` the cell's facing direction:
//!
//! ```text
//!   z = ZG-2   feed points: drivers must deliver dust here
//!   z = ZG-1   repeaters, one per fan-in, driving south into the pad
//!   z = ZG     input pad: substrate, dust on top, opaque roof above
//!   z = ZG+1   wall torch, attached to the pad's first support block
//!   z = ZG+2   output dust, beside the torch and one level below the pad
//! ```
//!
//! Why each piece is there:
//!
//! * Fan-in arrives through **repeaters**, not bare dust. Repeaters are diodes,
//!   so several driven nets can merge onto one pad without back-feeding each
//!   other, and each one restores the signal to full strength.
//! * The **pad dust sits on top of** the torch's support block. Dust-on-top is
//!   the one unambiguous way to weakly power a block, and a weakly powered
//!   block is exactly what extinguishes a torch.
//! * The **output is beside the torch, not above it.** A lit torch *strongly*
//!   powers the block above itself, and a strongly powered block energises every
//!   adjacent dust - including the input pad one block away. Routing the output
//!   over the top therefore turns the gate into an oscillator. Taking it from
//!   the side costs one level of drop and keeps input and output isolated.
//! * The **roof** seals the pad from above so nothing routed overhead can form
//!   an up-slope connection into the gate's input.

use crate::world::{Block, Dir, Grid, Material, Pos};

/// Signal strength budget for a dust run before a repeater is required.
/// Dust starts at 15 and dies at 0, so 14 blocks is the safe span.
pub const MAX_RUN: i32 = 13;

/// Y offsets within a cell, relative to the cell's base (the support level).
///
/// A gate's output emerges one level *below* its input. That asymmetry is
/// forced by physics rather than chosen: the block above a torch is strongly
/// powered, and a strongly powered block energises every adjacent dust,
/// including the gate's own input pad. Taking the output from beside the torch
/// instead keeps input and output electrically isolated. Routing pays for it
/// with a one-block ramp between levels.
pub mod plane {
    /// Torch support blocks and the pad substrate.
    pub const SUPPORT: i32 = 0;
    /// Input pad dust and the fan-in repeaters.
    pub const IN: i32 = 1;
    /// Opaque roof over the pad, isolating it from anything routed above.
    pub const ROOF: i32 = 2;
    /// Output dust, one level below the input plane.
    pub const OUT: i32 = 0;
}

/// A placed NOR cell: where its inputs must be delivered and where its output
/// appears.
#[derive(Debug, Clone)]
pub struct NorCell {
    /// One feed point per fan-in. A driver must deliver dust here, at
    /// `plane::IN`, so the repeater sees it on its input side.
    pub feeds: Vec<Pos>,
    /// The cell's output dust, at `plane::OUT`.
    pub out: Pos,
    /// Columns the cell occupies in X, starting at its base.
    pub width: i32,
}

/// Stamp a NOR cell with `fanin` inputs.
///
/// `base` is the substrate corner `(x, y, z)`. The cell grows toward +X for
/// additional fan-in and toward +Z for its output.
pub fn stamp_nor(g: &mut Grid, base: Pos, fanin: usize) -> Result<NorCell, String> {
    let (x, y, z) = base;
    let fanin = fanin.max(1);
    // Fan-in columns are spaced two apart. Adjacent feed points would be
    // orthogonally adjacent dust, which conducts - so the gate's separate input
    // nets would short together. The pad between them is deliberately continuous
    // (it is one net), but the feeds must not touch.
    let pitch = 2i32;
    let pad_width = (fanin as i32 - 1) * pitch + 1;
    let mut feeds = Vec::with_capacity(fanin);

    for i in 0..fanin as i32 {
        let cx = x + i * pitch;
        // Repeater row: reads from the north, drives south into the pad.
        g.set((cx, y + plane::SUPPORT, z - 1), Block::Solid(Material::Wire))?;
        g.set(
            (cx, y + plane::IN, z - 1),
            Block::Repeater { facing: Dir::North, delay: 1, powered: false },
        )?;
        // The driver must deliver dust here for the repeater to read.
        feeds.push((cx, y + plane::IN, z - 2));
    }

    // Input pad: one continuous dust net across the full width, on substrate,
    // sealed above by an opaque roof.
    for i in 0..pad_width {
        g.set((x + i, y + plane::SUPPORT, z), Block::Solid(Material::Gate))?;
        g.set((x + i, y + plane::IN, z), Block::Dust { power: 0 })?;
        g.set((x + i, y + plane::ROOF, z), Block::Solid(Material::Shield))?;
    }

    // Torch on the south face of the pad's first support block. The output is
    // the dust beside the torch, NOT the block above it: that block would be
    // strongly powered and would drive the input pad next door.
    g.set((x, y + plane::SUPPORT, z + 1), Block::WallTorch { facing: Dir::South, lit: true })?;
    g.set((x, y + plane::OUT - 1, z + 2), Block::Solid(Material::Gate))?;
    g.set((x, y + plane::OUT, z + 2), Block::Dust { power: 0 })?;

    Ok(NorCell { feeds, out: (x, y + plane::OUT, z + 2), width: pad_width })
}

/// Tracks how far a signal has travelled since it was last restored to full
/// strength.
///
/// The budget must be threaded across an *entire* driver-to-sink path, not per
/// straight segment. A path assembled from several short runs still decays
/// monotonically, so a per-segment counter silently produces circuits whose
/// signal dies in transit.
pub struct Budget {
    since: i32,
}

impl Budget {
    /// Start at a fresh driver (a torch, repeater or lever), which emits 15.
    pub fn fresh() -> Budget {
        Budget { since: 0 }
    }

    /// Record one block of travel; returns true if a repeater is due here.
    fn step(&mut self) -> bool {
        self.since += 1;
        if self.since >= MAX_RUN {
            self.since = 0;
            true
        } else {
            false
        }
    }

    /// A ramp block costs strength but cannot hold a repeater, so it only
    /// consumes budget.
    fn step_no_repeater(&mut self) -> Result<(), String> {
        self.since += 1;
        if self.since >= MAX_RUN {
            return Err("ramp is too long to cross without a repeater".to_string());
        }
        Ok(())
    }
}

/// Lay a straight dust run along X at a fixed Y and Z, inserting repeaters so
/// the signal never decays to nothing.
///
/// `from_x` is the driven end; repeaters point back toward it, since a
/// repeater's `facing` runs from its output side to its input side. A repeater
/// is never placed on the first or last block of a segment: those are corners
/// where the path changes direction, and a repeater only conducts along its own
/// axis.
pub fn run_x(
    g: &mut Grid,
    y: i32,
    z: i32,
    from_x: i32,
    to_x: i32,
    material: Material,
    budget: &mut Budget,
) -> Result<(), String> {
    if from_x == to_x {
        return Ok(());
    }
    let step = if to_x > from_x { 1 } else { -1 };
    let facing = if step == 1 { Dir::West } else { Dir::East };
    let mut x = from_x;
    let mut first = true;
    while x != to_x {
        x += step;
        g.set((x, y - 1, z), Block::Solid(material))?;
        let due = budget.step();
        if due && !first && x != to_x {
            g.set((x, y, z), Block::Repeater { facing, delay: 1, powered: false })?;
        } else {
            if due {
                // Could not refresh at a corner; carry the cost forward.
                budget.since = MAX_RUN - 1;
            }
            g.set((x, y, z), Block::Dust { power: 0 })?;
        }
        first = false;
    }
    Ok(())
}

/// Lay a straight dust run along Z, same discipline as [`run_x`].
pub fn run_z(
    g: &mut Grid,
    y: i32,
    x: i32,
    from_z: i32,
    to_z: i32,
    material: Material,
    budget: &mut Budget,
) -> Result<(), String> {
    if from_z == to_z {
        return Ok(());
    }
    let step = if to_z > from_z { 1 } else { -1 };
    let facing = if step == 1 { Dir::North } else { Dir::South };
    let mut z = from_z;
    let mut first = true;
    while z != to_z {
        z += step;
        g.set((x, y - 1, z), Block::Solid(material))?;
        let due = budget.step();
        if due && !first && z != to_z {
            g.set((x, y, z), Block::Repeater { facing, delay: 1, powered: false })?;
        } else {
            if due {
                budget.since = MAX_RUN - 1;
            }
            g.set((x, y, z), Block::Dust { power: 0 })?;
        }
        first = false;
    }
    Ok(())
}

/// A dust staircase climbing or descending one level per step along Z.
///
/// Minecraft dust connects diagonally by one Y per horizontal block. The
/// connection exists only if the block **directly above the lower of the two
/// dusts** is non-opaque, so each step verifies exactly that cell. Returns the
/// Z coordinate the ramp finished on.
pub fn ramp_z(
    g: &mut Grid,
    x: i32,
    from: (i32, i32),
    to_y: i32,
    step_z: i32,
    material: Material,
    budget: &mut Budget,
) -> Result<i32, String> {
    let (mut y, mut z) = from;
    while y != to_y {
        let dy = if to_y > y { 1 } else { -1 };
        y += dy;
        z += step_z;
        g.set((x, y - 1, z), Block::Solid(material))?;
        g.set((x, y, z), Block::Dust { power: 0 })?;
        budget.step_no_repeater()?;
        // Descending, the new dust is the lower one; ascending, the previous
        // dust is. Either way the cell above the lower dust must stay clear.
        let above_lower = if dy < 0 { (x, y + 1, z) } else { (x, y, z - step_z) };
        if g.get(above_lower).is_opaque() {
            return Err(format!(
                "ramp at x={x} z={z}: {above_lower:?} is opaque and breaks the slope"
            ));
        }
    }
    Ok(z)
}

/// A lever on top of a block, used for input ports, reset, and the clock.
pub fn stamp_lever(g: &mut Grid, pos: Pos, material: Material, on: bool) -> Result<(), String> {
    g.set((pos.0, pos.1 - 1, pos.2), Block::Solid(material))?;
    g.set(pos, Block::Lever { face: crate::world::Face::Floor, facing: Dir::North, powered: on })?;
    Ok(())
}

/// A lamp driven by adjacent dust, used to display output bits.
pub fn stamp_lamp(g: &mut Grid, pos: Pos) -> Result<(), String> {
    g.set(pos, Block::Lamp { lit: false })?;
    Ok(())
}

/// Climb or descend to `to_y`, pausing on flat landings so the signal can be
/// refreshed. Returns the Z the staircase finished on.
///
/// A plain ramp cannot carry a repeater - a repeater needs the blocks either
/// side of it level - so a tall ramp spends its whole signal budget on height
/// and dies. Breaking the climb into short flights with a flat landing between
/// them gives each landing somewhere to put a repeater, which is the same
/// staircase the main layout uses for deep drops.
pub fn ramp_staged(
    g: &mut Grid,
    x: i32,
    from: (i32, i32),
    to_y: i32,
    step_z: i32,
    mat: Material,
    budget: &mut Budget,
) -> Result<i32, String> {
    // Short enough flights that a flight plus its landing always fits the
    // budget, leaving room for the horizontal run afterwards.
    const FLIGHT: i32 = 4;
    const LANDING: i32 = 3;

    let (mut y, mut z) = from;

    while y != to_y {
        let flight = (to_y - y).abs().min(FLIGHT);
        let dy = if to_y > y { 1 } else { -1 };
        for _ in 0..flight {
            y += dy;
            z += step_z;
            g.set((x, y - 1, z), Block::Solid(mat))?;
            g.set((x, y, z), Block::Dust { power: 0 })?;
            budget.step_no_repeater()?;
            let above_lower = if dy < 0 { (x, y + 1, z) } else { (x, y, z - step_z) };
            if g.get(above_lower).is_opaque() {
                return Err(format!("staircase at x={x} z={z} is roofed at {above_lower:?}"));
            }
        }
        if y != to_y {
            // Flat landing: a repeater in the middle restores full strength.
            for i in 0..LANDING {
                z += step_z;
                g.set((x, y - 1, z), Block::Solid(mat))?;
                if i == 1 {
                    let facing = if step_z > 0 { Dir::North } else { Dir::South };
                    g.set((x, y, z), Block::Repeater { facing, delay: 1, powered: false })?;
                    *budget = Budget::fresh();
                } else {
                    g.set((x, y, z), Block::Dust { power: 0 })?;
                    budget.step_no_repeater()?;
                }
            }
        }
    }
    Ok(z)
}

/// Wire one cell's output to another cell's input feed.
///
/// Every such link starts by climbing a level, because a gate's output plane
/// sits one below its input plane (see [`plane`]). Stages are therefore placed
/// at increasing Z so the ramp has somewhere to go.
pub fn link(g: &mut Grid, from_out: Pos, to_feed: Pos, mat: Material) -> Result<(), String> {
    link_at(g, from_out, to_feed, to_feed.1, mat)
}

/// Wire an output to a feed, carrying the signal over on its own Y plane.
///
/// Routing every link in the feed plane does not work once two of them converge
/// on the same cell: one link's vertical segment occupies a column across many
/// Z values, and the other's horizontal run crosses it. Giving each link a
/// private lane height means crossings pass over one another, which is the only
/// way a single wire plane can be avoided - and the same conclusion the main
/// router reached.
pub fn link_at(
    g: &mut Grid,
    from_out: Pos,
    to_feed: Pos,
    lane_y: i32,
    mat: Material,
) -> Result<(), String> {
    link_via(g, from_out, to_feed, lane_y, None, mat)
}

/// As [`link_at`], but descending at a chosen Z rather than as late as possible.
///
/// Descending directly onto a feed means dropping through whatever sits above
/// it, and above a macro's feed is that macro's own internal wiring. Coming
/// down early - in the clear gap between macros - and then approaching the feed
/// horizontally avoids the body entirely. Feeds are built to be entered from the
/// north, so a flat approach is what they expect.
pub fn link_via(
    g: &mut Grid,
    from_out: Pos,
    to_feed: Pos,
    lane_y: i32,
    descend_by: Option<i32>,
    mat: Material,
) -> Result<(), String> {
    let mut bud = Budget::fresh();
    // Climb to the lane, cross, then descend onto the feed. The descent has to
    // begin far enough back in Z to land exactly on the feed, since dust drops
    // one level per block travelled.
    let z1 = ramp_staged(g, from_out.0, (from_out.1, from_out.2), lane_y, 1, mat, &mut bud)?;
    run_x(g, lane_y, z1, from_out.0, to_feed.0, mat, &mut bud)?;
    // The descent is a staircase too, so it costs more Z than its height.
    // A staircase costs its height plus a landing between each flight, and it
    // must start on a full budget - a flight cannot refresh mid-climb. So the
    // descent is preceded by an explicit landing, laid here rather than inside
    // `ramp_staged`, which keeps the cost a pure function of the height and
    // therefore predictable enough to work backwards from the target.
    const LAND: i32 = 3;
    let h = lane_y - to_feed.1;
    let flights = (h + 3) / 4;
    let drop = h + LAND * (flights - 1).max(0) + LAND;
    // Land by `descend_by` when given, so the drop happens in open ground and
    // the last stretch into the feed is flat.
    let land_at = descend_by.unwrap_or(to_feed.2);
    let z_turn = land_at - drop;
    if z_turn < z1 {
        return Err(format!(
            "link from {from_out:?} to {to_feed:?} on lane y={lane_y} has no room to descend"
        ));
    }
    run_z(g, lane_y, to_feed.0, z1, z_turn, mat, &mut bud)?;
    // Landing: refresh so the descent starts with the whole budget.
    for i in 1..=LAND {
        let p = (to_feed.0, lane_y, z_turn + i);
        g.set((p.0, p.1 - 1, p.2), Block::Solid(mat))?;
        if i == 2 {
            g.set(p, Block::Repeater { facing: Dir::North, delay: 1, powered: false })?;
            bud = Budget::fresh();
        } else {
            g.set(p, Block::Dust { power: 0 })?;
        }
    }
    let end = ramp_staged(g, to_feed.0, (lane_y, z_turn + LAND), to_feed.1, 1, mat, &mut bud)?;
    if end != land_at {
        return Err(format!("descent landed at z={end}, wanted {land_at}"));
    }
    // Flat approach along the feed plane.
    run_z(g, to_feed.1, to_feed.0, end, to_feed.2, mat, &mut bud)?;
    Ok(())
}

/// A cross-coupled NOR latch: the memory element every flip-flop is built from.
///
/// Two NOR cells, each feeding the other's input. Raise one input and that
/// torch goes out, releasing the other, which then holds itself lit through the
/// cross-coupling. The latch remembers which input was raised last.
///
/// This is the first structure in the compiler with feedback, and the router
/// cannot express it: levelisation assumes a DAG and a cycle has no levels. So
/// the wiring is hand-placed and the whole thing is handed to the placer as an
/// opaque macro - the way a standard-cell library ships a flip-flop rather than
/// asking a router to build one.
///
/// The two cells are offset in Z as well as X. Side by side, each one's feedback
/// wire runs straight through the other's output dust; separate Z bands give the
/// forward link clear space, and the return link gets its own column to the west.
///
/// # Asynchronous clear
///
/// B takes a third fan-in used as `CLR`. Q is B's output and B is a NOR, so
/// raising CLR pulls Q low no matter what the feedback loop is doing, and the
/// loop re-settles cleared when CLR drops. That is the only way to put this
/// latch into a known state in the real game: the initial `lit=`/`powered=`
/// chosen at stamp time does survive export, but a `.mcfunction` places blocks
/// one `setblock` at a time and Minecraft re-evaluates every torch and repeater
/// as its neighbours appear, so the built circuit settles into whatever state
/// the placement order produces rather than the one we picked. State has to be
/// driven in after construction, not born in.
///
/// Returns `(set_feed, reset_feed, clr_feed, q, q_not)`.
pub fn stamp_rs_latch(g: &mut Grid, base: Pos) -> Result<(Pos, Pos, Pos, Pos, Pos), String> {
    let (x, y, z) = base;
    const DZ: i32 = 8;
    // Fan-in two, not one: each NOR takes the cross-coupled feedback on one
    // input and the external set/reset on the other. With a single input the
    // feedback and the external drive land on the same wire, shorted together,
    // and the latch degenerates into a follower that cannot hold anything.
    let a = stamp_nor(g, (x, y, z), 2)?;
    // B gets a third input: the asynchronous clear. See the doc comment.
    let b = stamp_nor(g, (x + 6, y, z + DZ), 3)?;

    // Start the loop in a state that is consistent all the way round, not just
    // at the torches.
    //
    // Both torches are stamped lit, which a cross-coupled pair cannot be, so B
    // starts dark to pick a winner. But that alone is not enough: the repeaters
    // carrying the loop also hold state, and they start unpowered. A stamped
    // with output high and the repeater relaying it starting low is an
    // inconsistency, and it launches a one-tick pulse. In a loop with two
    // inversions - non-inverting overall - that pulse circulates forever
    // instead of dying out. Real redstone damps it via torch burnout; a
    // deterministic simulator rings.
    g.force(
        (x + 6, y + plane::SUPPORT, z + DZ + 1),
        Block::WallTorch { facing: Dir::South, lit: false },
    );
    // A drives high, so the repeater feeding B's loop input starts powered.
    g.force(
        (b.feeds[0].0, b.feeds[0].1, b.feeds[0].2 + 1),
        Block::Repeater { facing: Dir::North, delay: 1, powered: true },
    );

    // A -> B. A gate's output plane sits one below its input plane, so every
    // link starts by climbing a level.
    // A drives high at rest, so every repeater on the A -> B path must start
    // powered. Missing one leaves the loop inconsistent, which launches a
    // one-tick pulse that circulates forever round a non-inverting loop. The
    // path now contains refresh repeaters that `link` inserts on its own, so
    // they are found by diffing the grid rather than assumed to be at a known
    // spot.
    let before: Vec<Pos> = g
        .iter()
        .filter(|(_, b)| matches!(b, Block::Repeater { .. }))
        .map(|(p, _)| *p)
        .collect();
    link(g, a.out, b.feeds[0], Material::Gate)?;
    let added: Vec<Pos> = g
        .iter()
        .filter(|(p, b)| matches!(b, Block::Repeater { .. }) && !before.contains(p))
        .map(|(p, _)| *p)
        .collect();
    for p in added {
        if let Block::Repeater { facing, delay, .. } = g.get(p) {
            g.force(p, Block::Repeater { facing, delay, powered: true });
        }
    }

    // B -> A, returning in a private column west of both cells.
    let ret = x - 3;
    let mut bud = Budget::fresh();
    let zb = ramp_z(g, b.out.0, (y + plane::OUT, b.out.2), y + plane::IN, 1, Material::Gate, &mut bud)?;
    run_x(g, y + plane::IN, zb, b.out.0, ret, Material::Gate, &mut bud)?;
    run_z(g, y + plane::IN, ret, zb, a.feeds[0].2, Material::Gate, &mut bud)?;
    run_x(g, y + plane::IN, a.feeds[0].2, ret, a.feeds[0].0, Material::Gate, &mut bud)?;

    // The *second* feed of each cell is the external input; the first carries
    // the loop.
    //
    // Q is B's output, not A's. Asserting the external input of a cross-coupled
    // NOR drives *that cell's* output low, so the cell fed by S produces !Q. A
    // is the cell fed by S, therefore Q = B. Returning A as Q made the whole
    // latch invert - Q followed !D - and because A is the cell initialised high,
    // it also made the latch power up set instead of clear. The unit test missed
    // both: it asserted only that q and q_not differ, never which was which.
    Ok((a.feeds[1], b.feeds[1], b.feeds[2], b.out, a.out))
}

/// # Status: boundary ports verified in real Minecraft
///
/// The latch drives correctly through its boundary ports in game - Q follows D
/// high, follows it low, and holds when the enable drops. So the port rework
/// achieves both things at once: the cell is game-correct *and* composable,
/// where the old design was game-correct and could not be composed.
///
/// Getting there needed the unsupported-dust fix. The leg to `r_gate` was not
/// weak or shorted; its target dust was resting on a repeater, which in
/// Minecraft is not a placement at all.
///
/// The flip-flop built from two of these is still wrong in game: the master
/// captures a 1 and resets on the asynchronous clear, but will not take a 0 from
/// D, and Q never rises.
///
/// That is a genuinely odd result, because the master is stamped first into an
/// empty grid - so its blocks are *identical* to the standalone latch that does
/// reset in game - and it is driven by levers on the same ports. Three things
/// that could have made the difference are ruled out, each by measurement rather
/// than argument:
///
/// * **Loading Q.** A standalone latch with a long routed wire hung off Q
///   (`OHMC_LOAD_Q=1 cargo run --example dlatch_export`) sets, resets and holds
///   correctly in game. So driving the slave is not what breaks the master.
/// * **The probe lamps.** Simulating with them inserted (`dff_seq --probes`)
///   changes nothing.
/// * **Lever attachment clobbering the circuit.** The exporters use `force`,
///   which overwrites silently, but `examples/export_audit.rs` shows the only
///   overwrites are Gate-to-Wire swaps between two solid blocks.
///
/// So whatever is left is caused by something placed *after* the master: the
/// slave macro, or the outer route between them. Since the master's own blocks
/// cannot have changed, the next thing to check is what those later placements
/// put near the master - not the master itself.
///
/// # Superseded: ports were in the wrong place
///
/// **This is a regression against the previous design in one respect, and it is
/// deliberate but unresolved.** The old latch exposed raw internal feeds; driven
/// by levers sitting directly on them it was verified correct in Minecraft, but
/// it could not be composed - the flip-flop built from two of them failed,
/// because the master\'s Q had to thread into the slave\'s buried feeds.
///
/// The boundary ports fix composition in simulation: the flip-flop now captures
/// a 1, holds while D moves, and captures a 0. In game the latch itself now
/// fails, at the first stage: with E=1 and D=1 the reset gate asserts, so
/// `r_gate` sees D=0 while the D port reads high. One fan-out leg is dead.
///
/// Decay is ruled out. That leg measured 5 of 15 arriving; routing it with 4
/// levels of pessimism lifted it to 9 and the game behaves identically. Lowering
/// `MAX_RUN` globally lifts it further but breaks the placer, whose gate arrays
/// cannot absorb more repeaters.
///
/// So there is a systematic divergence between this simulator and Minecraft on
/// router-produced wires, and it is now reproducible in the smallest case yet:
/// one leg, inside one macro, with everything else verified. That is a much
/// better bug than the flip-flop was, and it is the thing to chase next -
/// against the router and the dust model, not against the latch.
///
/// # Its ports are in the wrong place, and that is the open bug
///
/// `d_a` and `d_b` are the feeds of two *internal* gates - `not_d` and `r_gate` -
/// which sit two and four Z stages deep inside the macro. So are the enables.
/// Anything outside that wants to drive this latch has to thread a wire through
/// the macro\'s own occupied space to reach them.
///
/// That is fine when the driver is a lever sitting right on the feed, which is
/// how every passing in-game test has driven it, and it is why this latch is
/// verified correct in Minecraft on its own. It is not fine inside
/// [`stamp_dff`], where the master\'s Q has to reach the slave\'s buried D feeds,
/// and that flip-flop does not work in game. Widening the gap between the two
/// latches made it *worse*, which pins the disturbance on those links rather
/// than on the macros being near each other.
///
/// Routing them over the top on a dedicated Y lane - the fix that shape of
/// evidence points to - was tried and cannot be done as things stand: the link
/// descends straight into the slave\'s body, because the port it is aiming for
/// is inside the body.
///
/// So the fix is an interface change, not a routing change. A cell library puts
/// its ports on the boundary; this one does not. Bringing `d`, `enable` and
/// `clr` out to the macro\'s north face - a short internal stub per port, laid
/// once when the macro is built and in known-clear space - would let every
/// external connection land flat on an edge and never enter the body at all.
/// That also removes the need to expose each input twice, since a boundary port
/// can fan out internally where the geometry is known.
///
/// A gated D latch: `Q` follows `D` while `enable` is high, and holds when it
/// falls. The storage element of a flip-flop.
///
/// Built as `S = D AND E`, `R = !D AND E` feeding an [`stamp_rs_latch`], with
/// the ANDs expressed in NOR form: `S = NOR(!D, !E)` and `R = NOR(D, !E)`.
///
/// Each gate gets its own X column *and* its own Z stage, so links always run
/// forward in Z on a lane unique to their source. That was meant to make the
/// macro collision-free by construction, and it is not sufficient: a link's
/// horizontal lane is unique, but its *vertical* segment occupies one column
/// across many Z values, and a later link's horizontal run crosses it. Two
/// links that converge on the same cell - as S and R do on the latch - cross by
/// necessity, and a single wiring plane has nowhere to put a crossing.
///
/// This is the same lesson the main router learned: a crossing needs a spare Y
/// layer. Hand-placed macro geometry does not get to skip it.
///
/// `D` and `enable` are each exposed **twice**, because two internal gates need
/// each of them. Fanning out inside the macro would mean two links leaving one
/// output and overlapping; the caller is already routing a net to many feeds, so
/// handing it two feed points costs nothing.
///
/// Returns [`DLatchPorts`]. It carries internal nodes as well as external ones
/// because the simulator rings on anything containing this macro, so the only
/// instrument that can see inside is the in-game harness - and that needs
/// coordinates.
pub struct DLatchPorts {
    /// Boundary ports on the macro's north face: one pad per logical input,
    /// fanned out internally. Drive these, not the `_a`/`_b` feeds.
    pub d: Pos,
    pub en: Pos,
    pub d_a: Pos,
    pub d_b: Pos,
    pub en_a: Pos,
    pub en_b: Pos,
    pub q: Pos,
    pub q_not: Pos,
    /// Asynchronous clear: hold high to force `q` low, release to hold cleared.
    pub clr: Pos,
    /// The two links into the latch. Exposed so their *arriving strength* can be
    /// measured, not just whether they arrive: a link landing on 1 here lands on
    /// 0 in game if this model's decay is even slightly generous, and it would
    /// be dead in one direction only.
    pub set_feed: Pos,
    pub reset_feed: Pos,
    /// Output of the inverter feeding the R gate's `!E` input.
    pub not_e_r: Pos,
    /// The reset gate's output.
    pub r_out: Pos,
}

pub fn stamp_d_latch(g: &mut Grid, base: Pos) -> Result<DLatchPorts, String> {
    use crate::route::Router;
    let (x, y, z) = base;
    // Stage pitch was sized for hand-placed wiring, which needed room to climb to
    // a lane, cross it, and descend again. The router does that job now and packs
    // it tighter, so the old pitch is waste - and waste matters here, since gcd
    // needs 42 of these.
    const DZ: i32 = 8;

    // Gates first, then let the real router wire them.
    //
    // Hand-placing these links cost four rounds and three distinct spacing
    // bugs - lanes clashing with feed rows, links crossing each other, ramps
    // spending their whole signal budget on height. Every one of those is
    // something `route.rs` already handles: keepout, per-net slots, staged
    // descents, repeater insertion. The only part that genuinely cannot be
    // routed is the latch's feedback cycle, because levelisation needs a DAG.
    // So the cycle stays hand-placed and everything else is delegated.
    let not_e_s = stamp_nor(g, (x, y, z), 1)?;
    let not_e_r = stamp_nor(g, (x + 10, y, z + DZ), 1)?;
    let not_d = stamp_nor(g, (x + 20, y, z + 2 * DZ), 1)?;
    let s_gate = stamp_nor(g, (x + 30, y, z + 3 * DZ), 2)?;
    let r_gate = stamp_nor(g, (x + 40, y, z + 4 * DZ), 2)?;
    let (set_feed, reset_feed, clr_feed, q, qn) = stamp_rs_latch(g, (x + 55, y, z + 6 * DZ))?;

    // At rest the enable is low, so !E is high and both S = NOR(!D,!E) and
    // R = NOR(D,!E) are low. Cells are stamped with their torch lit, which would
    // have S and R *both* asserting - telling the latch to set and reset at
    // once. That is not a state the latch can occupy, and it is what makes the
    // enclosing loop ring. Start them where the logic says they belong.
    for gate in [&s_gate, &r_gate] {
        let torch = (gate.out.0, y + plane::SUPPORT, gate.out.2 - 1);
        if matches!(g.get(torch), Block::WallTorch { .. }) {
            g.force(torch, Block::WallTorch { facing: Dir::South, lit: false });
        }
    }

    let mut router = Router::from_grid(g);
    let bounds = ((x - 40, y - 30, z - 40), (x + 140, y + 40, z + 9 * DZ + 40));

    // Each wire is its own net as far as the router is concerned.
    let wire = |g: &mut Grid, r: &mut Router, net: u32, from: Pos, to: Pos| -> Result<(), String> {
        r.claim(from, net);
        r.claim(to, net);
        r.route(g, net, &[from], to, bounds, Material::Gate, 0)
            .map_err(|e| format!("d-latch wire {net}: {e}"))
    };

    // Reserve every port target, the cell above it, *and the cell under it*,
    // before any routing at all - including the five internal wires below.
    //
    // A target's support is as much a part of the target as the target is. The
    // search exempts a target from its own support check, so an earlier route
    // that parks a repeater underneath one leaves dust resting on nothing. That
    // dust is gone the moment Minecraft places it, while this simulator models
    // it as ordinary wire - which is exactly what killed the leg to r_gate, and
    // why the latch was correct here and dead in game.
    for (net, t) in [
        (6u32, not_d.feeds[0]),
        (9, r_gate.feeds[0]),
        (7, not_e_s.feeds[0]),
        (10, not_e_r.feeds[0]),
        (8, clr_feed),
    ] {
        router.claim(t, net);
        router.reserve(t, net);
        router.reserve((t.0, t.1 + 1, t.2), net);
        router.reserve((t.0, t.1 - 1, t.2), net);
    }

    wire(g, &mut router, 1, not_d.out, s_gate.feeds[0])?;
    wire(g, &mut router, 2, not_e_s.out, s_gate.feeds[1])?;
    wire(g, &mut router, 3, not_e_r.out, r_gate.feeds[1])?;
    wire(g, &mut router, 4, s_gate.out, set_feed)?;
    wire(g, &mut router, 5, r_gate.out, reset_feed)?;

    // Boundary ports.
    //
    // Each input previously came out as the raw feed of an internal gate, two
    // or four Z stages deep in the body, so anything outside had to thread a
    // wire through this macro's occupied space to reach it. That is fine for a
    // lever placed directly on the feed - which is how every in-game test drove
    // this latch, and why it passes on its own - and not fine for a real driver,
    // which is why the flip-flop built from two of these fails in game.
    //
    // The fan-out moves inside instead. Each logical input gets one pad on the
    // north face, in clear ground, and the router - which already wires this
    // macro and is verified in game doing it - carries it to the internal gates
    // that need it. Callers then land flat on an edge and never enter the body.
    // Each port is a pad, a repeater, then the point the internal legs fan out
    // from. The repeater is not optional: the legs are routed with `decay: 0`,
    // which claims the source is at full strength, and a port driven from
    // outside is not - the flip-flop delivers 12 to the slave's D pad. Without
    // the refresh the router under-inserts repeaters on the longer leg and it
    // dies silently, which is the same failure that once left a spine tap dead.
    // The route reports success either way; only the logic is wrong.
    // `extra` adds delay on top of the refresh repeater.
    //
    // D is deliberately slower than the enable. Changing D and the enable in the
    // same instant is a setup violation, but a caller will do it, and with equal
    // delays the latch sees the new D while the enable is still high and
    // overwrites the bit it was told to keep. Holding D back by one more
    // repeater means the enable always closes the latch first, so the cell
    // tolerates a simultaneous change instead of corrupting on it.
    let pad = |g: &mut Grid, i: i32, reps: usize| -> Result<(Pos, Pos), String> {
        let x0 = x + i * 9;
        let y0 = not_d.feeds[0].1;
        let entry = (x0, y0, z - 8);
        let last = if reps == 0 { 1 } else { 2 * reps };
        for dz in 0..=last as i32 {
            let p = (x0, y0, z - 8 + dz);
            g.set((p.0, p.1 - 1, p.2), Block::Solid(Material::Gate))?;
            let b = if reps > 0 && dz % 2 == 1 {
                Block::Repeater { facing: Dir::North, delay: 1, powered: false }
            } else {
                Block::Dust { power: 0 }
            };
            g.set(p, b)?;
        }
        Ok((entry, (x0, y0, z - 8 + last as i32)))
    };
    // D is refreshed, the enable is not - which also makes the enable strictly
    // faster than D. That ordering is what lets the cell survive a caller
    // changing both in the same instant: the enable closes the latch before the
    // new data arrives, instead of the latch overwriting the bit it was keeping.
    // The enable can afford to skip the refresh because it is lever-driven both
    // standalone and inside the flip-flop, whereas D arrives from the master's Q
    // already down to 12.
    let (d_port, d_hub) = pad(g, 0, 1)?;
    let (en_port, en_hub) = pad(g, 1, 0)?;
    let (clr_port, clr_hub) = pad(g, 2, 0)?;

    // One net per logical input, fanned out to every gate that needs it. Same
    // net id for both legs so the router treats them as one signal and lets
    // them share dust rather than fighting over it.

    // The pad is deliberately re-claimed for each leg: it is one physical cell
    // acting as the source of two independent nets, which is exactly what a
    // fan-out point is.
    let fan = |g: &mut Grid, r: &mut Router, net: u32, from: Pos, to: Pos| -> Result<(), String> {
        r.claim(from, net);
        r.claim(to, net);
        // Claim a head start on the decay budget that has not actually been
        // spent. MAX_RUN is 13, which leaves a wire arriving with 2 of 15 to
        // spare - enough for the simulator and not enough for the game, which
        // is how a leg measured at 5 here turned out to be dead there.
        // Overstating the decay makes the router insert repeaters earlier and
        // the leg land around 10 instead. Done here rather than by lowering
        // MAX_RUN globally, because the placer's gate arrays are packed tightly
        // enough that refreshing more often makes routes unsatisfiable.
        const PESSIMISM: i32 = 4;
        r.route(g, net, &[from], to, bounds, Material::Gate, PESSIMISM)
            .map_err(|e| format!("d-latch port net {net} -> {to:?}: {e}"))
    };
    // Each leg is its own net, tapped off a different cell of a short spine.
    //
    // Sharing one net id between two legs does not work: the router then treats
    // their dust as interchangeable and is free to satisfy the second route by
    // reusing the first one's path, which leaves the second gate connected to
    // nothing. It looks like a routing success and behaves like an open circuit -
    // in the flip-flop it left the slave's R gate seeing D=0 while its D port
    // read 12, so R asserted forever and Q could never rise.
    // Enable first: its hub is one cell from the pad and therefore the most
    // easily walled in, and D's routes were doing exactly that.
    fan(g, &mut router, 7, en_hub, not_e_s.feeds[0])?;
    fan(g, &mut router, 10, en_hub, not_e_r.feeds[0])?;
    fan(g, &mut router, 6, d_hub, not_d.feeds[0])?;
    fan(g, &mut router, 9, d_hub, r_gate.feeds[0])?;
    fan(g, &mut router, 8, clr_hub, clr_feed)?;

    Ok(DLatchPorts {
        d: d_port,
        en: en_port,
        d_a: not_d.feeds[0],
        d_b: r_gate.feeds[0],
        en_a: not_e_s.feeds[0],
        en_b: not_e_r.feeds[0],
        q,
        q_not: qn,
        clr: clr_port,
        set_feed,
        reset_feed,
        not_e_r: not_e_r.out,
        r_out: r_gate.out,
    })
}

/// Extend a cell's output into a short spine of dust, so several links can
/// leave it from different points.
///
/// A cell's output is a single dust block, and two links starting there would
/// both ramp out of the same cell and overlap. Giving the output a spine is the
/// same answer the main router uses for high-fanout drivers: several places to
/// leave from rather than one.
pub fn stamp_out_spine(g: &mut Grid, out: Pos, len: i32, mat: Material) -> Result<Vec<Pos>, String> {
    stamp_out_spine_dir(g, out, len, (0, 1), mat)
}

/// A spine running in a chosen direction, given as `(dx, dz)`.
///
/// Direction matters: an RS latch's `q` has its own return wiring immediately
/// behind it in Z, so a spine extended that way collides with the macro that
/// produced it. Sideways is clear.
pub fn stamp_out_spine_dir(
    g: &mut Grid,
    out: Pos,
    len: i32,
    dir: (i32, i32),
    mat: Material,
) -> Result<Vec<Pos>, String> {
    // A repeater immediately after the output, so every tap downstream of it
    // starts from full strength again.
    //
    // Without it the spine is plain dust and each tap is weaker than the last.
    // That is not merely inefficient, it is a correctness problem: the flip-flop
    // feeds two *different* gates in the slave from two different taps, and if
    // the far tap dies while the near one survives, the latch sees D=1 on one
    // input and D=0 on the other. That makes S and R assert together, which is
    // the one state the latch cannot occupy - it stops responding entirely.
    // Measured at 11 and 7 of 15 arriving, which simulation tolerates and the
    // game did not.
    //
    // A repeater outputs opposite the side it faces, so it must face *back*
    // along the spine towards its input.
    let facing = match dir {
        (1, 0) => Dir::West,
        (-1, 0) => Dir::East,
        (0, 1) => Dir::North,
        (0, -1) => Dir::South,
        d => return Err(format!("spine direction {d:?} is not axis-aligned")),
    };
    let mut cells = vec![out];
    for i in 1..=len {
        let p = (out.0 + dir.0 * i, out.1, out.2 + dir.1 * i);
        g.set((p.0, p.1 - 1, p.2), Block::Solid(mat))?;
        if i == 1 {
            g.set(p, Block::Repeater { facing, delay: 1, powered: false })?;
        } else {
            g.set(p, Block::Dust { power: 0 })?;
        }
        cells.push(p);
    }
    Ok(cells)
}

/// A master-slave D flip-flop: `Q` takes the value of `D` on the clock's
/// falling edge, and holds it for the rest of the cycle.
///
/// Two [`stamp_d_latch`]es in series, the master enabled while the clock is
/// high and the slave while it is low. Only one is ever transparent, so data
/// cannot race through both in a single cycle - which is the whole point, and
/// the reason an FSM can have its next state depend on its current one.
///
/// Every input is exposed more than once rather than fanned out internally, for
/// the same reason the D latch does it: a caller routing a net to several feeds
/// costs nothing, whereas fanning out inside the macro means several links
/// leaving one output and overlapping.
///
/// # Two-phase clock
///
/// The slave's enable is an **input**, not something each flip-flop derives by
/// inverting the clock. Every flip-flop used to carry its own inverter, which
/// meant 42 of them in `gcd` all computing the same signal - and in game that
/// inverter was the component that failed: it went low on the first rising edge
/// and never came back, with every clock lever reading off.
///
/// Taking both phases from outside removes the failing part and 42 gates with
/// it, and matches how real sequential logic is clocked. The cost is that the
/// two phases must not overlap: if the master and slave are transparent at the
/// same instant, data races straight through both latches instead of being
/// held for an edge. Generating them from one global source is what keeps that
/// guarantee, and it is now the clock distribution's job rather than each
/// flip-flop's.
///
/// Returns [`DffPorts`], which carries the internal nodes as well as the
/// external ones, so both the simulator and the in-game harness can probe
/// inside. (The simulator used to ring on this circuit and be useless for
/// debugging it; that was a missing burnout rule, not the circuit.)
pub struct DffPorts {
    pub d_feeds: Vec<Pos>,
    /// Master enable: transparent while this is high.
    pub clk_feeds: Vec<Pos>,
    /// Slave enable, the opposite clock phase. Supplied externally rather than
    /// inverted inside each flip-flop - see the struct docs.
    pub clk_n_feeds: Vec<Pos>,
    /// Asynchronous clear, one feed per internal latch. Both must be driven:
    /// clearing only the slave leaves the master holding a stale value that the
    /// next clock edge would shift straight back in.
    pub clr_feeds: Vec<Pos>,
    pub q: Pos,
    /// The master latch's output, feeding the slave's D.
    pub master_q: Pos,
    /// Where that output lands on the slave. Exposed to measure the *arriving
    /// strength*, not just whether it arrives: this is the one path in the
    /// flip-flop made of parts not individually verified in game.
    pub slave_d_feeds: Vec<Pos>,
    /// The slave's `!E` inverter output and its reset gate output.
    pub slave_not_e_r: Pos,
    pub slave_r_out: Pos,
    /// The same two nodes in the master. The bare latch resets correctly in the
    /// game, so if the master's reset is dead here the difference is the
    /// flip-flop's wiring, not the latch.
    pub master_not_e_r: Pos,
    pub master_r_out: Pos,
}

pub fn stamp_dff(g: &mut Grid, base: Pos) -> Result<DffPorts, String> {
    use crate::route::Router;
    let (x, y, z) = base;

    // Measure each macro's footprint rather than guessing an offset: a caller
    // cannot see how much room a macro took.
    let end_z = |g: &Grid| g.bounds().map(|(_, hi)| hi.2).unwrap_or(z);
    // Kept tight on purpose. Widening this to 60 was tried in game to test
    // whether the two latches interfere, and it made things *worse*: the master
    // stopped capturing at all, even though its own structure and its
    // lever-driven inputs were unchanged and only the slave moved.
    //
    // The one thing that scales with this constant is the length of the two
    // routed links from the master's Q spine to the slave's D feeds. Longer
    // links, more damage - so the links are what disturb the circuit, not the
    // proximity of the two macros.
    const GAP: i32 = 14;

    let m = stamp_d_latch(g, (x, y, z))?;
    let (m_d, m_en, m_q, m_clr) = (m.d, m.en, m.q, m.clr);
    let sl = stamp_d_latch(g, (x, y, end_z(g) + GAP))?;
    let (s_d, s_en, s_q, s_clr) = (sl.d, sl.en, sl.q, sl.clr);

    let mut router = Router::from_grid(g);
    let bounds = ((x - 60, y - 40, z - 60), (x + 200, y + 60, end_z(g) + 60));

    // One link, not two.
    //
    // Both latches now expose boundary ports and fan out internally, so the
    // master's Q has a single destination on the slave's north face instead of
    // two feeds buried two and four stages inside its body. That halves this
    // connection and, more importantly, means it lands on an edge in clear
    // ground rather than threading through occupied space - which is what the
    // in-game evidence pinned the flip-flop's failure on.
    router.claim(m_q, 11);
    router.claim(s_d, 11);
    router
        .route(g, 11, &[m_q], s_d, bounds, Material::Gate, 0)
        .map_err(|e| format!("dff master->slave: {e}"))?;

    Ok(DffPorts {
        d_feeds: vec![m_d],
        clk_feeds: vec![m_en],
        clk_n_feeds: vec![s_en],
        clr_feeds: vec![m_clr, s_clr],
        q: s_q,
        master_q: m_q,
        slave_d_feeds: vec![s_d],
        slave_not_e_r: sl.not_e_r,
        slave_r_out: sl.r_out,
        master_not_e_r: m.not_e_r,
        master_r_out: m.r_out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redstone::Sim;
    use crate::world::Face;

    /// Drive a cell feed point from a lever, through enough dust to reach the
    /// repeater. Returns the lever position so the test can toggle it.
    fn drive(g: &mut Grid, feed: Pos) -> Pos {
        let (x, y, z) = feed;
        // Dust at the feed, then a lever two blocks north of it.
        g.force((x, y - 1, z), Block::Solid(Material::Wire));
        g.force((x, y, z), Block::Dust { power: 0 });
        g.force((x, y - 1, z - 1), Block::Solid(Material::Wire));
        g.force((x, y, z - 1), Block::Dust { power: 0 });
        let lever = (x, y, z - 2);
        g.force((x, y - 1, z - 2), Block::Solid(Material::PortIn));
        g.force(lever, Block::Lever { face: Face::Floor, facing: Dir::North, powered: false });
        lever
    }

    fn settle(sim: &mut Sim) {
        // Feedback loops take far longer to converge than the feed-forward
        // cells this helper was written for.
        let (ticks, stable) = sim.run_until_stable(5000);
        assert!(stable, "circuit failed to settle after {ticks} ticks");
    }

    /// A one-input NOR is an inverter. This is the single most important
    /// geometric claim in the compiler.
    #[test]
    fn nor_cell_with_one_input_inverts() {
        let mut g = Grid::new();
        let cell = stamp_nor(&mut g, (0, 0, 0), 1).unwrap();
        let lever = drive(&mut g, cell.feeds[0]);

        let mut sim = Sim::new(&g);
        settle(&mut sim);
        assert_eq!(sim.field().dust_at(cell.out), 15, "input low => output high");

        sim.set_lever(lever, true);
        settle(&mut sim);
        assert_eq!(sim.field().dust_at(cell.out), 0, "input high => output low");

        sim.set_lever(lever, false);
        settle(&mut sim);
        assert_eq!(sim.field().dust_at(cell.out), 15, "input low again => output high");
    }

    /// Full truth table of a two-input cell: output is high only when both
    /// inputs are low.
    #[test]
    fn nor_cell_with_two_inputs_matches_truth_table() {
        let mut g = Grid::new();
        let cell = stamp_nor(&mut g, (0, 0, 0), 2).unwrap();
        let l0 = drive(&mut g, cell.feeds[0]);
        let l1 = drive(&mut g, cell.feeds[1]);

        let mut sim = Sim::new(&g);
        for (a, b) in [(false, false), (true, false), (false, true), (true, true)] {
            sim.set_lever(l0, a);
            sim.set_lever(l1, b);
            settle(&mut sim);
            let high = sim.field().dust_at(cell.out) > 0;
            assert_eq!(high, !(a || b), "NOR({a}, {b})");
        }
    }

    /// The fan-in feeds must be electrically separate. Without this the four
    /// "inputs" merge into one shorted bus, and a NOR truth table still passes -
    /// because NOR of a shorted bus equals NOR of its members. This test fails
    /// on a shorted cell, where the truth table alone does not.
    #[test]
    fn fan_in_feeds_are_isolated_from_each_other() {
        let mut g = Grid::new();
        let cell = stamp_nor(&mut g, (0, 0, 0), 4).unwrap();
        let levers: Vec<Pos> = cell.feeds.iter().map(|&f| drive(&mut g, f)).collect();

        let mut sim = Sim::new(&g);
        // Raise only the first input.
        sim.set_lever(levers[0], true);
        settle(&mut sim);
        let f = sim.field();
        assert!(f.dust_at(cell.feeds[0]) > 0, "driven feed should be live");
        for i in 1..4 {
            assert_eq!(
                f.dust_at(cell.feeds[i]),
                0,
                "feed {i} must not pick up feed 0's signal"
            );
        }
    }

    #[test]
    fn nor_cell_with_four_inputs_matches_truth_table() {
        let mut g = Grid::new();
        let cell = stamp_nor(&mut g, (0, 0, 0), 4).unwrap();
        let levers: Vec<Pos> = cell.feeds.iter().map(|&f| drive(&mut g, f)).collect();

        let mut sim = Sim::new(&g);
        for v in 0..16u32 {
            for (i, &l) in levers.iter().enumerate() {
                sim.set_lever(l, (v >> i) & 1 == 1);
            }
            settle(&mut sim);
            let high = sim.field().dust_at(cell.out) > 0;
            assert_eq!(high, v == 0, "NOR of pattern {v:04b}");
        }
    }

    /// The pad must be electrically isolated from the gate's own output.
    #[test]
    fn input_pad_is_isolated_from_output() {
        let mut g = Grid::new();
        let cell = stamp_nor(&mut g, (0, 0, 0), 1).unwrap();
        drive(&mut g, cell.feeds[0]);
        let pad = (0, plane::IN, 0);
        // No connection of any kind from the pad toward the torch/output side.
        assert_eq!(g.dust_shape(pad).south, crate::world::Conn::None);
        // And the roof stops anything routed above from sloping down into it.
        assert!(g.get((0, plane::ROOF, 0)).is_opaque(), "pad must be roofed");
    }

    /// Two chained cells must give back the original signal.
    #[test]
    fn chained_cells_form_a_buffer() {
        let mut g = Grid::new();
        let a = stamp_nor(&mut g, (0, 0, 0), 1).unwrap();
        // Second cell far enough south that its repeater row clears cell A.
        let b = stamp_nor(&mut g, (0, 0, 6), 1).unwrap();
        let lever = drive(&mut g, a.feeds[0]);
        // A's output sits at plane::OUT; B's feed is at plane::IN, one level up.
        let mut budget = Budget::fresh();
        let z = ramp_z(&mut g, 0, (plane::OUT, a.out.2), plane::IN, 1, Material::Wire, &mut budget)
            .unwrap();
        run_z(&mut g, plane::IN, 0, z, b.feeds[0].2, Material::Wire, &mut budget).unwrap();

        let mut sim = Sim::new(&g);
        settle(&mut sim);
        assert_eq!(sim.field().dust_at(b.out), 0, "0 -> NOT -> 1 -> NOT -> 0");

        sim.set_lever(lever, true);
        settle(&mut sim);
        assert_eq!(sim.field().dust_at(b.out), 15, "1 -> NOT -> 0 -> NOT -> 1");
    }


    /// The latch must *hold* a bit: raise an input, drop it, and the state stays.
    ///
    /// Getting here took three constraints, each found by measurement:
    /// fan-in two per cell, separate Z bands so the feedback wires miss each
    /// other's outputs, and - the one that actually mattered - initialising the
    /// *whole loop* consistently rather than just the torches.
    #[test]
    fn rs_latch_remembers() {
        let mut g = Grid::new();
        let (sf, rf, clr, q, _qn) = stamp_rs_latch(&mut g, (0, 0, 0)).unwrap();
        let s_lever = drive(&mut g, sf);
        let r_lever = drive(&mut g, rf);
        let c_lever = drive(&mut g, clr);

        let mut sim = Sim::new(&g);
        settle(&mut sim);
        let hi = |sim: &Sim| sim.field().dust_at(q) > 0;

        // Absolute, not merely different. Asserting only that set and reset
        // disagree is satisfied by an inverted latch too, and this cell was
        // wired inverted for a whole session behind exactly that assertion.
        sim.set_lever(r_lever, true);
        settle(&mut sim);
        assert!(!hi(&sim), "reset must drive Q low");
        sim.set_lever(r_lever, false);
        settle(&mut sim);
        assert!(!hi(&sim), "cleared state must survive its input dropping");

        sim.set_lever(s_lever, true);
        settle(&mut sim);
        assert!(hi(&sim), "set must drive Q high");
        sim.set_lever(s_lever, false);
        settle(&mut sim);
        assert!(hi(&sim), "the bit must be held, not merely tracked");

        // Asynchronous clear: Q must go low with set and reset both idle, and
        // stay low once the clear is released. This is what lets a built
        // circuit be put into a known state - the state chosen at stamp time
        // does not survive `.mcfunction` placement.
        sim.set_lever(c_lever, true);
        settle(&mut sim);
        assert!(!hi(&sim), "clear must force Q low from a set latch");
        sim.set_lever(c_lever, false);
        settle(&mut sim);
        assert!(!hi(&sim), "the latch must stay cleared after the pulse ends");

        // And it must still be usable afterwards.
        sim.set_lever(s_lever, true);
        settle(&mut sim);
        assert!(hi(&sim), "the latch must still set after being cleared");
    }


    /// The latch must follow D while enabled and freeze when the enable drops.
    ///
    /// **Verified in the real game too** (`tools/dlatch-validate.sh`):
    ///
    /// ```text
    /// enable=1 D=1   Q=off     set   (q is inverted: set pulls A low)
    /// enable=1 D=0   Q=ON      reset (Q moves the other way)
    /// enable=0 D=1   Q=ON      holds
    /// ```
    ///
    /// Both directions and the hold, on hardware rather than in our model of it.
    ///
    /// Getting here needed three fixes, each found by measurement:
    /// a stage pitch that keeps link lanes clear of feed rows, per-link Y lanes
    /// so converging links cross over rather than into each other, and
    /// staircased ramps so a link's climb does not spend its whole signal
    /// budget on height.
    #[test]
    fn d_latch_follows_then_holds() {
        let mut g = Grid::new();
        let p = stamp_d_latch(&mut g, (0, 0, 0)).unwrap();
        // Drive the boundary ports, which is what a caller has. Driving the
        // internal feeds directly - as this test used to - bypasses the macro's
        // own fan-out and tests a circuit no caller can build.
        let q = p.q;
        let d1 = drive(&mut g, p.d);
        let e1 = drive(&mut g, p.en);
        let mut sim = Sim::new(&g);

        let set = |sim: &mut Sim, d: bool, e: bool| {
            sim.set_lever(d1, d);
            sim.set_lever(e1, e);
            let (t, ok) = sim.run_until_stable(5000);
            assert!(ok, "did not settle after {t} ticks (d={d} e={e})");
        };

        // Transparent: Q tracks D while the enable is high.
        //
        // Assert the actual value, not merely that the two differ. The weaker
        // `assert_ne!` passed for a whole session while the latch was wired
        // inverted - Q followed !D - because an inverted latch also produces two
        // different answers. Polarity is the thing being tested, so test it.
        set(&mut sim, true, true);
        assert!(sim.field().dust_at(q) > 0, "Q must be high when D=1 and enabled");
        set(&mut sim, false, true);
        assert!(sim.field().dust_at(q) == 0, "Q must be low when D=0 and enabled");
        set(&mut sim, true, true);
        assert!(sim.field().dust_at(q) > 0, "Q must return high when D goes back to 1");

        // Opaque: drop the enable, then change D. Q must not move.
        set(&mut sim, false, false);
        let held = sim.field().dust_at(q) > 0;
        set(&mut sim, true, false);
        assert_eq!(sim.field().dust_at(q) > 0, held, "Q must hold once disabled");
    }


    /// A flip-flop must sample D on the clock edge and hold it, not follow D.
    ///
    /// Places cleanly - every geometry problem vanished at once when the wiring
    /// was handed to `route.rs` instead of being laid by hand. Four rounds of
    /// collisions (lanes clashing with feed rows, links crossing each other,
    /// ramps spending their budget on height, descents landing on top of a body)
    /// were all solved by machinery that already existed and was validated
    /// in-game.
    ///
    /// The master is transparent while the clock is high and the slave while it
    /// is low, so Q updates on the **falling** edge.
    ///
    /// # The bug this test now guards
    ///
    /// For most of a session this flip-flop could capture a 1 but not a 0, and
    /// the search went in two wrong directions before finding it. Both are worth
    /// recording, because both were reasonable and both were wasted effort:
    ///
    /// 1. *"The reset path is dead."* The probe showed `MQ` low through the
    ///    whole D=0 cycle, so the master looked like it never captured. It was
    ///    capturing - the port being read was inverted, so "low" was the right
    ///    answer to the wrong question.
    /// 2. *"The harness is lying."* It genuinely was, at least once: a probe read
    ///    the inverted clock high while the clock itself was high. But fixing the
    ///    harness was never going to find this, and each attempt cost ten minutes.
    ///
    /// The actual cause was in [`stamp_rs_latch`], one level below: it returned
    /// cell A as Q, but A is the cell fed by S, and asserting a cross-coupled
    /// NOR's external input drives *that cell* low. So Q followed !D. Composed
    /// into a flip-flop the two inversions cancelled for a 1 and did not for a 0,
    /// which is exactly the shape of the symptom.
    ///
    /// It survived so long because the D latch test asserted only that Q differed
    /// between D=1 and D=0. An inverted latch satisfies that too. Every assertion
    /// here is absolute for that reason.
    ///
    /// # Still open in the real game
    ///
    /// The simulator now captures both directions, and `tools/dff-validate.sh`
    /// does not agree: in game Q never rises, and at the D=0 clock-high stage the
    /// probe reads the inverted clock *high* while the clock levers read high.
    /// That reading is trustworthy now - the harness reads its levers back at
    /// every stage, so this is the circuit, not the measurement.
    ///
    /// The likely cause is initial state. This latch is hand-initialised (B's
    /// torch dark, the loop repeaters powered) and the exporter does preserve
    /// `lit=`/`powered=`, but a `.mcfunction` places blocks one `setblock` at a
    /// time and Minecraft re-evaluates every torch and repeater as its
    /// neighbours appear. The chosen state does not survive placement; the game
    /// settles into whichever state the placement order produces.
    ///
    /// If that is right, the fix is not more careful initialisation - it is an
    /// explicit **reset line** into the latch, so the circuit can be driven to a
    /// known state after it is built rather than born in one. The goal for this
    /// project already assumes one ("pulse reset, let it clock"), so the reset
    /// has to exist regardless.
    ///
    /// Finding it needed the simulator, which had been unusable on anything with
    /// feedback because it rang forever - and that turned out to be a missing
    /// rule rather than a broken circuit. Real redstone damps a circulating pulse
    /// through torch burnout; once `redstone.rs` modelled it, this settled at
    /// every stage and the fault was visible in under a second.
    ///
    /// No longer ignored. It was skipped because the flip-flop oscillated here
    /// forever, which was the simulator missing torch burnout rather than the
    /// circuit misbehaving; with burnout modelled it settles at every stage.
    #[test]
    fn dff_samples_on_the_clock_edge() {
        let mut g = Grid::new();
        let p = stamp_dff(&mut g, (0, 0, 0)).unwrap();
        let (q, dfs, cfs, rfs) = (p.q, p.d_feeds.clone(), p.clk_feeds.clone(), p.clr_feeds.clone());
        let dl: Vec<Pos> = dfs.iter().map(|&f| drive(&mut g, f)).collect();
        let cl: Vec<Pos> = cfs.iter().map(|&f| drive(&mut g, f)).collect();
        let rl: Vec<Pos> = rfs.iter().map(|&f| drive(&mut g, f)).collect();
        // The second clock phase. Driven as the exact complement here; keeping
        // the two non-overlapping is the clock distribution's job.
        let nl: Vec<Pos> = p.clk_n_feeds.iter().map(|&f| drive(&mut g, f)).collect();
        let mut sim = Sim::new(&g);

        let apply = |sim: &mut Sim, d: bool, c: bool| {
            for &l in &dl {
                sim.set_lever(l, d);
            }
            for &l in &cl {
                sim.set_lever(l, c);
            }
            for &l in &nl {
                sim.set_lever(l, !c);
            }
            let (t, ok) = sim.run_until_stable(20000);
            assert!(ok, "did not settle after {t} ticks (d={d} clk={c})");
        };

        // The master is transparent while the clock is high and the slave while
        // it is low, so Q updates on the falling edge.
        let hi = |sim: &Sim| sim.field().dust_at(q) > 0;

        // Pulse the asynchronous reset. This is how the built circuit is put
        // into a known state: the `lit=`/`powered=` chosen at stamp time does
        // not survive `.mcfunction` placement, because the game re-evaluates
        // every torch and repeater as its neighbours are placed.
        let pulse_reset = |sim: &mut Sim| {
            for &l in &rl {
                sim.set_lever(l, true);
            }
            assert!(sim.run_until_stable(20000).1, "reset did not settle");
            for &l in &rl {
                sim.set_lever(l, false);
            }
            assert!(sim.run_until_stable(20000).1, "reset release did not settle");
        };
        pulse_reset(&mut sim);
        assert!(!hi(&sim), "Q must be low after a reset pulse");

        // Reset must clear a *stored* 1, with the clock idle. Reset is pulsed
        // with the clock low on purpose: the master is transparent while the
        // clock is high, so releasing reset there just reloads D immediately -
        // correct for an asynchronous clear, and not a useful thing to assert.
        apply(&mut sim, true, true);
        apply(&mut sim, true, false);
        assert!(hi(&sim), "precondition: a 1 is stored");
        apply(&mut sim, false, false);
        pulse_reset(&mut sim);
        assert!(!hi(&sim), "a reset pulse must clear a stored 1");

        // And it must clear the master too, not just the slave: if the master
        // still held the 1, the next edge would shift it straight back in.
        apply(&mut sim, false, true);
        apply(&mut sim, false, false);
        assert!(!hi(&sim), "reset must clear the master, not only the slave");

        // Now clock a 0 in the ordinary way and confirm it agrees.
        apply(&mut sim, false, true);
        apply(&mut sim, false, false);
        assert!(!hi(&sim), "Q must be low after clocking in a 0");

        // Clock a 1 through.
        apply(&mut sim, true, true);
        apply(&mut sim, true, false);
        assert!(hi(&sim), "Q must be high after clocking in a 1");

        // Drop D with the clock idle. Q must NOT follow.
        apply(&mut sim, false, false);
        assert!(hi(&sim), "Q must hold between clock edges, not follow D");

        // Clock the 0 through. Asserted absolutely: the previous `assert_ne!`
        // against the earlier reading would have accepted any change at all,
        // and the bug this guards was the latch being wired inverted.
        apply(&mut sim, false, true);
        apply(&mut sim, false, false);
        assert!(!hi(&sim), "Q must take the 0 on the next edge");

        // And a 1 again, so the test covers both capture directions.
        apply(&mut sim, true, true);
        apply(&mut sim, true, false);
        assert!(hi(&sim), "Q must capture a 1 after having held a 0");
    }

    /// A run longer than the dust budget must still deliver full strength.
    #[test]
    fn long_runs_are_repeated() {
        let mut g = Grid::new();
        g.force((0, plane::IN - 1, 0), Block::Solid(Material::Wire));
        g.force((0, plane::IN, 0), Block::Lever { face: Face::Floor, facing: Dir::North, powered: true });
        run_x(&mut g, plane::IN, 0, 0, 40, Material::Wire, &mut Budget::fresh()).unwrap();

        let mut sim = Sim::new(&g);
        settle(&mut sim);
        assert!(
            sim.field().dust_at((40, plane::IN, 0)) > 0,
            "40-block run should survive via inserted repeaters"
        );
    }
}
