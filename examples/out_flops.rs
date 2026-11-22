//! Which flip-flop drives each output bit, by index into the bank - the same
//! order `tick_sim` prints its Q vector in. Lets a lamp reading be checked
//! against the register it should mirror without placing anything.
use ohmc::{bitblast, lower, parser};

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "examples/gcd.ohm".into());
    let src = std::fs::read_to_string(&path).unwrap();
    let design = lower::lower_program(&parser::parse(&src).unwrap()).unwrap();
    let mut net = bitblast::blast(&design);
    if let Some(t) = std::env::var("OHMC_CLONE").ok().and_then(|v| v.parse::<usize>().ok()) {
        net.clone_high_fanout(t);
    }
    if let Some(t) = std::env::var("OHMC_BUFFER").ok().and_then(|v| v.parse::<usize>().ok()) {
        net.buffer_high_fanout(t);
    }
    // `OHMC_WHO=n`: what net n is - a flop's Q, or which flops' D read it.
    if let Some(n) = std::env::var("OHMC_WHO").ok().and_then(|v| v.parse::<u32>().ok()) {
        println!("net {n} = {:?}", net.sigs[n as usize]);
        for f in 0..net.dffs.len() as u32 {
            if net.dff_q(f) == Some(n) { println!("  is Q of flop {f} `{}`", net.dffs[f as usize].name); }
            if net.dffs[f as usize].d == n { println!("  is D of flop {f} `{}`", net.dffs[f as usize].name); }
        }
    }
    for (name, bits) in &net.outputs {
        for (i, &b) in bits.iter().enumerate() {
            let flop = (0..net.dffs.len() as u32).find(|&f| net.dff_q(f) == Some(b));
            println!("{name}[{i}] net {b} <- flop {flop:?}");
        }
    }
}
