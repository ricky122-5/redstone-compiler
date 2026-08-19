//! Locked repeaters and unsupported dust in a *placed* design.
use ohmc::world::{down, offset, Block, Dir};
use ohmc::{bitblast, layout, lower, parser};

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "examples/add2.ohm".into());
    let src = std::fs::read_to_string(&path).unwrap();
    let prog = parser::parse(&src).unwrap();
    let design = lower::lower_program(&prog).unwrap();
    let net = bitblast::blast_combinational(&design).unwrap();
    let lay = layout::build(&net).unwrap();
    let g = &lay.grid;

    let mut locked = 0;
    for (&p, &b) in g.iter() {
        let Block::Repeater { facing, .. } = b else { continue };
        let sides = match facing {
            Dir::North | Dir::South => [Dir::East, Dir::West],
            Dir::East | Dir::West => [Dir::North, Dir::South],
        };
        for s in sides {
            let n = offset(p, s);
            if let Block::Repeater { facing: f2, .. } = g.get(n) {
                if offset(n, f2.opposite()) == p {
                    locked += 1;
                    println!("LOCKED: repeater {p:?} ({facing:?}) locked by {n:?} ({f2:?})");
                }
            }
        }
    }
    let mut unsup = 0;
    for (&p, &b) in g.iter() {
        if matches!(b, Block::Dust { .. }) && !g.get(down(p)).is_opaque() {
            unsup += 1;
            println!("UNSUPPORTED dust {p:?} on {:?}", g.get(down(p)));
        }
    }
    // Dust directly on top of a repeater? And torches whose above-block hosts foreign dust.
    let mut on_rep = 0;
    for (&p, &b) in g.iter() {
        if matches!(b, Block::Dust { .. }) && matches!(g.get(down(p)), Block::Repeater { .. }) {
            on_rep += 1;
        }
    }
    println!("{locked} locked, {unsup} unsupported, {on_rep} dust-on-repeater");
}
