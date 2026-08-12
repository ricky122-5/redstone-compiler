//! Does anything we emit contain a locked repeater?
//!
//! A repeater whose *side* is driven by another powered repeater freezes and
//! ignores its input. We serialise `locked=false`, but the game recomputes it
//! from the neighbours it finds, so this is a hazard the simulator cannot see
//! and only geometry can rule out.
//!
//! Suspected after the flip-flop's clock inverter went low on the first rising
//! edge in game and never came back, with every clock lever reading off.
use ohmc::tech::{stamp_d_latch, stamp_dff, stamp_rs_latch};
use ohmc::world::{down, offset, up, Block, Dir, Grid, Pos};

/// Repeaters that are locked by another repeater facing into their side.
fn locked(g: &Grid) -> Vec<(Pos, Pos)> {
    let mut out = Vec::new();
    for (&p, &b) in g.iter() {
        let Block::Repeater { facing, .. } = b else { continue };
        // The two sides are perpendicular to the input/output axis.
        let sides: [Dir; 2] = match facing {
            Dir::North | Dir::South => [Dir::East, Dir::West],
            Dir::East | Dir::West => [Dir::North, Dir::South],
        };
        for s in sides {
            let n = offset(p, s);
            if let Block::Repeater { facing: f2, .. } = g.get(n) {
                // A repeater outputs opposite the side it faces. If that output
                // lands on `p`, it is driving `p`'s side and locks it.
                if offset(n, f2.opposite()) == p {
                    out.push((p, n));
                }
            }
        }
    }
    out.sort();
    out
}

/// Torch supports with more than one thing able to power them.
///
/// A NOR cell's torch goes out exactly when its support block is powered, and
/// the only thing that should ever power it is that cell's own input pad. If a
/// routed wire also touches the support, the gate is shorted: it will follow
/// whichever net asserts and then sit there, which in game looks like a gate
/// that responds once and freezes.
fn shorted_supports(g: &Grid) -> Vec<(Pos, Pos, Vec<Pos>)> {
    let mut out = Vec::new();
    for (&p, &b) in g.iter() {
        let support = match b {
            Block::WallTorch { facing, .. } => offset(p, facing.opposite()),
            Block::Torch { .. } => down(p),
            _ => continue,
        };
        // Everything that can energise that block: dust on top, dust pointing
        // into it, or a repeater facing into it.
        let mut sources = Vec::new();
        if matches!(g.get(up(support)), Block::Dust { .. }) {
            sources.push(up(support));
        }
        for d in [Dir::North, Dir::South, Dir::East, Dir::West] {
            let n = offset(support, d);
            match g.get(n) {
                Block::Dust { .. } => sources.push(n),
                Block::Repeater { facing, .. } if offset(n, facing.opposite()) == support => {
                    sources.push(n)
                }
                _ => {}
            }
        }
        if sources.len() > 1 {
            sources.sort();
            out.push((p, support, sources));
        }
    }
    out.sort();
    out
}

fn main() {
    let cases: Vec<(&str, Box<dyn Fn(&mut Grid)>)> = vec![
        ("rs_latch", Box::new(|g: &mut Grid| {
            stamp_rs_latch(g, (0, 0, 0)).unwrap();
        })),
        ("d_latch", Box::new(|g: &mut Grid| {
            stamp_d_latch(g, (0, 0, 0)).unwrap();
        })),
        ("dff", Box::new(|g: &mut Grid| {
            stamp_dff(g, (0, 0, 0)).unwrap();
        })),
    ];

    let mut total = 0;
    for (name, build) in &cases {
        let mut g = Grid::new();
        build(&mut g);
        let reps = g.iter().filter(|(_, b)| matches!(b, Block::Repeater { .. })).count();
        let l = locked(&g);
        total += l.len();
        let sh = shorted_supports(&g);
        println!(
            "{name:<10} {reps:>4} repeaters, {} locked, {} shorted torch supports",
            l.len(),
            sh.len()
        );
        for (t, sup, srcs) in sh.iter().take(6) {
            println!("    torch {t:?} support {sup:?} powered by {srcs:?}");
        }
    }
    println!("\n{total} locked repeater(s) total");

    // What is physically touching the clock inverter's input? In game that pad
    // stays powered once the master's Q goes high, which points at a short, and
    // a short is something only geometry can show.
    let mut g = Grid::new();
    let p = stamp_dff(&mut g, (0, 0, 0)).unwrap();
    // Does a probe lamp land on a block that is a torch's support? The exporter
    // replaces the block *under* each node's output dust with a lamp. If that
    // block is also the support a torch is mounted on, the probe turns the gate
    // into a self-extinguishing loop: torch lit -> output dust powered -> lamp
    // powered -> support powered -> torch out. The gate would then follow one
    // change and freeze, which is exactly what the game shows.
    {
        let mut supports: std::collections::HashMap<Pos, Pos> = std::collections::HashMap::new();
        for (&tp, &tb) in g.iter() {
            let sup = match tb {
                Block::WallTorch { facing, .. } => offset(tp, facing.opposite()),
                Block::Torch { .. } => down(tp),
                _ => continue,
            };
            supports.insert(sup, tp);
        }
        println!("\nprobe lamps landing on a torch support:");
        let mut bad = 0;
        for (name, node) in [
            ("Q", p.q),
            ("MQ", p.master_q),
            ("SNOTER", p.slave_not_e_r),
            ("SROUT", p.slave_r_out),
            ("MNOTER", p.master_not_e_r),
            ("MROUT", p.master_r_out),
        ] {
            let lamp = (node.0, node.1 - 1, node.2);
            match supports.get(&lamp) {
                Some(t) => {
                    bad += 1;
                    println!("  {name:<7} lamp {lamp:?} IS the support of torch {t:?}");
                }
                None => println!("  {name:<7} lamp {lamp:?} ok"),
            }
        }
        println!("  {bad} probe(s) sitting on a torch support");
    }

    println!("\nneighbourhood of the clock inverter feed:");
    for &feed in &p.clk_feeds {
        println!("  clk feed {feed:?}");
        for dx in -2..=2i32 {
            for dy in -2..=2i32 {
                for dz in -2..=2i32 {
                    if (dx, dy, dz) == (0, 0, 0) {
                        continue;
                    }
                    let n = (feed.0 + dx, feed.1 + dy, feed.2 + dz);
                    let b = g.get(n);
                    if !matches!(b, Block::Air) {
                        let d = dx.abs() + dy.abs() + dz.abs();
                        if d <= 2 {
                            println!("    {n:?} d={d} {b:?}");
                        }
                    }
                }
            }
        }
    }
}
