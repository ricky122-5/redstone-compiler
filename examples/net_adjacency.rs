//! Do two different nets end up electrically joined inside the D latch?
//!
//! Keepout cannot be checked from the grid alone - dust is dust, and nothing in
//! a `Grid` records which signal a cell belongs to. `Router::owners` does, so
//! this walks every placed dust cell and reports any neighbour, orthogonal or
//! diagonal across a Y step, that belongs to a different net.
//!
//! Asked because the boundary-port latch freezes in game in a way that looks
//! like the enable and something else being the same wire.
use ohmc::route::Router;
use ohmc::tech::stamp_d_latch;
use ohmc::world::{offset, Block, Dir, Grid, Pos};

fn main() {
    let mut g = Grid::new();
    let p = stamp_d_latch(&mut g, (0, 0, 0)).unwrap();
    // A router rebuilt over the finished macro knows only PREPLACED, so the per
    // net map has to come from the macro itself. `stamp_d_latch` does not return
    // its router, so re-derive ownership by flood filling each port instead:
    // whatever a port reaches through dust is that port's net.
    let names: [(&str, Pos); 3] = [("D", p.d), ("EN", p.en), ("CLR", p.clr)];
    let mut owner: std::collections::HashMap<Pos, &str> = std::collections::HashMap::new();

    for (name, start) in names {
        let mut stack = vec![start];
        while let Some(q) = stack.pop() {
            // A repeater is part of the net too, and the signal continues out
            // its far side. Stopping at one hides most of the wire.
            let next: Vec<Pos> = match g.get(q) {
                Block::Dust { .. } => {
                    let mut v = Vec::new();
                    for d in Dir::ALL {
                        let n = offset(q, d);
                        v.push(n);
                        v.push((n.0, n.1 + 1, n.2));
                        v.push((n.0, n.1 - 1, n.2));
                    }
                    v
                }
                Block::Repeater { facing, .. } => vec![offset(q, facing.opposite())],
                _ => continue,
            };
            if let Some(prev) = owner.insert(q, name) {
                if prev != name {
                    println!("!! {q:?} ({:?}) reachable from both {prev} and {name}", g.get(q));
                }
                continue;
            }
            stack.extend(next);
        }
    }

    // Report how far each port's net actually reaches, and whether it arrives.
    for (name, _) in names {
        let cells = owner.values().filter(|v| **v == name).count();
        println!("{name:<4} net covers {cells} dust cells");
    }
    for (label, feed) in [("not_d.D", p.d_a), ("r_gate.D", p.d_b), ("en_s", p.en_a), ("en_r", p.en_b)] {
        println!("  {label:<10} {feed:?} owned by {:?}", owner.get(&feed));
    }
    let _ = Router::from_grid(&g);
}
