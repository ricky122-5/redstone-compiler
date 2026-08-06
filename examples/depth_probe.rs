//! Evidence for the `maj3` comment in `netlist.rs`.
//!
//! Compares the OR-of-ANDs carry against the "obviously better" two-level NOR
//! form in an isolated ripple-carry adder. They come out identical in gate
//! count and one level apart in depth overall - the carry chain is already two
//! levels per bit either way, because `or_all` emits a single-input NOR that
//! the next bit's `not()` peephole removes for free.
//!
//! Run with: cargo run --release --example depth_probe
use ohmc::netlist::{Netlist, Sig};

fn adder(w: u32, new_maj: bool) -> (usize, u32) {
    let mut n = Netlist::new();
    let a: Vec<Sig> = (0..w).map(|i| n.input(0, i)).collect();
    let b: Vec<Sig> = (0..w).map(|i| n.input(1, i)).collect();
    let mut carry = n.zero();
    let mut sum = vec![];
    for i in 0..w as usize {
        let s = n.xor3(a[i], b[i], carry);
        carry = if new_maj {
            n.maj3(a[i], b[i], carry)
        } else {
            // the previous formulation: OR of ANDs
            let ab = n.and(a[i], b[i]);
            let ac = n.and(a[i], carry);
            let bc = n.and(b[i], carry);
            n.or_all(&[ab, ac, bc])
        };
        sum.push(s);
    }
    sum.push(carry);
    n.outputs.push(("s".into(), sum));
    (n.gate_count(), n.logic_depth())
}

fn main() {
    println!("{:<6} {:>18} {:>18}", "width", "old (OR-of-ANDs)", "new (2-level NOR)");
    for w in [1u32, 2, 4, 8, 16] {
        let (og, od) = adder(w, false);
        let (ng, nd) = adder(w, true);
        println!(
            "u{:<5} {:>9} g d={:<4} {:>9} g d={:<4}",
            w, og, od, ng, nd
        );
    }
}
