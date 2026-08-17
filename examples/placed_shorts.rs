//! Are any two nets shorted together in a *placed* design?
//!
//! `add2` is history-dependent in game and in the block simulator: six torches
//! settle differently depending on what the circuit computed before, with no
//! torch burnout involved. A combinational netlist is a DAG and cannot be
//! bistable, so the placed circuit must contain a connection the netlist does
//! not - two nets touching, which closes a feedback loop through the gates.
//!
//! Nothing in a `Grid` records which signal a cell carries, so this floods from
//! every driver through dust and repeaters, labels what it reaches, and reports
//! any cell two different drivers can both reach.
use ohmc::world::{down, offset, up, Block, Dir, Grid, Pos};
use ohmc::{bitblast, layout, lower, parser};
use std::collections::HashMap;

/// Everything electrically joined to `start`, following dust and through
/// repeaters in their output direction.
fn flood(g: &Grid, start: Pos, label: usize, owner: &mut HashMap<Pos, usize>) -> Vec<(Pos, usize)> {
    let mut clashes = Vec::new();
    let mut stack = vec![start];
    let mut seen = std::collections::HashSet::new();
    while let Some(q) = stack.pop() {
        if !seen.insert(q) {
            continue;
        }
        let next: Vec<Pos> = match g.get(q) {
            Block::Dust { .. } => {
                let mut v = Vec::new();
                for d in Dir::ALL {
                    let n = offset(q, d);
                    // Dust joins dust orthogonally and across a one-block step.
                    v.push(n);
                    v.push(up(n));
                    v.push(down(n));
                }
                // Dust weakly powers the block it sits on, and a repeater
                // facing that block reads it. This is how most signals leave a
                // wire, and following only dust-to-dust misses every one of
                // them - so any loop closed through a repeater fed off a wire
                // is invisible.
                let sub = down(q);
                for d in Dir::ALL {
                    let r = offset(sub, d);
                    if let Block::Repeater { facing, .. } = g.get(r) {
                        if offset(r, facing) == sub {
                            v.push(r);
                        }
                    }
                }
                v
            }
            // A repeater is a one-way valve: carry on out its far side only.
            Block::Repeater { facing, .. } => vec![offset(q, facing.opposite())],
            _ => continue,
        };
        if let Some(&prev) = owner.get(&q) {
            if prev != label {
                clashes.push((q, prev));
                continue;
            }
        }
        owner.insert(q, label);
        stack.extend(next);
    }
    clashes
}

/// Torch-to-torch driver graph of the placed circuit, and any cycle in it.
///
/// A combinational netlist is a DAG, so the placed gates must be one too. If
/// they are not, the circuit can hold state - which is exactly what a
/// history-dependent adder is doing.
fn find_cycle(g: &Grid) -> Option<Vec<Pos>> {
    // Where each torch's support is, so a net can be asked which gates it feeds.
    let mut support_of: HashMap<Pos, Pos> = HashMap::new();
    for (&p, &b) in g.iter() {
        let sup = match b {
            Block::WallTorch { facing, .. } => offset(p, facing.opposite()),
            Block::Torch { .. } => down(p),
            _ => continue,
        };
        support_of.insert(p, sup);
    }
    let powers: HashMap<Pos, Pos> = support_of.iter().map(|(&t, &s)| (s, t)).collect();

    let mut edges: HashMap<Pos, Vec<Pos>> = HashMap::new();
    for (&t, _) in support_of.iter() {
        // The torch strongly powers the block above it; its output is whatever
        // dust touches that block.
        let mut seeds = Vec::new();
        let above = up(t);
        if matches!(g.get(above), Block::Dust { .. }) {
            seeds.push(above);
        }
        for d in Dir::ALL {
            let n = offset(above, d);
            match g.get(n) {
                Block::Dust { .. } => seeds.push(n),
                // A repeater reading *from* this torch's output block carries
                // the signal onward. Seeding only dust misses every net that
                // leaves a gate through a refresh repeater - which the router
                // inserts constantly - and so misses any loop closed through
                // one.
                Block::Repeater { facing, .. } if offset(n, facing) == above => seeds.push(n),
                _ => {}
            }
        }
        let mut owner = HashMap::new();
        for s in seeds {
            flood(g, s, 0, &mut owner);
        }
        // Which torch supports does that net energise?
        let mut sinks = Vec::new();
        for (&cell, _) in owner.iter() {
            for cand in [down(cell)].into_iter().chain(Dir::ALL.iter().map(|&d| offset(cell, d))) {
                if let Some(&sink) = powers.get(&cand) {
                    if sink != t {
                        sinks.push(sink);
                    }
                }
            }
        }
        // A lit torch strongly powers the block directly above it. If that
        // block is another torch's support, this is a driver edge with no dust
        // anywhere on it - invisible to a net flood, and the kind of coupling
        // that only appears once cells are packed against each other.
        if let Some(&direct) = powers.get(&above) {
            if direct != t {
                sinks.push(direct);
            }
        }
        sinks.sort();
        sinks.dedup();
        edges.insert(t, sinks);
    }

    // Depth-first cycle search.
    let mut colour: HashMap<Pos, u8> = HashMap::new();
    let mut stack: Vec<Pos> = Vec::new();
    fn dfs(
        n: Pos,
        edges: &HashMap<Pos, Vec<Pos>>,
        colour: &mut HashMap<Pos, u8>,
        stack: &mut Vec<Pos>,
    ) -> Option<Vec<Pos>> {
        colour.insert(n, 1);
        stack.push(n);
        for &m in edges.get(&n).into_iter().flatten() {
            match colour.get(&m).copied().unwrap_or(0) {
                1 => {
                    let at = stack.iter().position(|&x| x == m).unwrap();
                    return Some(stack[at..].to_vec());
                }
                0 => {
                    if let Some(c) = dfs(m, edges, colour, stack) {
                        return Some(c);
                    }
                }
                _ => {}
            }
        }
        stack.pop();
        colour.insert(n, 2);
        None
    }
    let mut keys: Vec<Pos> = edges.keys().copied().collect();
    keys.sort();
    for k in keys {
        if colour.get(&k).copied().unwrap_or(0) == 0 {
            if let Some(c) = dfs(k, &edges, &mut colour, &mut stack) {
                return Some(c);
            }
        }
    }
    None
}

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "examples/add2.ohm".into());
    let src = std::fs::read_to_string(&path).unwrap();
    let prog = parser::parse(&src).unwrap();
    let design = lower::lower_program(&prog).unwrap();
    let net = bitblast::blast_combinational(&design).unwrap();
    let lay = layout::build(&net).unwrap();
    let g = &lay.grid;

    // Every driver in the placed design: a lit-or-unlit torch drives the block
    // above it, and that block's dust is the net. Levers drive their tap.
    let mut drivers: Vec<Pos> = Vec::new();
    for (&p, &b) in g.iter() {
        match b {
            Block::WallTorch { .. } | Block::Torch { .. } => {
                // The output dust sits beside the torch, on top of the block it
                // powers. Take whichever neighbour of the powered block is dust.
                let powered = up(p);
                for d in Dir::ALL {
                    let n = offset(powered, d);
                    if matches!(g.get(n), Block::Dust { .. }) {
                        drivers.push(n);
                    }
                }
            }
            Block::Lever { .. } => {
                for d in Dir::ALL {
                    let n = offset(p, d);
                    if matches!(g.get(n), Block::Dust { .. }) {
                        drivers.push(n);
                    }
                }
            }
            _ => {}
        }
    }
    drivers.sort();
    drivers.dedup();

    let mut owner: HashMap<Pos, usize> = HashMap::new();
    let mut all_clashes: Vec<(Pos, usize, usize)> = Vec::new();
    for (i, &d) in drivers.iter().enumerate() {
        for (cell, other) in flood(g, d, i, &mut owner) {
            all_clashes.push((cell, other, i));
        }
    }

    // A NOR cell's torch goes out exactly when its support block is powered,
    // and the only thing that should ever power that support is the cell's own
    // input pad. Anything else touching it shorts the gate - and a gate whose
    // input is shorted to something downstream is a feedback loop, which is
    // precisely what a bistable combinational circuit needs to exist.
    let mut shorted = Vec::new();
    for (&p, &b) in g.iter() {
        let support = match b {
            Block::WallTorch { facing, .. } => offset(p, facing.opposite()),
            Block::Torch { .. } => down(p),
            _ => continue,
        };
        let mut srcs = Vec::new();
        if matches!(g.get(up(support)), Block::Dust { .. }) {
            srcs.push(up(support));
        }
        for d in Dir::ALL {
            let n = offset(support, d);
            match g.get(n) {
                Block::Dust { .. } => srcs.push(n),
                Block::Repeater { facing, .. } if offset(n, facing.opposite()) == support => {
                    srcs.push(n)
                }
                _ => {}
            }
        }
        if srcs.len() > 1 {
            srcs.sort();
            shorted.push((p, support, srcs));
        }
    }
    shorted.sort();
    // Self-loops first: a gate whose own output reaches its own support is the
    // original NOR-oscillator bug, and the cycle search deliberately skips them.
    {
        let mut selfies = Vec::new();
        for (&p, &b) in g.iter() {
            let sup = match b {
                Block::WallTorch { facing, .. } => offset(p, facing.opposite()),
                Block::Torch { .. } => down(p),
                _ => continue,
            };
            let above = up(p);
            let mut seeds = Vec::new();
            // A torch strongly powers the block above it only if that block
            // conducts. When it is air the torch drives nothing there, and
            // treating its neighbours as the gate's output sweeps in the gate's
            // own input pad - which invents a self-loop at every cell.
            if g.get(above).conducts() {
                for d in Dir::ALL {
                    let n = offset(above, d);
                    match g.get(n) {
                        Block::Dust { .. } => seeds.push(n),
                        Block::Repeater { facing, .. } if offset(n, facing) == above => {
                            seeds.push(n)
                        }
                        _ => {}
                    }
                }
            }
            // The output the cell library actually uses: dust beside the torch.
            for d in Dir::ALL {
                let n = offset(p, d);
                if matches!(g.get(n), Block::Dust { .. }) {
                    seeds.push(n);
                }
            }
            let mut own = HashMap::new();
            for s in seeds {
                flood(g, s, 0, &mut own);
            }
            let feeds_self = own.keys().any(|&c| {
                down(c) == sup || Dir::ALL.iter().any(|&d| offset(c, d) == sup)
            });
            if feeds_self {
                selfies.push((p, sup));
            }
        }
        println!("{} gate(s) whose own output reaches their own support:", selfies.len());
        for (t, sup) in selfies.iter().take(10) {
            println!("  torch {t:?} support {sup:?}");
        }
    }

    match find_cycle(g) {
        Some(c) => {
            println!("\nFEEDBACK LOOP through {} gate(s):", c.len());
            for t in &c {
                println!("  torch {t:?}");
            }
        }
        None => println!("\nno feedback loop among the placed gates"),
    }

    println!("\n{} torch support(s) with more than one power source:", shorted.len());
    for (t, sup, srcs) in shorted.iter().take(12) {
        println!("  torch {t:?} support {sup:?} <- {srcs:?}");
    }

    println!("\n{path}: {} drivers, {} dust/repeater cells labelled", drivers.len(), owner.len());
    println!("{} cell(s) reachable from two different drivers", all_clashes.len());
    // Dump the neighbourhood of each clash so a genuine short can be told from
    // an artefact of guessing drivers out of geometry.
    if let Ok(want) = std::env::var("OHMC_DUMP") {
        let t: Vec<i32> = want.split(',').map(|v| v.trim().parse().unwrap()).collect();
        let c = (t[0], t[1], t[2]);
        println!("\nneighbourhood of {c:?}:");
        for dy in -2..=2i32 {
            for dz in -2..=2i32 {
                for dx in -2..=2i32 {
                    let n = (c.0 + dx, c.1 + dy, c.2 + dz);
                    let b = g.get(n);
                    if !matches!(b, Block::Air) {
                        println!("  {n:?} d=({dx},{dy},{dz}) {b:?} owner={:?}", owner.get(&n));
                    }
                }
            }
        }
    }
    for (cell, a, b) in all_clashes.iter().take(20) {
        println!("  {cell:?} shared by driver {} {:?} and driver {} {:?}", a, drivers[*a], b, drivers[*b]);
    }
}
