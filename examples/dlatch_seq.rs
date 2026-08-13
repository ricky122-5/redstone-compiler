//! Can the D latch overwrite a stored 1 with a 0?
//!
//! `dlatch_debug` builds a fresh `Sim` for each case, so every case starts from
//! power-up and the latch never has to *change* a stored value. That is the one
//! thing a latch exists to do, and it is untested there. The same fresh-per-case
//! flaw already produced a false pass once in `sim_sweep`.
//!
//! This drives one persistent latch through a sequence, which is what the
//! hardware actually sees.
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
    // One lever per logical input now: the macro fans out internally.
    let d = [drive(&mut g, p.d)];
    let e = [drive(&mut g, p.en)];

    let mut sim = Sim::new(&g);
    // Store a 1, then try to overwrite it with a 0 while still enabled.
    let stages: [(&str, bool, bool); 8] = [
        ("init d=0 e=0", false, false),
        ("open  d=1 e=1", true, true),
        ("hold  d=1 e=0", true, false),
        ("open  d=0 e=1", false, true), // <- the one that matters
        ("hold  d=0 e=0", false, false),
        ("open  d=1 e=1", true, true),
        // Q is 1 here. Drop D with the enable low: Q must not move.
        ("shut  d=0 e=0", false, false),
        ("shut  d=1 e=0", true, false),
    ];

    // Arriving strength on the two links into the latch. Whether a link
    // *arrives* is not the question - the question is with how much margin. A
    // link that lands on 1 here lands on 0 in game if our decay model is even
    // slightly generous, and it would be dead in one direction only.
    println!(
        "{:<15} {:>3} {:>3} {:>4} {:>4} {:>7} {:>6} {:>6} {:>6}",
        "stage", "D", "E", "Q", "Qn", "notD_in", "rGate_D", "enS_in", "enR_in"
    );
    for (name, dv, ev) in stages {
        for &l in &d {
            sim.set_lever(l, dv);
        }
        for &l in &e {
            sim.set_lever(l, ev);
        }
        let (_, stable) = sim.run_until_stable(5000);
        let f = sim.field();
        println!(
            "{:<15} {:>3} {:>3} {:>4} {:>4} {:>7} {:>6} {:>6} {:>6} {}{}",
            name,
            dv as u8,
            ev as u8,
            f.dust_at(p.q),
            f.dust_at(p.q_not),
            f.dust_at(p.d_a),
            f.dust_at(p.d_b),
            f.dust_at(p.en_a),
            f.dust_at(p.en_b),
            if stable { "" } else { "UNSTABLE " },
            if sim.burned_out().is_empty() { "" } else { "BURNT" }
        );
    }
}
