//! What lands near the master after the master is built?
//!
//! The flip-flop's master will not take a 0 in game, while the same macro
//! standalone does - and the master is stamped first into an empty grid, so its
//! own blocks are identical in both. Loading Q, probe lamps and lever
//! attachment are all ruled out by measurement. So the disturbance has to come
//! from something placed *afterwards*, and that is a question the grid can
//! answer directly.
use ohmc::route::Router;
use ohmc::tech::{stamp_d_latch, stamp_dff};
use ohmc::world::{Block, Grid, Material, Pos};
use std::collections::HashMap;

fn snapshot(g: &Grid) -> HashMap<Pos, Block> {
    g.iter().map(|(&p, &b)| (p, b)).collect()
}

fn main() {
    // The master exactly as the flip-flop builds it, and nothing else.
    let mut solo = Grid::new();
    let m = stamp_d_latch(&mut solo, (0, 0, 0)).unwrap();
    let before = snapshot(&solo);
    let hi_z = solo.bounds().unwrap().1 .2;
    println!("master alone: {} blocks, reaching z={}", before.len(), hi_z);

    // Now the whole flip-flop.
    let mut g = Grid::new();
    let p = stamp_dff(&mut g, (0, 0, 0)).unwrap();
    let after = snapshot(&g);
    println!("flip-flop:    {} blocks", after.len());

    // Anything inside the master's own footprint that differs, or is new.
    let mut changed = Vec::new();
    let mut added = Vec::new();
    for (&q, &b) in &after {
        if q.2 > hi_z {
            continue; // beyond the master's extent
        }
        match before.get(&q) {
            Some(&old) if old != b => changed.push((q, old, b)),
            Some(_) => {}
            None => added.push((q, b)),
        }
    }
    changed.sort_by_key(|&(q, _, _)| q);
    added.sort_by_key(|&(q, _)| q);

    println!("\n{} block(s) of the master CHANGED:", changed.len());
    for (q, old, new) in changed.iter().take(20) {
        println!("  {q:?}: {old:?} -> {new:?}");
    }
    println!("\n{} block(s) ADDED inside the master's footprint:", added.len());
    for (q, b) in added.iter().take(20) {
        println!("  {q:?}: {b:?}");
    }

    // Which added cells could join the master's wiring? Orthogonal neighbours
    // are the obvious case, but dust also connects diagonally across a Y step,
    // so a wire one level down and one cell over is electrically the same net.
    let mut touching = Vec::new();
    for (q, b) in &added {
        if !matches!(b, Block::Dust { .. } | Block::Repeater { .. }) {
            continue;
        }
        for dx in -1..=1i32 {
            for dy in -1..=1i32 {
                for dz in -1..=1i32 {
                    let manh = dx.abs() + dy.abs() + dz.abs();
                    // orthogonal, or a one-step diagonal that changes level
                    let ortho = manh == 1;
                    let diag_step = dy != 0 && dx.abs() + dz.abs() == 1 && manh == 2;
                    if !ortho && !diag_step {
                        continue;
                    }
                    let n = (q.0 + dx, q.1 + dy, q.2 + dz);
                    if matches!(before.get(&n), Some(Block::Dust { .. })) {
                        touching.push((*q, *b, n, if ortho { "orthogonal" } else { "DIAGONAL" }));
                    }
                }
            }
        }
    }
    println!("\n{} added wire cell(s) touching the master's dust:", touching.len());
    for (q, b, n, how) in touching.iter().take(20) {
        println!("  {how:<10} {q:?} {b:?} <-> master dust {n:?}");
    }
    let _ = (m, p, Material::Gate, Router::from_grid(&g));
}
