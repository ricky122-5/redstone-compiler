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
use ohmc::world::{offset, Block, Dir, Grid, Pos};

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
        println!("{name:<10} {reps:>4} repeaters, {} locked", l.len());
        for (p, by) in l.iter().take(8) {
            println!("    {p:?} locked by {by:?}");
        }
    }
    println!("\n{total} locked repeater(s) total");
}
