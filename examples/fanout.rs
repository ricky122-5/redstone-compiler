//! Fan-out distribution of a netlist.
//!
//! A driver's output spine is how many places a branch can leave from, and it
//! is capped. If a net has far more consumers than spine cells, every branch
//! competes for the same few exits and the net becomes a bottleneck no amount
//! of rerouting can fix - which looks like congestion somewhere else.
use ohmc::{bitblast, lower, parser};
use std::collections::HashMap;

fn main() {
    let path = std::env::args().nth(1).unwrap_or("examples/gcd.ohm".into());
    let src = std::fs::read_to_string(&path).unwrap();
    let design = lower::lower_program(&parser::parse(&src).unwrap()).unwrap();
    let net = bitblast::blast(&design);

    let mut fanout: HashMap<u32, usize> = HashMap::new();
    for s in 0..net.sigs.len() as u32 {
        for &o in net.operands(s).iter() {
            *fanout.entry(o).or_default() += 1;
        }
    }
    for d in &net.dffs {
        *fanout.entry(d.d).or_default() += 1;
    }
    let mut v: Vec<(u32, usize)> = fanout.into_iter().collect();
    v.sort_by_key(|&(s, f)| (std::cmp::Reverse(f), s));
    println!("{path}: {} signals", net.sigs.len());
    println!("top fan-out nets:");
    for (s, f) in v.iter().take(12) {
        println!("  net {s:>4}: {f:>4} consumers   {:?}", net.src(*s));
    }
    let total: usize = v.iter().map(|&(_, f)| f).sum();
    println!("total connections {total}");
    for cap in [24usize, 48, 96] {
        let over = v.iter().filter(|&&(_, f)| f > cap).count();
        println!("  nets with fan-out > {cap}: {over}");
    }
}
