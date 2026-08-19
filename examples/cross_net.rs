//! Do two different nets touch in a placed design, by the game's rules?
//!
//! Authoritative: uses the router's own ownership map rather than guessing
//! nets from geometry. Reports every pair of cells owned by different signals
//! that Minecraft would join - orthogonal, or a one-block step where the step
//! is actually open. A wire that is adjacent to another net reads that net's
//! signal, which is how a0's net in add2 stays at 15 with its lever off.
use ohmc::world::{down, offset, up, Block, Pos};
use ohmc::{bitblast, layout, lower, parser};

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "examples/add2.ohm".into());
    let src = std::fs::read_to_string(&path).unwrap();
    let prog = parser::parse(&src).unwrap();
    let design = lower::lower_program(&prog).unwrap();
    let net = bitblast::blast_combinational(&design).unwrap();
    let lay = layout::build(&net).unwrap();
    let g = &lay.grid;

    let owner = &lay.wire_owner;
    let mut hits: Vec<(Pos, u32, Pos, u32, &str)> = Vec::new();
    for (&p, &a) in owner.iter() {
        if !matches!(g.get(p), Block::Dust { .. }) {
            continue;
        }
        for d in ohmc::world::Dir::ALL {
            let n = offset(p, d);
            for (c, kind, open) in [
                (n, "orthogonal", true),
                // Climbing: blocked if something opaque sits above the lower dust.
                (up(n), "step-up", !g.get(up(p)).is_opaque()),
                // Descending: blocked if something opaque sits above the lower dust.
                (down(n), "step-down", !g.get(n).is_opaque()),
            ] {
                if !open || !matches!(g.get(c), Block::Dust { .. }) {
                    continue;
                }
                if let Some(&b) = owner.get(&c) {
                    if b != a && p < c {
                        hits.push((p, a, c, b, kind));
                    }
                }
            }
        }
    }
    let total_dust = g.iter().filter(|(_, b)| matches!(b, Block::Dust { .. })).count();
    let owned_dust = g
        .iter()
        .filter(|(p, b)| matches!(b, Block::Dust { .. }) && owner.contains_key(p))
        .count();
    println!("dust cells: {total_dust} total, {owned_dust} with a known net");
    // Unowned dust is invisible to this audit, so say where it is.
    let mut unowned: Vec<Pos> = g
        .iter()
        .filter(|(p, b)| matches!(b, Block::Dust { .. }) && !owner.contains_key(p))
        .map(|(&p, _)| p)
        .collect();
    unowned.sort();
    for p in unowned.iter().take(8) {
        println!("  unowned dust {p:?}");
    }

    hits.sort();
    println!("{}: {} cross-net dust contact(s)", path, hits.len());
    for (p, a, c, b, kind) in hits.iter().take(25) {
        println!("  {p:?} net {a}  <{kind}>  {c:?} net {b}");
    }
}
