//! Does attaching levers to the flip-flop destroy any of it?
//!
//! The exporters place input levers with `force`, which overwrites whatever is
//! already there instead of erroring. Everything else in this project uses
//! `set`, which refuses to overwrite - so this is the one place a wire can be
//! silently deleted after it was routed.
use ohmc::tech::{stamp_d_latch, stamp_dff};
use ohmc::world::{offset, Block, Dir, Face, Grid, Material, Pos};

/// Repeaters frozen by another repeater driving their side.
///
/// Audited on the *exported* grid, levers and all - not the bare macro. A
/// component that follows its input and then freezes at whatever it last
/// output is exactly a locked repeater, and that is what the enable path does
/// in game.
fn locked(g: &Grid) -> Vec<(Pos, Pos)> {
    let mut out = Vec::new();
    for (&p, &b) in g.iter() {
        let Block::Repeater { facing, .. } = b else { continue };
        let sides: [Dir; 2] = match facing {
            Dir::North | Dir::South => [Dir::East, Dir::West],
            Dir::East | Dir::West => [Dir::North, Dir::South],
        };
        for s in sides {
            let n = offset(p, s);
            if let Block::Repeater { facing: f2, .. } = g.get(n) {
                if offset(n, f2.opposite()) == p {
                    out.push((p, n));
                }
            }
        }
    }
    out.sort();
    out
}

fn attach(g: &mut Grid, feed: Pos) {
    let (x, y, z) = feed;
    g.force((x, y - 1, z), Block::Solid(Material::Wire));
    g.force((x, y, z), Block::Dust { power: 0 });
    g.force((x, y - 1, z - 1), Block::Solid(Material::Wire));
    g.force((x, y, z - 1), Block::Dust { power: 0 });
    g.force((x, y - 1, z - 2), Block::Solid(Material::PortIn));
    g.force((x, y, z - 2), Block::Lever { face: Face::Floor, facing: Dir::North, powered: false });
}

fn main() {
    let mut g = Grid::new();
    let p = stamp_dff(&mut g, (0, 0, 0)).unwrap();
    let before: Vec<(Pos, Block)> = g.iter().map(|(&q, &b)| (q, b)).collect();

    // Exactly what the exporters do for each input port.
    let mut clobbered = Vec::new();
    let mut ports: Vec<Pos> = Vec::new();
    ports.extend(p.d_feeds.iter().copied());
    ports.extend(p.clk_feeds.iter().copied());
    ports.extend(p.clk_n_feeds.iter().copied());
    ports.extend(p.clr_feeds.iter().copied());

    for feed in ports {
        let (x, y, z) = feed;
        for (q, nb) in [
            ((x, y - 1, z), Block::Solid(Material::Wire)),
            ((x, y, z), Block::Dust { power: 0 }),
            ((x, y - 1, z - 1), Block::Solid(Material::Wire)),
            ((x, y, z - 1), Block::Dust { power: 0 }),
            ((x, y - 1, z - 2), Block::Solid(Material::PortIn)),
            ((x, y, z - 2), Block::Lever { face: Face::Floor, facing: Dir::North, powered: false }),
        ] {
            let old = g.get(q);
            if old != Block::Air && old != nb {
                clobbered.push((feed, q, old, nb));
            }
            g.force(q, nb);
        }
    }

    // The D latch as `dlatch_export` builds it: macro, levers, probe lamps.
    {
        let mut lg = Grid::new();
        let lp = stamp_d_latch(&mut lg, (0, 0, 0)).unwrap();
        let bare = locked(&lg).len();
        for f in [lp.d, lp.en, lp.clr] {
            attach(&mut lg, f);
        }
        for q in [lp.q, lp.not_e_r, lp.r_out] {
            lg.force((q.0, q.1 - 1, q.2), Block::Lamp { lit: false });
        }
        let full = locked(&lg);
        println!("d_latch: {bare} locked repeaters bare, {} once exported", full.len());
        for (a, b) in full.iter().take(10) {
            println!("    {a:?} locked by {b:?}");
        }
    }

    println!("\n{} cells overwritten by lever attachment:", clobbered.len());
    for (feed, q, old, new) in clobbered.iter().take(20) {
        println!("  port {feed:?} -> {q:?}: {old:?} replaced by {new:?}");
    }
    let _ = before;
}
