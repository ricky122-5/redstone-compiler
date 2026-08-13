//! Does the placer emit dust with no support?
//!
//! The D latch's dead leg was dust resting on a repeater - a wire that does not
//! exist in Minecraft but that the simulator models as ordinary dust. Nothing in
//! the test suite could see it. The placer routes far more wire than any macro,
//! so the same check belongs on its output before gcd is ever attempted.
use ohmc::layout::build;
use ohmc::world::{down, Block, Grid};
use ohmc::{bitblast, lower, parser};

fn unsupported(g: &Grid) -> Vec<(ohmc::world::Pos, Block)> {
    g.iter()
        .filter(|(_, b)| matches!(b, Block::Dust { .. }))
        .map(|(&p, _)| (p, g.get(down(p))))
        // Support means opaque, not conducting: dust sits on a lamp fine.
        .filter(|(_, u): &(_, Block)| !u.is_opaque())
        .collect()
}

fn main() {
    let mut bad = 0;
    for src in ["examples/invert.ohm", "examples/andgate.ohm", "examples/add2.ohm", "examples/add.ohm", "examples/alu.ohm"] {
        let text = match std::fs::read_to_string(src) {
            Ok(t) => t,
            Err(_) => continue,
        };
        // The same combinational extraction `main` uses to place a design.
        let net = match parser::parse(&text)
            .and_then(|p| lower::lower_program(&p))
            .and_then(|d| bitblast::blast_combinational(&d))
        {
            Ok(n) => n,
            Err(e) => {
                println!("{src:<24} compile failed: {e}");
                continue;
            }
        };
        match build(&net) {
            Ok(layout) => {
                let u = unsupported(&layout.grid);
                bad += u.len();
                println!("{src:<24} {:>7} blocks, {} unsupported dust", layout.grid.len(), u.len());
                for (p, under) in u.iter().take(5) {
                    println!("      {p:?} sits on {under:?}");
                }
            }
            Err(e) => println!("{src:<24} place failed: {e}"),
        }
    }
    println!("\n{bad} unsupported dust cell(s) across the placed designs");
}
