//! How many gates land on each logic level?
//!
//! Levelisation is ASAP - every gate sits at the earliest level its operands
//! allow. If that leaves a few levels enormous and the rest nearly empty, the
//! wide levels are what set the array's width, and the spread of barycenters
//! follows from the shape of the schedule rather than from the netlist being
//! irreducibly spread out.
use ohmc::netlist::Src;
use ohmc::{bitblast, lower, parser};
use std::collections::HashMap;

fn main() {
    let path = std::env::args().nth(1).unwrap_or("examples/gcd.ohm".into());
    let src = std::fs::read_to_string(&path).unwrap();
    let design = lower::lower_program(&parser::parse(&src).unwrap()).unwrap();
    let net = bitblast::blast(&design);

    // Same ASAP rule the placer uses.
    let mut level = vec![0i32; net.sigs.len()];
    // The placer's own roots, and only the gates it actually places - counting
    // every NOR in the netlist includes ones no root reaches, which never get
    // placed and would inflate the low levels with gates that do not exist in
    // the build.
    let roots = net.roots();
    let order = net.topo_order(&roots);
    let placed: std::collections::HashSet<u32> = order
        .iter()
        .copied()
        .filter(|&s| matches!(net.src(s), Src::Nor(_)))
        .collect();
    for s in net.topo_order(&roots) {
        if matches!(net.src(s), Src::Nor(_)) {
            let d = net.operands(s).iter().map(|&o| level[o as usize]).max().unwrap_or(0);
            level[s as usize] = d + 1;
        }
    }
    let mut per: HashMap<i32, usize> = HashMap::new();
    for &s in &placed {
        *per.entry(level[s as usize]).or_default() += 1;
    }
    let mut ls: Vec<(i32, usize)> = per.into_iter().collect();
    ls.sort();
    let total: usize = ls.iter().map(|&(_, n)| n).sum();
    println!("{path}: {total} gates over {} levels", ls.len());
    let widest = ls.iter().map(|&(_, n)| n).max().unwrap_or(0);
    for (l, n) in &ls {
        println!("  level {l:>3}: {n:>4} {}", "#".repeat((*n * 60 / widest.max(1)).max(1)));
    }
    let mean = total as f64 / ls.len() as f64;
    println!("  widest {widest}, mean {mean:.1}, ratio {:.1}x", widest as f64 / mean);
}
