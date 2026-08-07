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
    let z_turn = to_feed.2 - drop;
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
    if end != to_feed.2 {
        return Err(format!("descent landed at z={end}, wanted {}", to_feed.2));
    }
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
/// Returns `(set_feed, reset_feed, q, q_not)`.
pub fn stamp_rs_latch(g: &mut Grid, base: Pos) -> Result<(Pos, Pos, Pos, Pos), String> {
    let (x, y, z) = base;
    const DZ: i32 = 8;
    // Fan-in two, not one: each NOR takes the cross-coupled feedback on one
    // input and the external set/reset on the other. With a single input the
    // feedback and the external drive land on the same wire, shorted together,
    // and the latch degenerates into a follower that cannot hold anything.
    let a = stamp_nor(g, (x, y, z), 2)?;
    let b = stamp_nor(g, (x + 6, y, z + DZ), 2)?;

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
    Ok((a.feeds[1], b.feeds[1], a.out, b.out))
}

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
/// Returns `(d_feed_a, d_feed_b, en_feed_a, en_feed_b, q, q_not)`.
pub fn stamp_d_latch(
    g: &mut Grid,
    base: Pos,
) -> Result<(Pos, Pos, Pos, Pos, Pos, Pos), String> {
    let (x, y, z) = base;
    // Stage pitch is set by what the links need, not by how big the cells are.
    //
    // Each link climbs to its own lane, crosses, and descends onto the feed, and
    // dust moves one block of Z per block of Y either way. So a link on a lane
    // `h` above the feed plane spends `2h` blocks of Z just changing height, and
    // the two stages it spans must be at least that far apart. With five lanes
    // at +2..+10 the deepest costs 19, so adjacent stages sit 28 apart.
    //
    // A pitch of 6 also put every link's lane adjacent to the next stage's feed
    // row, shorting it into that gate's input - which is what held the S gate's
    // pad high.
    const DZ: i32 = 28;

    // Stage per gate, marching forward in Z and X together.
    let not_e_s = stamp_nor(g, (x, y, z), 1)?;
    let not_e_r = stamp_nor(g, (x + 6, y, z + DZ), 1)?;
    let not_d = stamp_nor(g, (x + 12, y, z + 2 * DZ), 1)?;
    let s_gate = stamp_nor(g, (x + 18, y, z + 3 * DZ), 2)?;
    let r_gate = stamp_nor(g, (x + 26, y, z + 4 * DZ), 2)?;
    // Well clear of the gate stages: the two links into the latch are the
    // longest in the macro, and crowding them puts an inserted repeater on top
    // of the other link's dust.
    let (set_feed, reset_feed, q, qn) = stamp_rs_latch(g, (x + 40, y, z + 7 * DZ))?;

    // Every link gets its own lane height, three apart so wires on neighbouring
    // lanes cannot couple. Crossings then pass over one another.
    let lane = |i: i32| y + 2 + 2 * i;
    // S = NOR(!D, !E)
    link_at(g, not_d.out, s_gate.feeds[0], lane(0), Material::Gate)?;
    link_at(g, not_e_s.out, s_gate.feeds[1], lane(1), Material::Gate)?;
    // R = NOR(D, !E); D arrives from outside on feeds[0].
    link_at(g, not_e_r.out, r_gate.feeds[1], lane(2), Material::Gate)?;

    link_at(g, s_gate.out, set_feed, lane(3), Material::Gate)?;
    link_at(g, r_gate.out, reset_feed, lane(4), Material::Gate)?;

    Ok((not_d.feeds[0], r_gate.feeds[0], not_e_s.feeds[0], not_e_r.feeds[0], q, qn))
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
        let (sf, rf, q, _qn) = stamp_rs_latch(&mut g, (0, 0, 0)).unwrap();
        let s_lever = drive(&mut g, sf);
        let r_lever = drive(&mut g, rf);

        let mut sim = Sim::new(&g);
        settle(&mut sim);

        sim.set_lever(r_lever, true);
        settle(&mut sim);
        let after_reset = sim.field().dust_at(q) > 0;
        sim.set_lever(r_lever, false);
        settle(&mut sim);
        assert_eq!(
            sim.field().dust_at(q) > 0,
            after_reset,
            "state must survive its input dropping"
        );

        sim.set_lever(s_lever, true);
        settle(&mut sim);
        let after_set = sim.field().dust_at(q) > 0;
        assert_ne!(after_set, after_reset, "set and reset must reach different states");
        sim.set_lever(s_lever, false);
        settle(&mut sim);
        assert_eq!(
            sim.field().dust_at(q) > 0,
            after_set,
            "the bit must be held, not merely tracked"
        );
    }


    /// The latch must follow D while enabled and freeze when the enable drops.
    ///
    /// Getting here needed three fixes, each found by measurement:
    /// a stage pitch that keeps link lanes clear of feed rows, per-link Y lanes
    /// so converging links cross over rather than into each other, and
    /// staircased ramps so a link's climb does not spend its whole signal
    /// budget on height.
    #[test]
    fn d_latch_follows_then_holds() {
        let mut g = Grid::new();
        let (da, db, ea, eb, q, _qn) = stamp_d_latch(&mut g, (0, 0, 0)).unwrap();
        let d1 = drive(&mut g, da);
        let d2 = drive(&mut g, db);
        let e1 = drive(&mut g, ea);
        let e2 = drive(&mut g, eb);
        let mut sim = Sim::new(&g);

        let set = |sim: &mut Sim, d: bool, e: bool| {
            sim.set_lever(d1, d);
            sim.set_lever(d2, d);
            sim.set_lever(e1, e);
            sim.set_lever(e2, e);
            let (t, ok) = sim.run_until_stable(5000);
            assert!(ok, "did not settle after {t} ticks (d={d} e={e})");
        };

        // Transparent: Q tracks D while the enable is high.
        set(&mut sim, true, true);
        let q_hi = sim.field().dust_at(q) > 0;
        set(&mut sim, false, true);
        let q_lo = sim.field().dust_at(q) > 0;
        assert_ne!(q_hi, q_lo, "Q must follow D while enabled");

        // Opaque: drop the enable, then change D. Q must not move.
        set(&mut sim, false, false);
        let held = sim.field().dust_at(q) > 0;
        set(&mut sim, true, false);
        assert_eq!(sim.field().dust_at(q) > 0, held, "Q must hold once disabled");
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
