//! Which components are toggling in the oscillating RS latch?
//!
//! Guessing at a feedback failure has a poor track record in this project. This
//! steps the simulator one tick at a time and reports every component whose
//! state changes, so the oscillating loop names itself.
use ohmc::redstone::Sim;
use ohmc::tech::stamp_rs_latch;
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
    let (sf, rf, q, qn) = stamp_rs_latch(&mut g, (0, 0, 0)).unwrap();
    let _s = drive(&mut g, sf);
    let _r = drive(&mut g, rf);
    println!("set_feed={sf:?} reset_feed={rf:?} q={q:?} qn={qn:?}");

    let mut sim = Sim::new(&g);
    let mut prev_t = sim.state.torch_lit.clone();
    let mut prev_r = sim.state.repeater_powered.clone();
    let mut churn: HashMap<Pos, u32> = HashMap::new();

    for tick in 0..60 {
        sim.step();
        let mut changed: Vec<String> = Vec::new();
        for (p, v) in &sim.state.torch_lit {
            if prev_t.get(p) != Some(v) {
                changed.push(format!("torch{p:?}={}", if *v { "ON" } else { "off" }));
                *churn.entry(*p).or_default() += 1;
            }
        }
        for (p, v) in &sim.state.repeater_powered {
            if prev_r.get(p) != Some(v) {
                changed.push(format!("rep{p:?}={}", if *v { "ON" } else { "off" }));
                *churn.entry(*p).or_default() += 1;
            }
        }
        if !changed.is_empty() && tick > 20 {
            changed.sort();
            println!("t{tick:<3} {}", changed.join("  "));
        }
        prev_t = sim.state.torch_lit.clone();
        prev_r = sim.state.repeater_powered.clone();
    }

    println!("\n--- components toggling most (the loop) ---");
    let mut v: Vec<_> = churn.into_iter().collect();
    v.sort_by_key(|&(_, c)| std::cmp::Reverse(c));
    for (p, c) in v.iter().take(8) {
        println!("  {p:?} toggled {c} times  {:?}", g.get(*p));
    }
}
