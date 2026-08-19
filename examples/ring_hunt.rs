//! Find the cycle that lets a net hold itself powered in game.
//!
//! The a0 input net of the placed add2 latches: on once, it stays on with its
//! lever off - in Minecraft, not in the simulator. A net that holds itself
//! needs a directed cycle through at least one repeater. This walks the net's
//! own cells (from wire_owner), builds the directed drive graph twice - once
//! with the simulator's step-capping rule, once with every diagonal open - and
//! prints any cycle the permissive graph has that the strict one does not,
//! plus the capping cell that separates them.
use ohmc::netlist::Src;
use ohmc::world::{down, offset, up, Block, Dir, Pos};
use ohmc::{bitblast, layout, lower, parser};
use std::collections::{HashMap, HashSet};

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "examples/add2.ohm".into());
    let src = std::fs::read_to_string(&path).unwrap();
    let prog = parser::parse(&src).unwrap();
    let design = lower::lower_program(&prog).unwrap();
    let net = bitblast::blast_combinational(&design).unwrap();
    let lay = layout::build(&net).unwrap();
    let g = &lay.grid;

    let sig = (0..net.sigs.len() as u32)
        .find(|&s| matches!(*net.src(s), Src::Input { port: 0, bit: 0 }))
        .unwrap();
    let cells: HashSet<Pos> =
        lay.wire_owner.iter().filter(|(_, &o)| o == sig).map(|(&p, _)| p).collect();
    println!("net {sig}: {} cells", cells.len());

    // Directed edges among the net's cells. `strict` applies the simulator's
    // capping rule to diagonal steps; permissive allows any one-block step.
    let edges = |strict: bool| -> HashMap<Pos, Vec<Pos>> {
        let mut e: HashMap<Pos, Vec<Pos>> = HashMap::new();
        for &p in &cells {
            match g.get(p) {
                Block::Dust { .. } => {
                    for d in Dir::ALL {
                        let n = offset(p, d);
                        // flat
                        if cells.contains(&n) && matches!(g.get(n), Block::Dust { .. }) {
                            e.entry(p).or_default().push(n);
                        }
                        // climb: nothing opaque above p
                        let un = up(n);
                        if cells.contains(&un)
                            && matches!(g.get(un), Block::Dust { .. })
                            && (!strict || !g.get(up(p)).is_opaque())
                        {
                            e.entry(p).or_default().push(un);
                        }
                        // descend: nothing opaque above the lower dust
                        let dn = down(n);
                        if cells.contains(&dn)
                            && matches!(g.get(dn), Block::Dust { .. })
                            && (!strict || !g.get(n).is_opaque())
                        {
                            e.entry(p).or_default().push(dn);
                        }
                        // repeater fed from this dust (facing this cell)
                        if let Block::Repeater { facing, .. } = g.get(n) {
                            if cells.contains(&n) && offset(n, facing) == p {
                                e.entry(p).or_default().push(n);
                            }
                        }
                        // repeater reading the block under this dust
                        let sub = down(p);
                        if let Block::Repeater { facing, .. } = g.get(offset(sub, d)) {
                            let r = offset(sub, d);
                            if cells.contains(&r) && offset(r, facing) == sub {
                                e.entry(p).or_default().push(r);
                            }
                        }
                    }
                }
                Block::Repeater { facing, .. } => {
                    let out = offset(p, facing.opposite());
                    // drives dust directly, or the block ahead which energises
                    // every dust beside it
                    if cells.contains(&out) {
                        e.entry(p).or_default().push(out);
                    }
                    if g.get(out).is_opaque() {
                        for d in Dir::ALL {
                            let n = offset(out, d);
                            if cells.contains(&n) && matches!(g.get(n), Block::Dust { .. }) {
                                e.entry(p).or_default().push(n);
                            }
                        }
                        let above = up(out);
                        if cells.contains(&above) {
                            e.entry(p).or_default().push(above);
                        }
                    }
                }
                _ => {}
            }
        }
        e
    };

    fn find_cycle(e: &HashMap<Pos, Vec<Pos>>) -> Option<Vec<Pos>> {
        let mut colour: HashMap<Pos, u8> = HashMap::new();
        let mut stack = Vec::new();
        fn dfs(
            n: Pos,
            e: &HashMap<Pos, Vec<Pos>>,
            c: &mut HashMap<Pos, u8>,
            s: &mut Vec<Pos>,
        ) -> Option<Vec<Pos>> {
            c.insert(n, 1);
            s.push(n);
            for &m in e.get(&n).into_iter().flatten() {
                match c.get(&m).copied().unwrap_or(0) {
                    1 => {
                        let at = s.iter().position(|&x| x == m).unwrap();
                        return Some(s[at..].to_vec());
                    }
                    0 => {
                        if let Some(r) = dfs(m, e, c, s) {
                            return Some(r);
                        }
                    }
                    _ => {}
                }
            }
            s.pop();
            c.insert(n, 2);
            None
        }
        let mut ks: Vec<Pos> = e.keys().copied().collect();
        ks.sort();
        for k in ks {
            if colour.get(&k).copied().unwrap_or(0) == 0 {
                if let Some(r) = dfs(k, e, &mut colour, &mut stack) {
                    return Some(r);
                }
            }
        }
        None
    }

    // A dust-only "cycle" is just bidirectional adjacency and means nothing:
    // dust cannot sustain itself. A real ring runs through a repeater - its
    // output feeding, through any amount of dust, back to its own input. So
    // ask exactly that, per repeater, under each rule set.
    let reaches = |e: &HashMap<Pos, Vec<Pos>>, from: Pos, to: Pos| -> bool {
        let mut seen = HashSet::new();
        let mut stack = vec![from];
        while let Some(q) = stack.pop() {
            if q == to {
                return true;
            }
            if seen.insert(q) {
                stack.extend(e.get(&q).into_iter().flatten().copied());
            }
        }
        false
    };
    for strictness in [true, false] {
        let e = edges(strictness);
        for &p in &cells {
            if let Block::Repeater { facing, .. } = g.get(p) {
                let inp = offset(p, facing);
                let out = offset(p, facing.opposite());
                if cells.contains(&inp) && reaches(&e, out, inp) {
                    println!(
                        "RING through repeater {p:?} ({}): output {out:?} reaches input {inp:?}",
                        if strictness { "sim rules" } else { "game-permissive" }
                    );
                }
            }
        }
    }

    let strict = find_cycle(&edges(true));
    let loose = find_cycle(&edges(false));
    println!("cycle under simulator rules: {:?}", strict.as_ref().map(|c| c.len()));
    match loose {
        Some(c) => {
            println!("cycle under permissive rules, {} cells:", c.len());
            for p in &c {
                println!(
                    "  {p:?} {:?} above={:?}",
                    g.get(*p),
                    g.get(up(*p))
                );
            }
        }
        None => println!("no cycle even permissively - the ring is not dust-geometric"),
    }
}
