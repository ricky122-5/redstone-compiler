//! Dump which dust the *simulator* thinks is powered, in the same coordinate
//! frame the `.mcfunction` uses, so it can be diffed against the real game.
//!
//! Usage: cargo run --release --example sim_dump -- <file.ohm> <input-bits>

use ohmc::redstone::Sim;
use ohmc::world::Block;
use ohmc::{bitblast, layout, lower, parser};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().cloned().unwrap_or_else(|| "examples/andgate.ohm".into());
    let value: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);

    let src = std::fs::read_to_string(&path).expect("read source");
    let design = lower::lower_program(&parser::parse(&src).unwrap()).unwrap();
    let comb = bitblast::blast_combinational(&design).unwrap();
    let lay = layout::build(&comb).unwrap();

    // Same transform the mcfunction emitter applies.
    let lo = lay.grid.bounds().map(|(lo, _)| lo).unwrap();
    let rel = |p: (i32, i32, i32)| (p.0 - lo.0, p.1 - lo.1 + 1, p.2 - lo.2);

    let mut sim = Sim::new(&lay.grid);
    let mut bit = 0;
    for (_, levers) in &lay.input_levers {
        for &l in levers {
            sim.set_lever(l, (value >> bit) & 1 == 1);
            bit += 1;
        }
    }
    let (_, stable) = sim.run_until_stable(600);
    eprintln!("stable={stable} inputs={value:b}");

    let f = sim.field();
    let mut powered: Vec<(i32, i32, i32)> = lay
        .grid
        .iter()
        .filter(|(_, b)| matches!(b, Block::Dust { .. }))
        .filter(|(p, _)| f.dust_at(**p) > 0)
        .map(|(p, _)| rel(*p))
        .collect();
    powered.sort_by_key(|p| (p.1, p.2, p.0));
    for p in powered {
        println!("SIMPWR {} {} {}", p.0, p.1, p.2);
    }
}
