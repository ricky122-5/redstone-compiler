//! Does attaching levers to the flip-flop destroy any of it?
//!
//! The exporters place input levers with `force`, which overwrites whatever is
//! already there instead of erroring. Everything else in this project uses
//! `set`, which refuses to overwrite - so this is the one place a wire can be
//! silently deleted after it was routed.
use ohmc::tech::stamp_dff;
use ohmc::world::{Block, Dir, Face, Grid, Material, Pos};

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

    println!("{} cells overwritten by lever attachment:", clobbered.len());
    for (feed, q, old, new) in clobbered.iter().take(20) {
        println!("  port {feed:?} -> {q:?}: {old:?} replaced by {new:?}");
    }
    let _ = before;
}
