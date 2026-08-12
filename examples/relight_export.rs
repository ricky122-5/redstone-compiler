//! Export a single NOR cell with a lever and an output lamp.
//!
//! The smallest circuit that could reproduce the flip-flop's freeze: one gate,
//! driven high and low repeatedly in a single world. Everything larger has been
//! bisected by ten-minute server runs without result.
use ohmc::structure::to_mcfunction;
use ohmc::tech::stamp_nor;
use ohmc::world::{Block, Dir, Face, Grid, Material};

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "/tmp/relight.mcfunction".into());
    let mut g = Grid::new();
    let cell = stamp_nor(&mut g, (0, 0, 0), 1).unwrap();
    let feed = cell.feeds[0];

    // Lever driving the cell's input, the same way the test harness does.
    let (x, y, z) = feed;
    g.force((x, y - 1, z), Block::Solid(Material::Wire));
    g.force((x, y, z), Block::Dust { power: 0 });
    g.force((x, y - 1, z - 1), Block::Solid(Material::Wire));
    g.force((x, y, z - 1), Block::Dust { power: 0 });
    let lev = (x, y, z - 2);
    g.force((x, y - 1, z - 2), Block::Solid(Material::PortIn));
    g.force(lev, Block::Lever { face: Face::Floor, facing: Dir::North, powered: false });

    // Lamp under the output, as the other exporters do.
    let lamp = (cell.out.0, cell.out.1 - 1, cell.out.2);
    g.force(lamp, Block::Lamp { lit: false });

    let lo = g.bounds().unwrap().0;
    let rel = |p: (i32, i32, i32)| (p.0 - lo.0, p.1 - lo.1 + 1, p.2 - lo.2);
    let (lx, ly, lz) = rel(lev);
    let (ox, oy, oz) = rel(lamp);
    std::fs::write(
        format!("{out}.manifest"),
        format!("IN {lx} {ly} {lz}\nOUT {ox} {oy} {oz}\n"),
    )
    .unwrap();
    std::fs::write(&out, to_mcfunction(&g, (0, 1, 0))).unwrap();
    eprintln!("IN {lx} {ly} {lz}\nOUT {ox} {oy} {oz}\nblocks={}", g.len());
}
