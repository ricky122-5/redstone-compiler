//! Export a bare D latch for in-game testing.
//!
//! The simulator says this latch resets correctly and the flip-flop built from
//! it does not, so the disagreement is worth pinning on the smallest circuit
//! that shows it rather than on the whole flip-flop.
use ohmc::structure::to_mcfunction;
use ohmc::tech::stamp_d_latch;
use ohmc::world::{Block, Dir, Face, Grid, Material, Pos};

fn lever(g: &mut Grid, feed: Pos) -> Pos {
    let (x, y, z) = feed;
    g.force((x, y - 1, z), Block::Solid(Material::Wire));
    g.force((x, y, z), Block::Dust { power: 0 });
    g.force((x, y - 1, z - 1), Block::Solid(Material::Wire));
    g.force((x, y, z - 1), Block::Dust { power: 0 });
    let l = (x, y, z - 2);
    g.force((x, y - 1, z - 2), Block::Solid(Material::PortIn));
    g.force(l, Block::Lever { face: Face::Floor, facing: Dir::North, powered: false });
    l
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "/tmp/dlatch.mcfunction".into());
    let mut g = Grid::new();
    let p = stamp_d_latch(&mut g, (0, 0, 0)).unwrap();
    // With OHMC_LOAD_Q set, hang a long routed wire off Q, the way the
    // flip-flop's master drives its slave.
    //
    // This is the one difference between the master - which will not reset in
    // game - and this same macro standalone, which does. The latch is otherwise
    // identical and driven identically, so if loading Q breaks the reset, that
    // is the whole of the flip-flop bug in a circuit a third the size.
    if std::env::var("OHMC_LOAD_Q").is_ok() {
        use ohmc::route::Router;
        use ohmc::world::Material;
        let sink = (p.q.0 + 30, p.q.1, p.q.2 + 40);
        g.set((sink.0, sink.1 - 1, sink.2), Block::Solid(Material::Gate)).unwrap();
        g.set(sink, Block::Dust { power: 0 }).unwrap();
        let mut r = Router::from_grid(&g);
        r.claim(p.q, 90);
        r.claim(sink, 90);
        let bounds = ((-60, -40, -60), (200, 60, 200));
        r.route(&mut g, 90, &[p.q], sink, bounds, Material::Gate, 0)
            .expect("load wire");
        eprintln!("[Q loaded with a routed wire to {sink:?}]");
    }
    // Boundary ports, which is what a caller drives. The old export drove the
    // internal feeds directly, so the in-game pass it produced said nothing
    // about whether the ports themselves work.
    for f in [p.d] { lever(&mut g, f); }
    for f in [p.en] { lever(&mut g, f); }
    // Probe the enable path end to end: the pad the lever drives, and the two
    // internal gate feeds it fans out to. The inverter stops following the
    // enable after one rise, and this says whether the signal dies at the pad
    // or somewhere along the leg.
    // With OHMC_MIN_PROBES set, publish only Q.
    //
    // A probe replaces the block *under* a node's output dust with a lamp,
    // which is a circuit modification, not a passive read. Simulating with them
    // says they are harmless, but our lamp model is exactly the sort of thing
    // that could be wrong here - and the gate that freezes in game, NOTER, is
    // one of the probed nodes. The only way to know is to take them away.
    let min_probes = std::env::var("OHMC_MIN_PROBES").is_ok();
    let all_probes = [
        ("Q", p.q),
        ("NOTER", p.not_e_r),
        ("ROUT", p.r_out),
        ("ENPAD", p.en),
        ("ENA", p.en_a),
        ("ENB", p.en_b),
        ("DPAD", p.d),
    ];
    let probes: Vec<(&str, Pos)> = if min_probes {
        vec![("Q", p.q)]
    } else {
        all_probes.to_vec()
    };
    for (_, q) in probes.iter().copied() { g.force((q.0, q.1 - 1, q.2), Block::Lamp { lit: false }); }

    // With OHMC_TRACE_EN set, publish every cell of the enable net so the
    // harness can probe the wire itself by blockstate - no lamps, no circuit
    // modification. Every probe so far has been at a named node; the pair of
    // adjacent cells where the signal actually dies has never been observed.
    let mut en_cells: Vec<(Pos, bool)> = Vec::new(); // (cell, is_repeater)
    if std::env::var("OHMC_TRACE_EN").is_ok() {
        use ohmc::world::offset;
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![p.en];
        while let Some(q) = stack.pop() {
            if !seen.insert(q) {
                continue;
            }
            match g.get(q) {
                Block::Dust { .. } => {
                    en_cells.push((q, false));
                    for d in ohmc::world::Dir::ALL {
                        let n = offset(q, d);
                        stack.push(n);
                        stack.push((n.0, n.1 + 1, n.2));
                        stack.push((n.0, n.1 - 1, n.2));
                    }
                }
                Block::Repeater { facing, .. } => {
                    en_cells.push((q, true));
                    stack.push(offset(q, facing.opposite()));
                }
                _ => {}
            }
        }
        en_cells.sort_by_key(|&((x, y, z), _)| (z, x, y));
    }

    let lo = g.bounds().unwrap().0;
    let rel = |q: Pos| (q.0 - lo.0, q.1 - lo.1 + 1, q.2 - lo.2);
    let mut man = String::new();
    // `lever` puts the switch two blocks north of the feed, at the *same* Y.
    // Getting that offset wrong silently drives nothing, which reads exactly
    // like a dead circuit.
    for f in [p.d] { let r = rel((f.0, f.1, f.2 - 2)); man.push_str(&format!("D {} {} {}\n", r.0, r.1, r.2)); }
    for f in [p.en] { let r = rel((f.0, f.1, f.2 - 2)); man.push_str(&format!("CLK {} {} {}\n", r.0, r.1, r.2)); }
    for (n, q) in probes.iter().copied() { let r = rel((q.0, q.1 - 1, q.2)); man.push_str(&format!("{n} {} {} {}\n", r.0, r.1, r.2)); }
    // With OHMC_TRACE_STATE set, publish every torch and repeater in the
    // grid. Torches are the gates and the loop; repeaters are the delay
    // elements. Probing them by blockstate is a full register dump per stage
    // with zero circuit modification - the instrument the lamp probes never
    // were.
    if std::env::var("OHMC_TRACE_STATE").is_ok() {
        let mut torches = Vec::new();
        let mut reps = Vec::new();
        for (&q, &b) in g.iter() {
            match b {
                Block::WallTorch { .. } | Block::Torch { .. } => torches.push(q),
                Block::Repeater { .. } => reps.push(q),
                _ => {}
            }
        }
        torches.sort();
        reps.sort();
        for (i, t) in torches.iter().enumerate() {
            let r = rel(*t);
            man.push_str(&format!("T{i} {} {} {}\n", r.0, r.1, r.2));
            eprintln!("T{i} = grid {t:?}");
        }
        for (i, t) in reps.iter().enumerate() {
            let r = rel(*t);
            man.push_str(&format!("R{i} {} {} {}\n", r.0, r.1, r.2));
        }
    }
    for (i, (c, is_rep)) in en_cells.iter().enumerate() {
        let r = rel(*c);
        let kind = if *is_rep { "ER" } else { "EW" };
        man.push_str(&format!("{kind}{i} {} {} {}\n", r.0, r.1, r.2));
    }
    std::fs::write(format!("{out}.manifest"), &man).unwrap();
    std::fs::write(&out, to_mcfunction(&g, (0, 1, 0))).unwrap();
    eprintln!("{man}blocks={}", g.len());
}
