//! Export the flip-flop as a `.mcfunction` so the real game can be asked whether
//! it works.
//!
//! The circuit is correct in simulation; the question this answers is whether it
//! is correct once *placed*. A `.mcfunction` places blocks one `setblock` at a
//! time and the game re-evaluates every torch and repeater as its neighbours
//! appear, so the latch state chosen at stamp time does not survive placement.
//! That is what the RST feeds are for: drive the built circuit into a known
//! state rather than expecting it to be born in one.
use ohmc::structure::to_mcfunction;
use ohmc::tech::stamp_dff;
use ohmc::world::{Block, Dir, Face, Grid, Material, Pos};

fn lever(g: &mut Grid, feed: Pos) -> Pos {
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

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "/tmp/dff.mcfunction".into());
    let mut g = Grid::new();
    let p = stamp_dff(&mut g, (0, 0, 0)).unwrap();
    let (q, dfs, cfs, rfs) = (p.q, p.d_feeds.clone(), p.clk_feeds.clone(), p.clr_feeds.clone());
    let nfs = p.clk_n_feeds.clone();
    let dl: Vec<Pos> = dfs.iter().map(|&f| lever(&mut g, f)).collect();
    let cl: Vec<Pos> = cfs.iter().map(|&f| lever(&mut g, f)).collect();
    let rl: Vec<Pos> = rfs.iter().map(|&f| lever(&mut g, f)).collect();
    let nl: Vec<Pos> = nfs.iter().map(|&f| lever(&mut g, f)).collect();
    // Lamps under the nodes we need to read, so the game can be asked the same
    // questions the simulator answers.
    let probes = vec![
        ("Q", q),
        ("MQ", p.master_q),
        ("SNOTER", p.slave_not_e_r),
        ("SROUT", p.slave_r_out),
        ("MNOTER", p.master_not_e_r),
        ("MROUT", p.master_r_out),
    ];
    for (_, probe) in &probes {
        g.force((probe.0, probe.1 - 1, probe.2), Block::Lamp { lit: false });
    }

    let lo = g.bounds().unwrap().0;
    let rel = |p: Pos| (p.0 - lo.0, p.1 - lo.1 + 1, p.2 - lo.2);
    let mut man = String::new();
    for l in &dl { let r = rel(*l); man.push_str(&format!("D {} {} {}\n", r.0, r.1, r.2)); }
    for l in &cl { let r = rel(*l); man.push_str(&format!("CLK {} {} {}\n", r.0, r.1, r.2)); }
    for l in &rl { let r = rel(*l); man.push_str(&format!("RST {} {} {}\n", r.0, r.1, r.2)); }
    for l in &nl { let r = rel(*l); man.push_str(&format!("CLKN {} {} {}\n", r.0, r.1, r.2)); }
    for (name, probe) in &probes {
        let r = rel((probe.0, probe.1 - 1, probe.2));
        man.push_str(&format!("{name} {} {} {}\n", r.0, r.1, r.2));
    }

    std::fs::write(format!("{out}.manifest"), &man).unwrap();
    std::fs::write(&out, to_mcfunction(&g, (0, 1, 0))).unwrap();
    eprintln!("{man}blocks={}", g.len());
}
