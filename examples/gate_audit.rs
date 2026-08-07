//! Where do the gates actually go? Measures the combinational netlist so the
//! "there must be 2x of fat in here" hunch can be checked rather than assumed.
use ohmc::{bitblast, lower, netlist::Src, parser};

fn audit(name: &str, src: &str) {
    let d = lower::lower_program(&parser::parse(src).unwrap()).unwrap();
    let n = bitblast::blast_combinational(&d).unwrap();
    let mut fanin = [0usize; 8];
    for s in &n.sigs {
        if let Src::Nor(v) = s {
            fanin[v.len().min(7)] += 1;
        }
    }
    println!(
        "{:<10} gates={:<5} depth={:<4} fanin1={} fanin2={} fanin3+={}",
        name,
        n.gate_count(),
        n.logic_depth(),
        fanin[1],
        fanin[2],
        fanin[3..].iter().sum::<usize>()
    );
}

fn main() {
    audit("not1", "input u1 a; output u1 q; proc main(){ q = !a; }");
    audit("and1", "input u1 a; input u1 b; output u1 q; proc main(){ q = a & b; }");
    audit("xor1", "input u1 a; input u1 b; output u1 q; proc main(){ q = a ^ b; }");
    audit("add1", "input u1 a; input u1 b; output u1 q; proc main(){ q = a + b; }");
    audit("add4", "input u4 a; input u4 b; output u4 q; proc main(){ q = a + b; }");
    audit("add8", "input u8 a; input u8 b; output u8 q; proc main(){ q = a + b; }");
    audit("cmp8", "input u8 a; input u8 b; output u1 q; proc main(){ q = a < b; }");
}
