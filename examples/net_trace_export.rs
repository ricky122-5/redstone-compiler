//! Export a placed design plus a manifest of every cell on one input's net,
//! so the in-game harness can probe the wire itself by blockstate.
//!
//! Written for the add2 divergence: the new layout is correct in the simulator
//! and wrong in the game, every even input reading sum+1 - input a0 stuck high
//! whenever its lever is off. The simulator says that net is dead everywhere;
//! the game disagrees; only the game can say which cell is powered.
use ohmc::netlist::Src;
use ohmc::structure::to_mcfunction;
use ohmc::world::{Block, Pos};
use ohmc::{bitblast, layout, lower, parser};

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "examples/add2.ohm".into());
    let port: u32 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let bit: u32 = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(0);

    let src = std::fs::read_to_string(&path).unwrap();
    let prog = parser::parse(&src).unwrap();
    let design = lower::lower_program(&prog).unwrap();
    let net = bitblast::blast_combinational(&design).unwrap();
    let lay = layout::build(&net).unwrap();

    // port=999 selects a raw signal id passed in the bit argument, so output
    // nets can be traced with the same tool.
    let sig = if port == 999 {
        bit
    } else {
        (0..net.sigs.len() as u32)
            .find(|&s| matches!(*net.src(s), Src::Input { port: p, bit: b } if p == port && b == bit))
            .expect("no such input")
    };

    let mut cells: Vec<(Pos, bool)> = lay
        .wire_owner
        .iter()
        .filter(|(_, &o)| o == sig)
        .map(|(&p, _)| (p, matches!(lay.grid.get(p), Block::Repeater { .. })))
        .collect();
    cells.sort_by_key(|&((x, y, z), _)| (z, y, x));

    let lo = lay.grid.bounds().unwrap().0;
    let rel = |p: Pos| (p.0 - lo.0, p.1 - lo.1 + 1, p.2 - lo.2);
    let mut man = String::new();
    for (name, v) in &lay.input_levers {
        for (i, &l) in v.iter().enumerate() {
            let r = rel(l);
            man.push_str(&format!("LEV_{name}_{i} {} {} {}\n", r.0, r.1, r.2));
        }
    }
    for (i, (c, is_rep)) in cells.iter().enumerate() {
        let r = rel(*c);
        man.push_str(&format!("{}{i} {} {} {} # grid {c:?}\n", if *is_rep { "ER" } else { "EW" }, r.0, r.1, r.2));
    }
    // Every torch, named by the signal whose gate it belongs to. A torch's
    // output dust is one of gate_cells' `out` positions and sits beside it, so
    // nearest-out is an exact match. Probing torches by blockstate is a full
    // gate-state dump per stage, and diffing it against GateSim names the
    // first wrong gate in the *game*, not the simulator.
    for (&p, &b) in lay.grid.iter() {
        if !matches!(b, Block::WallTorch { .. } | Block::Torch { .. }) {
            continue;
        }
        let near = lay
            .gate_cells
            .iter()
            .map(|(&s, (o, _))| {
                let d = (o.0 - p.0).abs() + (o.1 - p.1).abs() + (o.2 - p.2).abs();
                (d, s)
            })
            .min();
        if let Some((d, s)) = near {
            if d <= 3 {
                let r = rel(p);
                man.push_str(&format!("TS{s} {} {} {}\n", r.0, r.1, r.2));
            }
        }
    }
    // OHMC_BOX=x,y,z,r publishes every non-air cell in a cube, so the game can
    // be asked what is actually adjacent to a stuck wire. The audits all say
    // this placement has no cross-net contact and the simulator agrees, so
    // whatever holds a0's net at 15 is a rule the model does not have - and only
    // a direct read of the neighbourhood in game can name it.
    if let Ok(spec) = std::env::var("OHMC_BOX") {
        let n: Vec<i32> = spec.split(',').map(|v| v.trim().parse().unwrap()).collect();
        let (cx, cy, cz, r) = (n[0], n[1], n[2], n[3]);
        let mut k = 0;
        for dy in -r..=r {
            for dz in -r..=r {
                for dx in -r..=r {
                    let c = (cx + dx, cy + dy, cz + dz);
                    let b = lay.grid.get(c);
                    if matches!(b, Block::Air) {
                        continue;
                    }
                    let rr = rel(c);
                    let tag = match b {
                        Block::Dust { .. } => "BW",
                        Block::Repeater { .. } => "BR",
                        Block::WallTorch { .. } | Block::Torch { .. } => "BT",
                        Block::Lever { .. } => "BL",
                        _ => "BS",
                    };
                    man.push_str(&format!(
                        "{tag}{k} {} {} {} # grid {c:?} {b:?} owner={:?}\n",
                        rr.0, rr.1, rr.2,
                        lay.wire_owner.get(&c)
                    ));
                    k += 1;
                }
            }
        }
        eprintln!("box around ({cx},{cy},{cz}) r={r}: {k} cells");
    }

    for (name, lamps) in &lay.output_lamps {
        for (i, &lp) in lamps.iter().enumerate() {
            let r = rel(lp);
            man.push_str(&format!("LAMP_{name}_{i} {} {} {}\n", r.0, r.1, r.2));
        }
    }
    std::fs::write("/tmp/nettrace.mcfunction.manifest", &man).unwrap();
    std::fs::write("/tmp/nettrace.mcfunction", to_mcfunction(&lay.grid, (0, 1, 0))).unwrap();
    eprintln!("sig {sig} (port{port} bit{bit}): {} cells", cells.len());
    // Expected torch state per mapped gate for a few whole-circuit inputs, so
    // the in-game dump can be diffed on sight.
    let gs = ohmc::netlist::GateSim::new(&net);
    for v in [0u64, 1, 2] {
        let (mut vals, mut rest) = (Vec::new(), v);
        for p in &design.inputs {
            vals.push(rest & ((1u64 << p.width) - 1));
            rest >>= p.width;
        }
        let truth = gs.eval(&vals, false);
        let mut sigs: Vec<u32> = lay.gate_cells.keys().copied().collect();
        sigs.sort();
        let line: Vec<String> = sigs
            .iter()
            .map(|&s| format!("TS{s}={}", if truth[s as usize] { "LIT" } else { "dark" }))
            .collect();
        eprintln!("expected v={v}: {}", line.join(" "));
    }
}
