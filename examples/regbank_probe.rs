//! Can two flip-flops sit side by side and hold different values?
//!
//! A register bank is the next thing the placer needs, and this is its core
//! risk: the flip-flop macro is ~1660 blocks with long routed wires, and two of
//! them tiled must neither overlap nor couple. Measuring the footprint and the
//! isolation now is much cheaper than discovering it inside a 42-register
//! floorplan.
use ohmc::redstone::Sim;
use ohmc::tech::stamp_dff;
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

fn extents(g: &Grid) -> (i32, i32, i32) {
    let (mut mx, mut my, mut mz) = (0, 0, 0);
    for (p, _) in g.iter() {
        mx = mx.max(p.0);
        my = my.max(p.1);
        mz = mz.max(p.2);
    }
    (mx, my, mz)
}

fn main() {
    // One flip-flop on its own, to measure the footprint the bank must tile.
    let mut one = Grid::new();
    stamp_dff(&mut one, (0, 0, 0)).unwrap();
    let (ex, ey, ez) = extents(&one);
    println!("one flip-flop: {}x{}x{}  {} blocks", ex + 1, ey + 1, ez + 1, one.iter().count());

    // Tile a second one clear of the first in X and check nothing overlaps.
    let pitch = ex + 8;
    let mut g = Grid::new();
    let a = stamp_dff(&mut g, (0, 0, 0)).unwrap();
    let before = g.iter().count();
    let b = stamp_dff(&mut g, (pitch, 0, 0)).unwrap();
    let after = g.iter().count();
    // Comparing against a standalone stamp does not detect overlap: the router
    // takes a different path at a different base, so the second flip-flop is not
    // the same block count. What matters is that the second one added blocks
    // roughly equal to its own size, i.e. it did not land on top of the first.
    println!(
        "pitch {pitch} in X -> {after} blocks total, second added {}",
        after - before
    );

    // Independent D lines, shared clock: the register-bank wiring pattern.
    let da: Vec<Pos> = a.d_feeds.iter().map(|&f| drive(&mut g, f)).collect();
    let db: Vec<Pos> = b.d_feeds.iter().map(|&f| drive(&mut g, f)).collect();
    let clk: Vec<Pos> = a
        .clk_feeds
        .iter()
        .chain(b.clk_feeds.iter())
        .map(|&f| drive(&mut g, f))
        .collect();
    // Both clock phases are inputs now. This example predated that and drove
    // only the master's phase, so the slave never opened and every register
    // read zero - which looks exactly like a broken bank.
    let clk_n: Vec<Pos> = a
        .clk_n_feeds
        .iter()
        .chain(b.clk_n_feeds.iter())
        .map(|&f| drive(&mut g, f))
        .collect();
    // And reset, since a placed circuit's state is whatever placement left.
    let rst: Vec<Pos> = a
        .clr_feeds
        .iter()
        .chain(b.clr_feeds.iter())
        .map(|&f| drive(&mut g, f))
        .collect();

    let mut sim = Sim::new(&g);
    // Pulse reset first: placement leaves the latches wherever it leaves them.
    for &l in &rst {
        sim.set_lever(l, true);
    }
    sim.run_until_stable(20000);
    for &l in &rst {
        sim.set_lever(l, false);
    }
    sim.run_until_stable(20000);
    let apply = |sim: &mut Sim, va: bool, vb: bool, c: bool| {
        for &l in &da {
            sim.set_lever(l, va);
        }
        for &l in &db {
            sim.set_lever(l, vb);
        }
        for &l in &clk {
            sim.set_lever(l, c);
        }
        for &l in &clk_n {
            sim.set_lever(l, !c);
        }
        sim.run_until_stable(20000).1
    };

    println!("\n{:<22} {:>4} {:>4}", "stage", "Qa", "Qb");
    // Clock in 0,0 to reach a known state, then the two opposite patterns.
    for (name, va, vb) in
        [("clear   a=0 b=0", false, false), ("load    a=1 b=0", true, false), ("swap    a=0 b=1", false, true)]
    {
        let s1 = apply(&mut sim, va, vb, true);
        let s2 = apply(&mut sim, va, vb, false);
        let f = sim.field();
        println!(
            "{:<22} {:>4} {:>4} {}",
            name,
            f.dust_at(a.q),
            f.dust_at(b.q),
            if s1 && s2 { "" } else { "UNSTABLE" }
        );
    }
}
