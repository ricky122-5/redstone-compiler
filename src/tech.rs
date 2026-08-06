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
        let (_, stable) = sim.run_until_stable(200);
        assert!(stable, "circuit failed to settle");
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
