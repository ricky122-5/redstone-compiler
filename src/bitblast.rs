//! Word-level IR -> bit-level NOR netlist.
//!
//! Arithmetic becomes real structural hardware: ripple-carry adders, an
//! array multiplier, and logarithmic barrel shifters. Comparison reuses the
//! adder (`a < b` is the complement of the carry-out of `a + !b + 1`), which is
//! both cheaper and shares gates with any nearby subtraction thanks to
//! hash-consing.
//!
//! The FSM is one-hot encoded: one flip-flop per basic block, exactly one set.
//! That makes next-state logic a flat OR of AND terms and keeps the fan-in of
//! every register's update mux small.

use crate::ir::{Design, Node, NodeId, Term};
use crate::netlist::{Netlist, Sig};
use std::collections::HashMap;

pub struct Blaster<'d> {
    d: &'d Design,
    pub net: Netlist,
    /// Bits of each register, LSB first.
    reg_q: Vec<Vec<Sig>>,
    reg_dff: Vec<Vec<u32>>,
    /// One-hot state bits, indexed by block.
    state_q: Vec<Sig>,
    state_dff: Vec<u32>,
    /// Latches once the machine reaches a `Halt` block, freezing all state.
    /// Without this the halt block would self-loop and re-run its assignments
    /// every cycle, which diverges from the golden model for anything that
    /// reads its own register (`q = q + 1`).
    halted_q: Sig,
    halted_dff: u32,
    cache: HashMap<NodeId, Vec<Sig>>,
}

pub fn blast(d: &Design) -> Netlist {
    Blaster::new(d).run()
}

/// Blast a straight-line program into a *purely combinational* netlist.
///
/// A single basic block ending in `Halt` computes each output as a pure
/// function of the inputs, so the surrounding FSM (state vector, halt latch,
/// register hold logic) carries no information and can be dropped. What remains
/// is a circuit with no state at all, which is what the placer can physically
/// build. Registers read as their reset value, since nothing can have written
/// them yet.
pub fn blast_combinational(d: &Design) -> Result<Netlist, String> {
    if d.blocks.len() != 1 || !matches!(d.blocks[0].term, Term::Halt) {
        return Err(format!(
            "program has {} basic block(s) and needs a control FSM; \
             only straight-line programs are combinational",
            d.blocks.len()
        ));
    }
    let mut b = Blaster::new(d);
    // Every register reads as its reset value: no cycle has elapsed.
    for r in &d.regs {
        let zeros = vec![b.net.zero(); r.width as usize];
        b.reg_q.push(zeros);
    }
    for &(ref name, r) in &d.outputs {
        let bits = match d.blocks[0].assigns.iter().find(|&&(rr, _)| rr == r) {
            Some(&(_, v)) => b.node_bits(v),
            None => vec![b.net.zero(); d.regs[r as usize].width as usize],
        };
        b.net.outputs.push((name.clone(), bits));
    }
    b.net.done = b.net.one();
    Ok(b.net)
}

impl<'d> Blaster<'d> {
    fn new(d: &'d Design) -> Blaster<'d> {
        // Carry the source-level input port names into the netlist here rather
        // than at either entry point, so the sequential and combinational paths
        // cannot drift apart - which they did: `blast` had them and
        // `blast_combinational` did not, so `add2.ohm` still reported `port0`.
        let mut net = Netlist::new();
        net.input_names = d.inputs.iter().map(|p| p.name.clone()).collect();
        Blaster {
            d,
            net,
            reg_q: Vec::new(),
            reg_dff: Vec::new(),
            state_q: Vec::new(),
            state_dff: Vec::new(),
            halted_q: 0,
            halted_dff: 0,
            cache: HashMap::new(),
        }
    }

    fn run(mut self) -> Netlist {
        // Allocate all state first, so combinational logic can read any Q.
        for r in &self.d.regs {
            let mut qs = Vec::with_capacity(r.width as usize);
            let mut ds = Vec::with_capacity(r.width as usize);
            for b in 0..r.width {
                let (idx, q) = self.net.add_dff(&format!("{}[{}]", r.name, b));
                qs.push(q);
                ds.push(idx);
            }
            self.reg_q.push(qs);
            self.reg_dff.push(ds);
        }
        for (i, b) in self.d.blocks.iter().enumerate() {
            let (idx, q) = self.net.add_dff(&format!("state.{}#{}", b.label, i));
            self.state_q.push(q);
            self.state_dff.push(idx);
        }
        let (hidx, hq) = self.net.add_dff("halted");
        self.halted_dff = hidx;
        self.halted_q = hq;

        self.build_halt_flag();
        self.build_next_state();
        self.build_register_updates();

        for &(ref name, r) in &self.d.outputs {
            let bits = self.reg_q[r as usize].clone();
            self.net.outputs.push((name.clone(), bits));
        }
        self.net.done = self.halted_q;

        self.net
    }

    /// `halted` latches on the first cycle spent in a `Halt` block and is only
    /// cleared by reset.
    fn build_halt_flag(&mut self) {
        let in_halt: Vec<Sig> = self
            .d
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| matches!(b.term, Term::Halt))
            .map(|(i, _)| self.state_q[i])
            .collect();
        let reached = self.net.or_all(&in_halt);
        let sticky = self.net.or(self.halted_q, reached);
        let reset = self.net.reset();
        let nreset = self.net.not(reset);
        let d = self.net.and(nreset, sticky);
        self.net.set_dff_d(self.halted_dff, d);
    }

    // --- sequential structure --------------------------------------------

    fn build_next_state(&mut self) {
        let n = self.d.blocks.len();
        // Contributions into each block, gathered from every predecessor.
        let mut into: Vec<Vec<Sig>> = vec![Vec::new(); n];
        for (i, b) in self.d.blocks.iter().enumerate() {
            let here = self.state_q[i];
            match b.term {
                Term::Jump(t) => into[t as usize].push(here),
                Term::Branch { cond, then_b, else_b } => {
                    let c = self.node_bits(cond)[0];
                    let taken = self.net.and(here, c);
                    let nc = self.net.not(c);
                    let not_taken = self.net.and(here, nc);
                    into[then_b as usize].push(taken);
                    into[else_b as usize].push(not_taken);
                }
                // A halted machine parks in place until reset.
                Term::Halt => into[i].push(here),
            }
        }

        let reset = self.net.reset();
        let nreset = self.net.not(reset);
        let halted = self.halted_q;
        for i in 0..n {
            let normal = self.net.or_all(&into[i]);
            // Once halted the state vector freezes, so the halt block's
            // assignments run exactly once.
            let here = self.state_q[i];
            let frozen = self.net.mux(halted, here, normal);
            let held = self.net.and(nreset, frozen);
            // Reset forces the one-hot vector to the entry state.
            let next = if i as u32 == self.d.entry {
                self.net.or(reset, held)
            } else {
                held
            };
            self.net.set_dff_d(self.state_dff[i], next);
        }
    }

    fn build_register_updates(&mut self) {
        let reset = self.net.reset();
        let nreset = self.net.not(reset);

        for r in 0..self.d.regs.len() {
            let width = self.d.regs[r].width as usize;
            // Blocks that write this register, with the value they write.
            let writers: Vec<(usize, NodeId)> = self
                .d
                .blocks
                .iter()
                .enumerate()
                .filter_map(|(i, b)| {
                    b.assigns.iter().find(|&&(rr, _)| rr as usize == r).map(|&(_, v)| (i, v))
                })
                .collect();

            if writers.is_empty() {
                // Never written: hold forever, cleared by reset.
                for bit in 0..width {
                    let q = self.reg_q[r][bit];
                    let d = self.net.and(nreset, q);
                    self.net.set_dff_d(self.reg_dff[r][bit], d);
                }
                continue;
            }
            let halted = self.halted_q;

            // A register holds its value in any state that does not write it,
            // so we only need one "is being written" term rather than a mux
            // arm per block.
            let write_states: Vec<Sig> = writers.iter().map(|&(i, _)| self.state_q[i]).collect();
            let any_write = self.net.or_all(&write_states);
            let no_write = self.net.not(any_write);

            let value_bits: Vec<Vec<Sig>> =
                writers.iter().map(|&(_, v)| self.node_bits(v)).collect();

            for bit in 0..width {
                let mut terms: Vec<Sig> = Vec::with_capacity(writers.len() + 1);
                for (w, &(i, _)) in writers.iter().enumerate() {
                    let v = value_bits[w].get(bit).copied().unwrap_or(self.net.zero());
                    let t = self.net.and(self.state_q[i], v);
                    terms.push(t);
                }
                let q = self.reg_q[r][bit];
                let hold = self.net.and(no_write, q);
                terms.push(hold);
                let next = self.net.or_all(&terms);
                // Freeze every register once the machine halts.
                let frozen = self.net.mux(halted, q, next);
                let d = self.net.and(nreset, frozen);
                self.net.set_dff_d(self.reg_dff[r][bit], d);
            }
        }
    }

    // --- combinational lowering ------------------------------------------

    fn zeros(&self, w: usize) -> Vec<Sig> {
        vec![self.net.zero(); w]
    }

    /// Bits of a word-level node, LSB first, memoised.
    fn node_bits(&mut self, id: NodeId) -> Vec<Sig> {
        if let Some(v) = self.cache.get(&id) {
            return v.clone();
        }
        // Build in topological order so no recursion is needed for deep DAGs.
        for n in self.d.topo_order(&[id]) {
            if self.cache.contains_key(&n) {
                continue;
            }
            let bits = self.build_node(n);
            self.cache.insert(n, bits);
        }
        self.cache[&id].clone()
    }

    fn get(&self, n: NodeId) -> Vec<Sig> {
        self.cache[&n].clone()
    }

    fn build_node(&mut self, n: NodeId) -> Vec<Sig> {
        let w = self.d.width(n) as usize;
        match self.d.node(n) {
            Node::Const { value, .. } => (0..w)
                .map(|i| self.net.constant((value >> i) & 1 == 1))
                .collect(),
            Node::Reg(r) => self.reg_q[r as usize].clone(),
            Node::Input(i) => (0..w).map(|b| self.net.input(i, b as u32)).collect(),
            Node::Not(a) => {
                let a = self.get(a);
                a.iter().map(|&x| self.net.not(x)).collect()
            }
            Node::And(a, b) => self.zip(a, b, |n, x, y| n.and(x, y)),
            Node::Or(a, b) => self.zip(a, b, |n, x, y| n.or(x, y)),
            Node::Xor(a, b) => self.zip(a, b, |n, x, y| n.xor(x, y)),
            Node::Add(a, b) => {
                let (x, y) = (self.get(a), self.get(b));
                let cin = self.net.zero();
                self.ripple_add(&x, &y, cin).0
            }
            Node::Lt(a, b) => {
                // a < b  <=>  the subtraction a - b borrows, i.e. no carry out.
                let (x, y) = (self.get(a), self.get(b));
                let yb: Vec<Sig> = y.iter().map(|&s| self.net.not(s)).collect();
                let one = self.net.one();
                let (_, cout) = self.ripple_add(&x, &yb, one);
                vec![self.net.not(cout)]
            }
            Node::Eq(a, b) => {
                // All bits equal <=> no XOR is set <=> NOR of the XORs.
                let (x, y) = (self.get(a), self.get(b));
                let diffs: Vec<Sig> = x
                    .iter()
                    .zip(y.iter())
                    .map(|(&p, &q)| self.net.xor(p, q))
                    .collect();
                vec![self.net.nor(&diffs)]
            }
            Node::Mul(a, b) => {
                let (x, y) = (self.get(a), self.get(b));
                self.array_multiply(&x, &y, w)
            }
            Node::Shl(a, b) => {
                let (x, s) = (self.get(a), self.get(b));
                self.barrel_shift(&x, &s, w, true)
            }
            Node::Shr(a, b) => {
                let (x, s) = (self.get(a), self.get(b));
                self.barrel_shift(&x, &s, w, false)
            }
            Node::Bit { src, bit } => {
                let s = self.get(src);
                vec![s.get(bit as usize).copied().unwrap_or(self.net.zero())]
            }
            Node::Resize { src, .. } => {
                let s = self.get(src);
                (0..w).map(|i| s.get(i).copied().unwrap_or(self.net.zero())).collect()
            }
            Node::Repeat { src, .. } => {
                let s = self.get(src);
                vec![s[0]; w]
            }
            Node::Mux { c, t, f } => {
                let cb = self.get(c)[0];
                let (tv, fv) = (self.get(t), self.get(f));
                (0..w)
                    .map(|i| {
                        let a = tv.get(i).copied().unwrap_or(self.net.zero());
                        let b = fv.get(i).copied().unwrap_or(self.net.zero());
                        self.net.mux(cb, a, b)
                    })
                    .collect()
            }
            Node::Any(a) => {
                let s = self.get(a);
                let none = self.net.nor(&s);
                vec![self.net.not(none)]
            }
        }
    }

    fn zip(
        &mut self,
        a: NodeId,
        b: NodeId,
        f: impl Fn(&mut Netlist, Sig, Sig) -> Sig,
    ) -> Vec<Sig> {
        let (x, y) = (self.get(a), self.get(b));
        x.iter().zip(y.iter()).map(|(&p, &q)| f(&mut self.net, p, q)).collect()
    }

    /// Ripple-carry adder. Returns the sum bits and the carry-out.
    fn ripple_add(&mut self, a: &[Sig], b: &[Sig], cin: Sig) -> (Vec<Sig>, Sig) {
        let w = a.len().max(b.len());
        let mut carry = cin;
        let mut sum = Vec::with_capacity(w);
        for i in 0..w {
            let x = a.get(i).copied().unwrap_or(self.net.zero());
            let y = b.get(i).copied().unwrap_or(self.net.zero());
            let s = self.net.xor3(x, y, carry);
            carry = self.net.maj3(x, y, carry);
            sum.push(s);
        }
        (sum, carry)
    }

    /// Shift-and-add array multiplier, truncated to `w` bits.
    fn array_multiply(&mut self, a: &[Sig], b: &[Sig], w: usize) -> Vec<Sig> {
        let mut acc = self.zeros(w);
        for (i, &bi) in b.iter().enumerate().take(w) {
            // Partial product: a << i, gated by b[i].
            let pp: Vec<Sig> = (0..w)
                .map(|j| {
                    if j < i {
                        self.net.zero()
                    } else {
                        let aj = a.get(j - i).copied().unwrap_or(self.net.zero());
                        self.net.and(aj, bi)
                    }
                })
                .collect();
            let cin = self.net.zero();
            acc = self.ripple_add(&acc, &pp, cin).0;
            acc.truncate(w);
        }
        acc
    }

    /// Logarithmic barrel shifter: one mux stage per bit of the shift amount.
    /// Shift amounts at or beyond the word width zero the result, which the
    /// `overflow` term below handles without building useless wide stages.
    fn barrel_shift(&mut self, a: &[Sig], s: &[Sig], w: usize, left: bool) -> Vec<Sig> {
        let stages = (0..).take_while(|k| (1usize << k) < w).count().max(1);
        let mut cur = a.to_vec();
        cur.resize(w, self.net.zero());

        for k in 0..stages.min(s.len()) {
            let amount = 1usize << k;
            let shifted: Vec<Sig> = (0..w)
                .map(|i| {
                    let src = if left { i.checked_sub(amount) } else { i.checked_add(amount) };
                    match src {
                        Some(j) if j < w => cur[j],
                        _ => self.net.zero(),
                    }
                })
                .collect();
            cur = (0..w).map(|i| self.net.mux(s[k], shifted[i], cur[i])).collect();
        }

        // Any high bit of the shift amount means "shifted out entirely".
        if s.len() > stages {
            let hi = self.net.or_all(&s[stages..]);
            let keep = self.net.not(hi);
            cur = cur.iter().map(|&x| self.net.and(keep, x)).collect();
        }
        cur
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lower::lower_program;
    use crate::machine::run_design;
    use crate::netlist::GateSim;
    use crate::parser::parse;

    fn build(src: &str) -> (Design, Netlist) {
        let d = lower_program(&parse(src).unwrap()).unwrap();
        let n = blast(&d);
        (d, n)
    }

    /// Run the gate netlist the way the hardware will: pulse reset, then clock
    /// until `done`.
    fn run_gates<'n>(
        d: &Design,
        n: &'n Netlist,
        inputs: &[(&str, u64)],
        budget: u64,
    ) -> GateSim<'n> {
        let mut vals = vec![0u64; d.inputs.len()];
        for (name, v) in inputs {
            let i = d.inputs.iter().position(|p| p.name == *name).unwrap();
            vals[i] = *v;
        }
        let mut sim = GateSim::new(n);
        sim.step(&vals, true); // reset: load the entry state
        for _ in 0..budget {
            if sim.done(&vals, false) {
                break;
            }
            sim.step(&vals, false);
        }
        sim
    }

    /// The core differential check: word-level golden model vs. gate netlist.
    fn assert_matches(src: &str, inputs: &[(&str, u64)], out: &str) {
        let (d, n) = build(src);
        let (m, _) = run_design(&d, inputs, 200_000).unwrap();
        let expect = m.output(&d, out).unwrap();

        let sim = run_gates(&d, &n, inputs, 200_000);
        let mut vals = vec![0u64; d.inputs.len()];
        for (name, v) in inputs {
            let i = d.inputs.iter().position(|p| p.name == *name).unwrap();
            vals[i] = *v;
        }
        assert!(sim.done(&vals, false), "gate netlist did not halt for {inputs:?}");
        let got = sim.read_output(out, &vals, false);
        assert_eq!(got, expect, "gates disagree with golden model for {inputs:?}");
    }

    #[test]
    fn adder_matches_golden_model() {
        let src = "input u8 a; input u8 b; output u8 q; proc main() { q = a + b; }";
        for (a, b) in [(0, 0), (1, 2), (200, 100), (255, 255), (128, 128)] {
            assert_matches(src, &[("a", a), ("b", b)], "q");
        }
    }

    #[test]
    fn subtractor_matches_golden_model() {
        let src = "input u8 a; input u8 b; output u8 q; proc main() { q = a - b; }";
        for (a, b) in [(5, 3), (3, 5), (0, 1), (255, 255)] {
            assert_matches(src, &[("a", a), ("b", b)], "q");
        }
    }

    #[test]
    fn comparisons_match_golden_model() {
        for op in ["<", "<=", ">", ">=", "==", "!="] {
            let src = format!(
                "input u8 a; input u8 b; output u1 q; proc main() {{ q = a {op} b; }}"
            );
            for (a, b) in [(0, 0), (1, 2), (2, 1), (255, 0), (0, 255), (7, 7)] {
                assert_matches(&src, &[("a", a), ("b", b)], "q");
            }
        }
    }

    #[test]
    fn multiplier_matches_golden_model() {
        let src = "input u8 a; input u8 b; output u8 q; proc main() { q = a * b; }";
        for (a, b) in [(0, 5), (1, 1), (12, 12), (20, 20), (255, 3), (16, 16)] {
            assert_matches(src, &[("a", a), ("b", b)], "q");
        }
    }

    #[test]
    fn barrel_shifter_matches_golden_model() {
        for op in ["<<", ">>"] {
            let src = format!(
                "input u8 a; input u8 b; output u8 q; proc main() {{ q = a {op} b; }}"
            );
            for b in 0..10u64 {
                assert_matches(&src, &[("a", 0b1011_0110), ("b", b)], "q");
            }
        }
    }

    #[test]
    fn sequential_gcd_matches_golden_model() {
        let src = r#"
            input u8 a; input u8 b; output u8 q;
            fn gcd(u8 x, u8 y) -> u8 {
                while (y != 0) {
                    if (x > y) { x = x - y; } else { y = y - x; }
                }
                return x;
            }
            proc main() { q = gcd(a, b); }
        "#;
        for (a, b) in [(48, 18), (17, 5), (36, 36), (100, 75), (13, 1)] {
            assert_matches(src, &[("a", a), ("b", b)], "q");
        }
    }

    #[test]
    fn loop_accumulator_matches_golden_model() {
        let src = "input u8 a; output u8 q; \
                   proc main() { var u8 n = a; var u8 acc = 0; \
                                 while (n != 0) { acc = acc + n; n = n - 1; } q = acc; }";
        for a in [0, 1, 5, 10, 20] {
            assert_matches(src, &[("a", a)], "q");
        }
    }

    #[test]
    fn one_hot_state_holds_exactly_one_bit() {
        let (d, n) = build(
            "input u8 a; output u8 q; \
             proc main() { var u8 x = a; while (x != 0) { x = x - 1; } q = x; }",
        );
        let vals = vec![3u64];
        let mut sim = GateSim::new(&n);
        sim.step(&vals, true);
        let nblocks = d.blocks.len();
        let first_state = n.dffs.iter().position(|f| f.name.starts_with("state.")).unwrap();
        for cycle in 0..40 {
            let hot = (0..nblocks).filter(|i| sim.q[first_state + i]).count();
            assert_eq!(hot, 1, "state must stay one-hot (cycle {cycle})");
            sim.step(&vals, false);
        }
    }

    #[test]
    fn reset_returns_the_machine_to_entry() {
        let (d, n) = build("input u8 a; output u8 q; proc main() { q = a + 1; }");
        let vals = vec![7u64];
        let mut sim = GateSim::new(&n);
        sim.step(&vals, true);
        let first_state = n.dffs.iter().position(|f| f.name.starts_with("state.")).unwrap();
        assert!(sim.q[first_state + d.entry as usize], "reset selects the entry state");
    }
}
