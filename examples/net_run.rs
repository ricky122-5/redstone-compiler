//! Run a design's gate netlist before placement - the netlist the placer is
//! handed, OHMC_CLONE/OHMC_BUFFER applied - and print every output each cycle
//! beside the golden model. Separates "the logic is wrong" from "the build is".
//!
//!   cargo run --example net_run -- examples/gcd.ohm a=48,b=18
use ohmc::{bitblast, lower, machine, netlist::GateSim, parser};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "examples/gcd.ohm".into());
    let spec = args.next().unwrap_or_default();
    let src = std::fs::read_to_string(&path).unwrap();
    let design = lower::lower_program(&parser::parse(&src).unwrap()).unwrap();
    let mut net = bitblast::blast(&design);
    if let Some(t) = std::env::var("OHMC_CLONE").ok().and_then(|v| v.parse::<usize>().ok()) {
        net.clone_high_fanout(t);
    }
    if let Some(t) = std::env::var("OHMC_BUFFER").ok().and_then(|v| v.parse::<usize>().ok()) {
        net.buffer_high_fanout(t);
    }
    let pairs: Vec<(String, u64)> = spec
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap();
            (k.to_string(), v.parse().unwrap())
        })
        .collect();
    let refs: Vec<(&str, u64)> = pairs.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    let (m, cycles) = machine::run_design(&design, &refs, 1_000_000).unwrap();
    for (name, _) in &design.outputs {
        println!("golden: {name} = {} after {cycles} cycle(s)", m.output(&design, name).unwrap());
    }
    let mut vals = vec![0u64; design.inputs.len()];
    for (k, v) in &pairs {
        vals[design.inputs.iter().position(|p| &p.name == k).unwrap()] = *v;
    }
    let mut sim = GateSim::new(&net);
    sim.step(&vals, true);
    for c in 0..200 {
        let outs: Vec<String> = net
            .outputs
            .iter()
            .map(|(n, _)| format!("{n}={}", sim.read_output(n, &vals, false)))
            .collect();
        let done = sim.done(&vals, false);
        let q: String = sim.q.iter().map(|&b| if b { '1' } else { '0' }).collect();
        println!("cycle {c:3} done={} {} Q={q}", done as u8, outs.join(" "));
        if let Ok(fs) = std::env::var("OHMC_FLOP_D") {
            let val = sim.eval(&vals, false);
            for f in fs.split(',').filter_map(|v| v.parse::<usize>().ok()) {
                let d = net.dffs[f].d;
                let ins = match &net.sigs[d as usize] {
                    ohmc::netlist::Src::Nor(i) => format!("{:?}", i.iter().map(|&x| (x, val[x as usize] as u8)).collect::<Vec<_>>()),
                    o => format!("{o:?}"),
                };
                println!("    flop {f} `{}` D=net {d} = {} <- {ins}", net.dffs[f].name, val[d as usize] as u8);
            }
        }
        if done {
            break;
        }
        sim.step(&vals, false);
    }
}
