//! What surrounds each output lamp in a placed design, and who owns it?
//!
//! In game, a lamp lights if anything powers it: dust on top, dust pointing
//! into it, an adjacent strongly powered block, a repeater facing it. If any
//! of those cells belongs to a different net than the lamp's driver, the lamp
//! reads the wrong signal even though every gate is correct - which is exactly
//! what the add2 in-game run shows.
use ohmc::world::{down, offset, up, Block, Dir};
use ohmc::{bitblast, layout, lower, parser};

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "examples/add2.ohm".into());
    let src = std::fs::read_to_string(&path).unwrap();
    let prog = parser::parse(&src).unwrap();
    let design = lower::lower_program(&prog).unwrap();
    let net = bitblast::blast_combinational(&design).unwrap();
    let lay = layout::build(&net).unwrap();
    let g = &lay.grid;

    for (name, lamps) in &lay.output_lamps {
        for (i, &lp) in lamps.iter().enumerate() {
            println!("\n{name}[{i}] lamp at {lp:?}:");
            for c in Dir::ALL
                .iter()
                .map(|&d| offset(lp, d))
                .chain([up(lp), down(lp)])
            {
                let b = g.get(c);
                if matches!(b, Block::Air) {
                    continue;
                }
                println!("  {c:?} {b:?} owner={:?}", lay.wire_owner.get(&c));
            }
        }
    }
}
