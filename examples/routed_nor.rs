//! One NOR cell, driven through a routed wire, toggled repeatedly.
//!
//! The smallest untested combination. A bare NOR cell with a lever on its feed
//! works in game through repeated toggles; a lever-dust-repeater-dust chain
//! works too. What has never been tested is a NOR cell whose input arrives over
//! a wire the *router* built - which is what every port fan-out leg is, and
//! which is where the latch freezes.
use ohmc::route::Router;
use ohmc::structure::to_mcfunction;
use ohmc::tech::stamp_nor;
use ohmc::world::{Block, Dir, Face, Grid, Material, Pos};

fn main() {
    let out = "/tmp/routednor.mcfunction";
    let mut g = Grid::new();
    let cell = stamp_nor(&mut g, (0, 0, 0), 1).unwrap();
    // With OHMC_FANOUT, add a second cell fed from the same pad. That is the
    // enable port's exact shape, and the only structure in the latch not yet
    // tested in game on its own.
    let fanout = std::env::var("OHMC_FANOUT").is_ok();
    let cell2 = if fanout { Some(stamp_nor(&mut g, (16, 0, 12), 1).unwrap()) } else { None };

    // A pad well away from the cell, then let the router connect them.
    let pad = (cell.feeds[0].0 - 14, cell.feeds[0].1, cell.feeds[0].2 - 10);
    g.set((pad.0, pad.1 - 1, pad.2), Block::Solid(Material::Gate)).unwrap();
    g.set(pad, Block::Dust { power: 0 }).unwrap();

    let mut r = Router::from_grid(&g);
    r.claim(pad, 1);
    r.claim(cell.feeds[0], 1);
    r.reserve(cell.feeds[0], 1);
    r.reserve((cell.feeds[0].0, cell.feeds[0].1 - 1, cell.feeds[0].2), 1);
    let bounds = ((-60, -30, -60), (60, 30, 60));
    if let Some(c2) = &cell2 {
        r.claim(c2.feeds[0], 2);
        r.reserve(c2.feeds[0], 2);
        r.reserve((c2.feeds[0].0, c2.feeds[0].1 - 1, c2.feeds[0].2), 2);
    }
    r.route(&mut g, 1, &[pad], cell.feeds[0], bounds, Material::Gate, 4).expect("route");
    if let Some(c2) = &cell2 {
        r.claim(pad, 2);
        r.route(&mut g, 2, &[pad], c2.feeds[0], bounds, Material::Gate, 4).expect("route 2");
    }

    // Lever onto the pad, exactly as the exporters attach one.
    let (x, y, z) = pad;
    g.force((x, y - 1, z - 1), Block::Solid(Material::Wire));
    g.force((x, y, z - 1), Block::Dust { power: 0 });
    g.force((x, y - 1, z - 2), Block::Solid(Material::PortIn));
    let lever = (x, y, z - 2);
    g.force(lever, Block::Lever { face: Face::Floor, facing: Dir::North, powered: false });

    // Lamp under the cell's output, and one under the pad to see the wire's near end.
    g.force((cell.out.0, cell.out.1 - 1, cell.out.2), Block::Lamp { lit: false });
    g.force((pad.0, pad.1 - 1, pad.2), Block::Lamp { lit: false });

    let lo = g.bounds().unwrap().0;
    let rel = |p: Pos| (p.0 - lo.0, p.1 - lo.1 + 1, p.2 - lo.2);
    let mut man = String::new();
    let l = rel(lever);
    man.push_str(&format!("LEV {} {} {}\n", l.0, l.1, l.2));
    let o = rel((cell.out.0, cell.out.1 - 1, cell.out.2));
    man.push_str(&format!("NOROUT {} {} {}\n", o.0, o.1, o.2));
    let pp = rel((pad.0, pad.1 - 1, pad.2));
    man.push_str(&format!("PAD {} {} {}\n", pp.0, pp.1, pp.2));
    if let Some(c2) = &cell2 {
        g.force((c2.out.0, c2.out.1 - 1, c2.out.2), Block::Lamp { lit: false });
        let o2 = rel((c2.out.0, c2.out.1 - 1, c2.out.2));
        man.push_str(&format!("NOROUT2 {} {} {}\n", o2.0, o2.1, o2.2));
    }

    std::fs::write(format!("{out}.manifest"), &man).unwrap();
    std::fs::write(out, to_mcfunction(&g, (0, 1, 0))).unwrap();
    let _ = Dir::North;
    eprintln!("{man}blocks={}", g.len());
}
