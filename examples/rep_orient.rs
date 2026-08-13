//! Which way does a repeater actually face in Minecraft?
//!
//! This project models a repeater's output as `opposite(facing)`. Every in-game
//! result we have would survive that being backwards, because mc-validate.sh
//! builds a fresh world per case and so only ever tests the first transition
//! after placement. The one test that drove a circuit through several changes in
//! one world is the one that froze.
//!
//! So: two identical lever-dust-repeater-dust-lamp chains, one with each
//! `facing`, driven repeatedly. Whichever lamp tracks the lever is the correct
//! convention.
use ohmc::structure::to_mcfunction;
use ohmc::world::{Block, Dir, Face, Grid, Material, Pos};

/// A chain running in +Z: lever, dust, repeater, dust, lamp.
fn chain(g: &mut Grid, x: i32, facing: Dir) -> (Pos, Pos) {
    let y = 1;
    for dz in 0..6 {
        g.force((x, y - 1, dz), Block::Solid(Material::Wire));
    }
    let lever = (x, y, 0);
    g.force(lever, Block::Lever { face: Face::Floor, facing: Dir::North, powered: false });
    g.force((x, y, 1), Block::Dust { power: 0 });
    g.force((x, y, 2), Block::Repeater { facing, delay: 1, powered: false });
    g.force((x, y, 3), Block::Dust { power: 0 });
    g.force((x, y, 4), Block::Dust { power: 0 });
    // The lamp sits under the last dust, as every probe in this project does.
    g.force((x, y - 1, 4), Block::Lamp { lit: false });
    (lever, (x, y - 1, 4))
}

fn main() {
    let out = "/tmp/reporient.mcfunction";
    let mut g = Grid::new();
    // North = -Z. Our model says output is opposite(facing), so facing=North
    // should drive the +Z end. If Minecraft reads `facing` as the output
    // direction instead, facing=South is the one that works.
    let (lev_a, lamp_a) = chain(&mut g, 0, Dir::North);
    let (lev_b, lamp_b) = chain(&mut g, 4, Dir::South);

    let lo = g.bounds().unwrap().0;
    let rel = |p: Pos| (p.0 - lo.0, p.1 - lo.1 + 1, p.2 - lo.2);
    let mut man = String::new();
    let a = rel(lev_a);
    let b = rel(lev_b);
    man.push_str(&format!("LEV {} {} {}\n", a.0, a.1, a.2));
    man.push_str(&format!("LEV {} {} {}\n", b.0, b.1, b.2));
    let la = rel(lamp_a);
    let lb = rel(lamp_b);
    man.push_str(&format!("NORTH_OUT {} {} {}\n", la.0, la.1, la.2));
    man.push_str(&format!("SOUTH_OUT {} {} {}\n", lb.0, lb.1, lb.2));

    std::fs::write(format!("{out}.manifest"), &man).unwrap();
    std::fs::write(out, to_mcfunction(&g, (0, 1, 0))).unwrap();
    eprintln!("{man}blocks={}", g.len());
}
