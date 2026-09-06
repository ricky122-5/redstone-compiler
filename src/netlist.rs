//! Gate-level netlist over a single logic primitive: the multi-input NOR.
//!
//! NOR is chosen because it is exactly what redstone gives you for free. Several
//! signals feeding one dust net OR together (dust takes the strongest source),
//! and a torch attached to the block under that net inverts it. So an
//! *arbitrary fan-in* NOR is one torch, which makes it the natural technology
//! primitive rather than an arbitrary choice.
//!
//! Sequential state lives in `Dff`s, which are treated as opaque macro cells.
//! They are the only cyclic element, so the combinational part of the netlist is
//! always a DAG - which is what lets the placer levelize it.

use std::collections::HashMap;

pub type Sig = u32;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Src {
    /// Constant true. Physically a torch on a block nothing ever powers.
    One,
    /// Constant false, derived by inverting `One`.
    Zero,
    /// Bit `bit` of input port `port`.
    Input { port: u32, bit: u32 },
    /// The global reset lever.
    Reset,
    /// Output of flip-flop `0..dffs.len()`.
    DffQ(u32),
    /// NOR of the listed signals. Inputs are sorted and deduplicated so
    /// structurally identical gates hash-cons together.
    Nor(Vec<Sig>),
}

#[derive(Debug, Clone)]
pub struct Dff {
    pub name: String,
    /// The signal sampled on the clock edge. Filled in after construction,
    /// because a flip-flop's D almost always depends on its own Q.
    pub d: Sig,
}

#[derive(Debug, Default)]
pub struct Netlist {
    pub sigs: Vec<Src>,
    hash: HashMap<Src, Sig>,
    pub dffs: Vec<Dff>,
    /// Input port names, indexed by the `port` field of [`Src::Input`].
    ///
    /// Outputs have carried their source-level name all along; inputs did not,
    /// so everything downstream had to call them `port0`, `port1`. That is fine
    /// until something outside the compiler needs to drive them: the in-game
    /// harness has to pass `n=2` to the golden model to compare against, and
    /// with only an index to go on it hardcoded a guess.
    pub input_names: Vec<String>,
    /// Output ports, least-significant bit first.
    pub outputs: Vec<(String, Vec<Sig>)>,
    /// Asserted when the machine has halted.
    pub done: Sig,
}

impl Netlist {
    pub fn new() -> Netlist {
        let mut n = Netlist::default();
        // Intern the constants first so they get stable low ids.
        let one = n.intern(Src::One);
        let zero = n.intern(Src::Zero);
        debug_assert_eq!((one, zero), (0, 1));
        n.done = zero;
        n
    }

    fn intern(&mut self, s: Src) -> Sig {
        if let Some(&id) = self.hash.get(&s) {
            return id;
        }
        let id = self.sigs.len() as Sig;
        self.sigs.push(s.clone());
        self.hash.insert(s, id);
        id
    }

    pub fn one(&self) -> Sig {
        0
    }

    pub fn zero(&self) -> Sig {
        1
    }

    pub fn constant(&self, v: bool) -> Sig {
        if v {
            self.one()
        } else {
            self.zero()
        }
    }

    pub fn src(&self, s: Sig) -> &Src {
        &self.sigs[s as usize]
    }

    pub fn input(&mut self, port: u32, bit: u32) -> Sig {
        self.intern(Src::Input { port, bit })
    }

    pub fn reset(&mut self) -> Sig {
        self.intern(Src::Reset)
    }

    pub fn add_dff(&mut self, name: &str) -> (u32, Sig) {
        let idx = self.dffs.len() as u32;
        // `d` is patched later via `set_dff_d`; zero is a safe placeholder.
        self.dffs.push(Dff { name: name.to_string(), d: self.zero() });
        let q = self.intern(Src::DffQ(idx));
        (idx, q)
    }

    /// The signal carrying flip-flop `idx`'s output.
    ///
    /// The placer needs this to treat Q as a source, the same way it treats an
    /// input lever. `sigs` is hash-consed, so the lookup is a scan rather than a
    /// second map to keep in step.
    pub fn dff_q(&self, idx: u32) -> Option<Sig> {
        self.sigs.iter().position(|s| matches!(s, Src::DffQ(i) if *i == idx)).map(|p| p as Sig)
    }

    pub fn set_dff_d(&mut self, idx: u32, d: Sig) {
        self.dffs[idx as usize].d = d;
    }

    /// NOR with constant folding and canonicalisation.
    pub fn nor(&mut self, inputs: &[Sig]) -> Sig {
        let mut v: Vec<Sig> = Vec::with_capacity(inputs.len());
        for &i in inputs {
            // A single true input forces the output low.
            if i == self.one() {
                return self.zero();
            }
            // False inputs contribute nothing to an OR.
            if i == self.zero() {
                continue;
            }
            v.push(i);
        }
        v.sort_unstable();
        v.dedup();
        if v.is_empty() {
            // NOR of nothing is true: a torch on a block nothing powers.
            return self.one();
        }
        self.intern(Src::Nor(v))
    }

    pub fn not(&mut self, a: Sig) -> Sig {
        // Peephole: NOT(NOR(x)) is x, which collapses the double inverters that
        // and/or construction produces in bulk.
        if let Src::Nor(ins) = self.src(a) {
            if ins.len() == 1 {
                return ins[0];
            }
        }
        self.nor(&[a])
    }

    pub fn or(&mut self, a: Sig, b: Sig) -> Sig {
        let n = self.nor(&[a, b]);
        self.not(n)
    }

    pub fn or_all(&mut self, xs: &[Sig]) -> Sig {
        let n = self.nor(xs);
        self.not(n)
    }

    pub fn and(&mut self, a: Sig, b: Sig) -> Sig {
        let na = self.not(a);
        let nb = self.not(b);
        self.nor(&[na, nb])
    }

    pub fn and_all(&mut self, xs: &[Sig]) -> Sig {
        let inv: Vec<Sig> = xs.iter().map(|&x| self.not(x)).collect();
        self.nor(&inv)
    }

    /// XOR built as `NOR(NOR(a,b), AND(a,b))`, i.e. `(a|b) & !(a&b)`.
    pub fn xor(&mut self, a: Sig, b: Sig) -> Sig {
        if a == b {
            return self.zero();
        }
        let nor_ab = self.nor(&[a, b]);
        let and_ab = self.and(a, b);
        self.nor(&[nor_ab, and_ab])
    }

    pub fn xnor(&mut self, a: Sig, b: Sig) -> Sig {
        let x = self.xor(a, b);
        self.not(x)
    }

    pub fn mux(&mut self, c: Sig, t: Sig, f: Sig) -> Sig {
        if t == f {
            return t;
        }
        if c == self.one() {
            return t;
        }
        if c == self.zero() {
            return f;
        }
        let nc = self.not(c);
        let a = self.and(c, t);
        let b = self.and(nc, f);
        self.or(a, b)
    }

    /// Majority of three: the carry-out of a full adder.
    ///
    /// Written as an OR of ANDs. The tempting "optimisation" is the two-level
    /// form `NOR(NOR(a,b), NOR(a,c), NOR(b,c))`, on the theory that each AND
    /// costs two NOR levels so this costs four. Measured, it does not help:
    /// `or_all` emits `not(nor(X))`, a single-input NOR that the *next* bit's
    /// `not()` peephole strips for free, so the carry chain is already two
    /// levels per bit either way. The two-level form saves exactly one level
    /// across an entire adder and shares fewer subexpressions with the
    /// surrounding datapath, which measured worse on real programs.
    /// See `examples/depth_probe.rs`.
    pub fn maj3(&mut self, a: Sig, b: Sig, c: Sig) -> Sig {
        let ab = self.and(a, b);
        let ac = self.and(a, c);
        let bc = self.and(b, c);
        self.or_all(&[ab, ac, bc])
    }

    pub fn xor3(&mut self, a: Sig, b: Sig, c: Sig) -> Sig {
        let x = self.xor(a, b);
        self.xor(x, c)
    }

    pub fn gate_count(&self) -> usize {
        self.sigs.iter().filter(|s| matches!(s, Src::Nor(_))).count()
    }

    pub fn operands(&self, s: Sig) -> &[Sig] {
        match &self.sigs[s as usize] {
            Src::Nor(v) => v,
            _ => &[],
        }
    }

    /// Combinational signals in topological order (operands before users).
    /// DFF outputs and primary inputs are leaves, so this always terminates.
    pub fn topo_order(&self, roots: &[Sig]) -> Vec<Sig> {
        let mut seen = vec![false; self.sigs.len()];
        let mut out = Vec::new();
        let mut stack: Vec<(Sig, bool)> = roots.iter().map(|&r| (r, false)).collect();
        while let Some((s, expanded)) = stack.pop() {
            if expanded {
                out.push(s);
                continue;
            }
            if seen[s as usize] {
                continue;
            }
            seen[s as usize] = true;
            stack.push((s, true));
            for &op in self.operands(s) {
                if !seen[op as usize] {
                    stack.push((op, false));
                }
            }
        }
        out
    }

    /// Every signal the design needs to compute: flip-flop inputs, outputs,
    /// and `done`.
    pub fn roots(&self) -> Vec<Sig> {
        let mut r: Vec<Sig> = self.dffs.iter().map(|d| d.d).collect();
        for (_, bits) in &self.outputs {
            r.extend(bits.iter().copied());
        }
        r.push(self.done);
        r
    }

    /// Longest combinational path, in gate levels, from any leaf to any root.
    /// This is what sets the minimum safe clock period.
    pub fn logic_depth(&self) -> u32 {
        let roots = self.roots();
        let mut depth = vec![0u32; self.sigs.len()];
        for s in self.topo_order(&roots) {
            let d = self
                .operands(s)
                .iter()
                .map(|&o| depth[o as usize])
                .max()
                .unwrap_or(0);
            depth[s as usize] = if matches!(self.sigs[s as usize], Src::Nor(_)) { d + 1 } else { d };
        }
        roots.iter().map(|&r| depth[r as usize]).max().unwrap_or(0)
    }
}

/// Evaluate the combinational network for one clock cycle.
pub struct GateSim<'n> {
    pub net: &'n Netlist,
    pub q: Vec<bool>,
}

impl<'n> GateSim<'n> {
    pub fn new(net: &'n Netlist) -> GateSim<'n> {
        GateSim { net, q: vec![false; net.dffs.len()] }
    }

    /// Values of every signal given the current flip-flop state.
    pub fn eval(&self, inputs: &[u64], reset: bool) -> Vec<bool> {
        let mut val = vec![false; self.net.sigs.len()];
        let roots = self.net.roots();
        for s in self.net.topo_order(&roots) {
            val[s as usize] = match &self.net.sigs[s as usize] {
                Src::One => true,
                Src::Zero => false,
                Src::Reset => reset,
                Src::Input { port, bit } => {
                    (inputs.get(*port as usize).copied().unwrap_or(0) >> bit) & 1 == 1
                }
                Src::DffQ(i) => self.q[*i as usize],
                Src::Nor(ins) => !ins.iter().any(|&i| val[i as usize]),
            };
        }
        val
    }

    /// Clock one edge: sample every D and commit.
    pub fn step(&mut self, inputs: &[u64], reset: bool) {
        let val = self.eval(inputs, reset);
        self.q = self.net.dffs.iter().map(|d| val[d.d as usize]).collect();
    }

    pub fn read_output(&self, name: &str, inputs: &[u64], reset: bool) -> u64 {
        let val = self.eval(inputs, reset);
        let Some((_, bits)) = self.net.outputs.iter().find(|(n, _)| n == name) else {
            return 0;
        };
        bits.iter()
            .enumerate()
            .map(|(i, &b)| (val[b as usize] as u64) << i)
            .sum()
    }

    pub fn done(&self, inputs: &[u64], reset: bool) -> bool {
        self.eval(inputs, reset)[self.net.done as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Evaluate a purely constant/1-input expression for all input assignments.
    fn truth_table(build: impl Fn(&mut Netlist, &[Sig]) -> Sig, arity: u32) -> Vec<bool> {
        let mut n = Netlist::new();
        let ins: Vec<Sig> = (0..arity).map(|i| n.input(0, i)).collect();
        let out = build(&mut n, &ins);
        n.outputs.push(("o".into(), vec![out]));
        let sim = GateSim::new(&n);
        (0..(1u64 << arity))
            .map(|v| sim.eval(&[v], false)[out as usize])
            .collect()
    }

    #[test]
    fn nor_of_nothing_is_true() {
        let mut n = Netlist::new();
        let s = n.nor(&[]);
        assert_eq!(s, n.one());
    }

    #[test]
    fn basic_gates_have_correct_truth_tables() {
        assert_eq!(truth_table(|n, i| n.not(i[0]), 1), vec![true, false]);
        // Index order is b<<1 | a.
        assert_eq!(truth_table(|n, i| n.and(i[0], i[1]), 2), vec![false, false, false, true]);
        assert_eq!(truth_table(|n, i| n.or(i[0], i[1]), 2), vec![false, true, true, true]);
        assert_eq!(truth_table(|n, i| n.xor(i[0], i[1]), 2), vec![false, true, true, false]);
        assert_eq!(truth_table(|n, i| n.xnor(i[0], i[1]), 2), vec![true, false, false, true]);
        assert_eq!(
            truth_table(|n, i| n.nor(&[i[0], i[1]]), 2),
            vec![true, false, false, false]
        );
    }

    #[test]
    fn mux_selects_correctly() {
        // c = bit0, t = bit1, f = bit2
        let table = truth_table(|n, i| n.mux(i[0], i[1], i[2]), 3);
        for v in 0..8u64 {
            let (c, t, f) = (v & 1 == 1, (v >> 1) & 1 == 1, (v >> 2) & 1 == 1);
            assert_eq!(table[v as usize], if c { t } else { f }, "v={v}");
        }
    }

    #[test]
    fn maj3_and_xor3_match_a_full_adder() {
        let sum = truth_table(|n, i| n.xor3(i[0], i[1], i[2]), 3);
        let carry = truth_table(|n, i| n.maj3(i[0], i[1], i[2]), 3);
        for v in 0..8u64 {
            let bits = (v & 1) + ((v >> 1) & 1) + ((v >> 2) & 1);
            assert_eq!(sum[v as usize], bits % 2 == 1, "sum v={v}");
            assert_eq!(carry[v as usize], bits >= 2, "carry v={v}");
        }
    }

    #[test]
    fn identical_gates_are_shared() {
        let mut n = Netlist::new();
        let a = n.input(0, 0);
        let b = n.input(0, 1);
        assert_eq!(n.nor(&[a, b]), n.nor(&[b, a]), "operand order is canonical");
        assert_eq!(n.and(a, b), n.and(a, b));
    }

    #[test]
    fn constants_fold_through_nor() {
        let mut n = Netlist::new();
        let a = n.input(0, 0);
        let one = n.one();
        let zero = n.zero();
        assert_eq!(n.nor(&[a, one]), zero, "a true input forces the output low");
        assert_eq!(n.nor(&[a, zero]), n.nor(&[a]), "false inputs drop out");
    }

    #[test]
    fn double_inversion_is_peepholed() {
        let mut n = Netlist::new();
        let a = n.input(0, 0);
        let na = n.not(a);
        assert_eq!(n.not(na), a);
    }

    #[test]
    fn logic_depth_counts_gate_levels() {
        let mut n = Netlist::new();
        let a = n.input(0, 0);
        let b = n.not(a);
        let c = n.not(b);
        n.outputs.push(("o".into(), vec![c]));
        // not(not(a)) peepholes back to `a`, so depth is 0.
        assert_eq!(n.logic_depth(), 0);

        let mut n = Netlist::new();
        let a = n.input(0, 0);
        let b = n.input(0, 1);
        let g = n.and(a, b); // not, not -> nor  = 2 levels
        n.outputs.push(("o".into(), vec![g]));
        assert_eq!(n.logic_depth(), 2);
    }
}
