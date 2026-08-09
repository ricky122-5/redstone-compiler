//! Where does the signal stop inside the D latch?
//!
//! Stages sit at known Z bands, so printing powered dust grouped by Z shows
//! exactly which stage the signal reaches and which one it does not.
use ohmc::redstone::Sim;
use ohmc::tech::stamp_d_latch;
use ohmc::world::{Block, Dir, Face, Grid, Material, Pos};

fn drive(g: &mut Grid, feed: Pos) -> Pos {
    let (x, y, z) = feed;
    g.force((x, y - 1, z), Block::Solid(Material::Wire));
    g.force((x, y, z), Block::Dust { power: 0 });
    g.force((x, y - 1, z - 1), Block::Solid(Material::Wire));
    g.force((x, y, z - 1), Block::Dust { power: 0 });
    let lever = (x, y, z - 2);
    g.force((x, y - 1, z - 2), Block::Solid(Material::PortIn));
    g.force(lever, Block::Lever { face: Face::Floor, facing: Dir::North, powered: false });
    lever
}

fn main() {
    let mut g = Grid::new();
    let p = stamp_d_latch(&mut g, (0, 0, 0)).unwrap();
    let (da, db, ea, eb, q, qn) = (p.d_a, p.d_b, p.en_a, p.en_b, p.q, p.q_not);
    let levers = [drive(&mut g, da), drive(&mut g, db), drive(&mut g, ea), drive(&mut g, eb)];
    println!("d_feeds {da:?} {db:?}  en_feeds {ea:?} {eb:?}  q={q:?} qn={qn:?}");

    for (d, e) in [(true, true), (false, true)] {
        let mut sim = Sim::new(&g);
        sim.set_lever(levers[0], d);
        sim.set_lever(levers[1], d);
        sim.set_lever(levers[2], e);
        sim.set_lever(levers[3], e);
        let (t, ok) = sim.run_until_stable(5000);
        let f = sim.field();
        println!("\n=== d={d} e={e}  settled={ok} after {t} ticks ===");
        println!("  q={} qn={}", f.dust_at(q), f.dust_at(qn));

        // Torches are the gates; a lit torch means that gate outputs high.
        let mut t: Vec<_> = sim
            .state
            .torch_lit
            .iter()
            .map(|(p, v)| (p.2, p.0, *v))
            .collect();
        t.sort();
        let lit: Vec<String> = t
            .iter()
            .map(|(z, x, v)| format!("z{z}x{x}={}", if *v { "1" } else { "0" }))
            .collect();
        println!("  gate torches (by stage): {}", lit.join(" "));
    }
}
