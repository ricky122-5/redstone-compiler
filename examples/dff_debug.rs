//! Which components ring in the flip-flop?
use ohmc::redstone::Sim;
use ohmc::tech::stamp_dff;
use ohmc::world::{Block, Dir, Face, Grid, Material, Pos};
use std::collections::HashMap;

fn drive(g: &mut Grid, feed: Pos) -> Pos {
    let (x, y, z) = feed;
    g.force((x, y - 1, z), Block::Solid(Material::Wire));
    g.force((x, y, z), Block::Dust { power: 0 });
    g.force((x, y - 1, z - 1), Block::Solid(Material::Wire));
    g.force((x, y, z - 1), Block::Dust { power: 0 });
    let lever = (x, y, z - 2);
    g.force((x, y - 1, z - 2), Block::Solid(Material::PortIn));
    g.force(lever, Block::Lever { face: Face::Floor, facing: Dir::North, powered: false });
    lever
}

fn main() {
    let mut g = Grid::new();
    let p = stamp_dff(&mut g, (0, 0, 0)).unwrap();
    let dl: Vec<Pos> = p.d_feeds.iter().map(|&f| drive(&mut g, f)).collect();
    let cl: Vec<Pos> = p.clk_feeds.iter().map(|&f| drive(&mut g, f)).collect();
    println!("q={:?} master_q={:?} not_clk={:?}", p.q, p.master_q, p.not_clk);

    let mut sim = Sim::new(&g);
    for &l in &dl { sim.set_lever(l, true); }
    for &l in &cl { sim.set_lever(l, false); }

    let mut prev = sim.state.torch_lit.clone();
    let mut churn: HashMap<Pos, u32> = HashMap::new();
    for _ in 0..400 {
        sim.step();
        for (p, v) in &sim.state.torch_lit {
            if prev.get(p) != Some(v) { *churn.entry(*p).or_default() += 1; }
        }
        prev = sim.state.torch_lit.clone();
    }
    let mut v: Vec<_> = churn.into_iter().filter(|&(_, c)| c > 2).collect();
    v.sort_by_key(|&(_, c)| std::cmp::Reverse(c));
    println!("torches ringing: {}", v.len());
    for (p, c) in v.iter().take(10) { println!("  {p:?} toggled {c}x"); }
}
