//! What does a shorted cell actually look like?
//!
//! `placed_shorts` says the clock and reset trunks reach the same cells out in
//! the register bank. That is a wiring fault, and fixing it needs the geometry:
//! which blocks sit there, which net the router thinks owns each, and which of
//! them the keepout rule was supposed to have kept apart.
use ohmc::world::Pos;
use ohmc::{bitblast, layout, lower, parser};

fn main() {
    let path = std::env::args().nth(1).unwrap_or("examples/tick.ohm".into());
    let at: Vec<i32> = std::env::var("OHMC_AT")
        .unwrap_or_else(|_| "27,7,-63".into())
        .split(',')
        .map(|v| v.trim().parse().unwrap())
        .collect();
    let c: Pos = (at[0], at[1], at[2]);
    let r: i32 = std::env::var("OHMC_R").ok().and_then(|v| v.parse().ok()).unwrap_or(3);

    let src = std::fs::read_to_string(&path).unwrap();
    let prog = parser::parse(&src).unwrap();
    let design = lower::lower_program(&prog).unwrap();
    let net = bitblast::blast(&design);
    let lay = layout::build(&net).expect("must place");
    let base = net.sigs.len() as u32;
    let name = |id: u32| -> String {
        match id {
            i if i == u32::MAX => "preplaced".into(),
            i if i < base => format!("net{i}"),
            i if i == base => "CLK".into(),
            i if i == base + 1 => "CLKN".into(),
            i if i == base + 2 => "RST".into(),
            i => format!("?{i}"),
        }
    };

    println!("around {c:?}:");
    for dy in (-r..=r).rev() {
        for dz in -r..=r {
            let mut row = String::new();
            for dx in -r..=r {
                let p = (c.0 + dx, c.1 + dy, c.2 + dz);
                let ch = match lay.grid.get(p) {
                    ohmc::world::Block::Dust { .. } => 'd',
                    ohmc::world::Block::Repeater { .. } => 'R',
                    ohmc::world::Block::Solid(_) => '#',
                    ohmc::world::Block::WallTorch { .. } | ohmc::world::Block::Torch { .. } => 'T',
                    ohmc::world::Block::Lever { .. } => 'L',
                    ohmc::world::Block::Air => '.',
                    _ => '?',
                };
                row.push(ch);
            }
            println!("  y={:<4} z={:<5} {row}", c.1 + dy, c.2 + dz);
        }
        println!();
    }
    println!("owners:");
    for dy in -r..=r {
        for dz in -r..=r {
            for dx in -r..=r {
                let p = (c.0 + dx, c.1 + dy, c.2 + dz);
                if let Some(&o) = lay.wire_owner.get(&p) {
                    println!("  {p:?} {:?}  owner {}", lay.grid.get(p), name(o));
                }
            }
        }
    }
}
