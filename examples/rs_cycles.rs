//! An RS latch driven over routed wires, flipped repeatedly.
//!
//! The bisection ladder has a working rung - one pad fanning out to two NOR
//! cells over routed legs, tracking a lever through three cycles - and a broken
//! one, the D latch. The difference is the latch: its density, or its feedback
//! loop. This adds only the feedback.
use ohmc::route::Router;
use ohmc::structure::to_mcfunction;
use ohmc::tech::stamp_rs_latch;
use ohmc::world::{Block, Dir, Face, Grid, Material, Pos};

fn port(g: &mut Grid, r: &mut Router, net: u32, feed: Pos, away: i32) -> Pos {
    let pad = (feed.0 - 12, feed.1, feed.2 - away);
    g.set((pad.0, pad.1 - 1, pad.2), Block::Solid(Material::Gate)).unwrap();
    g.set(pad, Block::Dust { power: 0 }).unwrap();
    r.claim(pad, net);
    r.claim(feed, net);
    r.reserve(feed, net);
    r.reserve((feed.0, feed.1 - 1, feed.2), net);
    pad
}

fn lever_on(g: &mut Grid, pad: Pos) -> Pos {
    let (x, y, z) = pad;
    g.force((x, y - 1, z - 1), Block::Solid(Material::Wire));
    g.force((x, y, z - 1), Block::Dust { power: 0 });
    g.force((x, y - 1, z - 2), Block::Solid(Material::PortIn));
    let l = (x, y, z - 2);
    g.force(l, Block::Lever { face: Face::Floor, facing: Dir::North, powered: false });
    l
}

fn main() {
    let out = "/tmp/rscycles.mcfunction";
    let mut g = Grid::new();
    let (sf, rf, _clr, q, _qn) = stamp_rs_latch(&mut g, (0, 0, 0)).unwrap();

    let mut router = Router::from_grid(&g);
    let s_pad = port(&mut g, &mut router, 1, sf, 10);
    let r_pad = port(&mut g, &mut router, 2, rf, 16);
    let bounds = ((-80, -40, -80), (120, 40, 120));
    router.route(&mut g, 1, &[s_pad], sf, bounds, Material::Gate, 4).expect("S route");
    router.route(&mut g, 2, &[r_pad], rf, bounds, Material::Gate, 4).expect("R route");

    let s_lev = lever_on(&mut g, s_pad);
    let r_lev = lever_on(&mut g, r_pad);
    g.force((q.0, q.1 - 1, q.2), Block::Lamp { lit: false });

    let lo = g.bounds().unwrap().0;
    let rel = |p: Pos| (p.0 - lo.0, p.1 - lo.1 + 1, p.2 - lo.2);
    let mut man = String::new();
    let a = rel(s_lev);
    let b = rel(r_lev);
    man.push_str(&format!("SET {} {} {}\n", a.0, a.1, a.2));
    man.push_str(&format!("RST {} {} {}\n", b.0, b.1, b.2));
    let lq = rel((q.0, q.1 - 1, q.2));
    man.push_str(&format!("Q {} {} {}\n", lq.0, lq.1, lq.2));
    std::fs::write(format!("{out}.manifest"), &man).unwrap();
    std::fs::write(out, to_mcfunction(&g, (0, 1, 0))).unwrap();
    eprintln!("{man}blocks={}", g.len());
}
