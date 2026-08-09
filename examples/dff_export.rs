//! Export the flip-flop as a `.mcfunction` so the real game can be asked whether
//! it works.
//!
//! The simulator says it oscillates. But the simulator deliberately does not
//! model torch burnout, which is precisely the mechanism real redstone uses to
//! damp a circulating pulse - so a ring that never dies in simulation may settle
//! in the game. That is a difference worth measuring rather than assuming.
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
    let (q, dfs, cfs) = (p.q, p.d_feeds.clone(), p.clk_feeds.clone());
    let dl: Vec<Pos> = dfs.iter().map(|&f| lever(&mut g, f)).collect();
    let cl: Vec<Pos> = cfs.iter().map(|&f| lever(&mut g, f)).collect();
    // Lamps under the nodes we need to read. The simulator rings on this
    // circuit, so the game is the only instrument that can see inside it.
    let mut probes = vec![
        ("Q", q),
        ("MQ", p.master_q),
        ("NCLK", p.not_clk),
        ("SNOTER", p.slave_not_e_r),
        ("SROUT", p.slave_r_out),
    ];
    for (i, &t) in p.not_clk_taps.iter().enumerate() {
        probes.push((if i == 0 { "NCTAP0" } else { "NCTAP3" }, t));
    }
    for (_, probe) in &probes {
        g.force((probe.0, probe.1 - 1, probe.2), Block::Lamp { lit: false });
    }

    let lo = g.bounds().unwrap().0;
    let rel = |p: Pos| (p.0 - lo.0, p.1 - lo.1 + 1, p.2 - lo.2);
    let mut man = String::new();
    for l in &dl { let r = rel(*l); man.push_str(&format!("D {} {} {}\n", r.0, r.1, r.2)); }
    for l in &cl { let r = rel(*l); man.push_str(&format!("CLK {} {} {}\n", r.0, r.1, r.2)); }
    for (name, probe) in &probes {
        let r = rel((probe.0, probe.1 - 1, probe.2));
        man.push_str(&format!("{name} {} {} {}\n", r.0, r.1, r.2));
    }

    std::fs::write(format!("{out}.manifest"), &man).unwrap();
    std::fs::write(&out, to_mcfunction(&g, (0, 1, 0))).unwrap();
    eprintln!("{man}blocks={}", g.len());
}
