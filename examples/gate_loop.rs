//! Which gates does each placed gate actually drive, and is there a cycle?
//!
//! `tick` rings with every lever held still, which a levelised gate array
//! cannot do: the netlist is a DAG, so a free-running oscillator means the
//! *placement* contains an edge the netlist does not. This builds the real
//! drive graph from the blocks - every gate's output net flooded to whatever
//! gate inputs it reaches - and compares it against the netlist's own edges.
//! Any edge in the placed graph that the netlist does not have is the fault.
use ohmc::netlist::Src;
use ohmc::world::{down, offset, up, Block, Dir, Grid, Pos};
use ohmc::{bitblast, layout, lower, parser};
use std::collections::{HashMap, HashSet};

/// Everything electrically joined to `start`, following dust and out of
/// repeaters in their output direction.
fn flood(g: &Grid, start: Pos) -> HashSet<Pos> {
    let mut seen = HashSet::new();
    let mut stack = vec![start];
    while let Some(q) = stack.pop() {
        if !seen.insert(q) {
            continue;
        }
        match g.get(q) {
            Block::Dust { .. } => {
                for d in Dir::ALL {
                    let n = offset(q, d);
                    for c in [n, up(n), down(n)] {
                        if matches!(g.get(c), Block::Dust { .. }) {
                            stack.push(c);
                        }
                    }
                    // Dust weakly powers the block under it; a repeater facing
                    // that block reads the wire.
                    if let Block::Repeater { facing, .. } = g.get(n) {
                        if offset(n, facing) == q {
                            stack.push(n);
                        }
                    }
                }
                let sub = down(q);
                for d in Dir::ALL {
                    let r = offset(sub, d);
                    if let Block::Repeater { facing, .. } = g.get(r) {
                        if offset(r, facing) == sub {
                            stack.push(r);
                        }
                    }
                }
            }
            Block::Repeater { facing, .. } => stack.push(offset(q, facing.opposite())),
            _ => {}
        }
    }
    seen
}

fn main() {
    let path = std::env::args().nth(1).unwrap_or("examples/tick.ohm".into());
    let src = std::fs::read_to_string(&path).unwrap();
    let prog = parser::parse(&src).unwrap();
    let design = lower::lower_program(&prog).unwrap();
    let net = bitblast::blast(&design);
    let lay = layout::build(&net).expect("must place");
    let g = &lay.grid;

    // Every gate's feed cells, so a flood can be asked which gates it lands on.
    let mut feed_owner: HashMap<Pos, u32> = HashMap::new();
    for (&sig, (_, feeds)) in &lay.gate_cells {
        for &f in feeds {
            feed_owner.insert(f, sig);
            // The pad the feed's repeater drives, and the pad's whole run, are
            // the gate's input just as much as the feed is.
            feed_owner.insert((f.0, f.1, f.2 + 1), sig);
            feed_owner.insert((f.0, f.1, f.2 + 2), sig);
        }
    }

    let mut extra = Vec::new();
    let mut sigs: Vec<u32> = lay.gate_cells.keys().copied().collect();
    sigs.sort();
    for s in sigs {
        let (out, _) = &lay.gate_cells[&s];
        let want: HashSet<u32> = net
            .sigs
            .iter()
            .enumerate()
            .filter(|(_, _)| true)
            .filter_map(|(i, _)| match net.src(i as u32) {
                Src::Nor(ops) if ops.contains(&s) => Some(i as u32),
                _ => None,
            })
            .collect();
        let reached: HashSet<u32> =
            flood(g, *out).iter().filter_map(|p| feed_owner.get(p).copied()).collect();
        for r in reached.difference(&want) {
            if *r != s {
                extra.push((s, *r));
            }
        }
    }
    println!("{} placed drive edges the netlist does not have:", extra.len());
    for (a, b) in extra.iter().take(30) {
        println!("  net {a} {:?}  ->  net {b} {:?}", net.src(*a), net.src(*b));
    }
}
