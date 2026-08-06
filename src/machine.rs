//! Cycle-accurate FSMD interpreter: the golden model.
//!
//! This executes the word-level IR directly. Everything downstream - the gate
//! netlist, and ultimately the redstone itself - is checked against the results
//! this produces.

use crate::ir::{mask, Design, Node, NodeId, Term};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Machine {
    pub regs: Vec<u64>,
    pub state: u32,
    pub done: bool,
    pub cycles: u64,
}

impl Machine {
    pub fn new(d: &Design) -> Machine {
        Machine {
            regs: d.regs.iter().map(|r| mask(r.reset, r.width)).collect(),
            state: d.entry,
            done: false,
            cycles: 0,
        }
    }

    /// Evaluate a combinational node against the current register state.
    fn eval(&self, d: &Design, id: NodeId, inputs: &[u64], memo: &mut HashMap<NodeId, u64>) -> u64 {
        if let Some(&v) = memo.get(&id) {
            return v;
        }
        // Evaluate in topological order so deep expression graphs do not blow
        // the native stack.
        for n in d.topo_order(&[id]) {
            if memo.contains_key(&n) {
                continue;
            }
            let w = d.width(n);
            let g = |x: NodeId, m: &HashMap<NodeId, u64>| -> u64 { m[&x] };
            let v = match d.node(n) {
                Node::Const { value, .. } => mask(value, w),
                Node::Reg(r) => self.regs[r as usize],
                Node::Input(i) => mask(inputs.get(i as usize).copied().unwrap_or(0), w),
                Node::Not(a) => mask(!g(a, memo), w),
                Node::And(a, b) => g(a, memo) & g(b, memo),
                Node::Or(a, b) => g(a, memo) | g(b, memo),
                Node::Xor(a, b) => g(a, memo) ^ g(b, memo),
                Node::Add(a, b) => mask(g(a, memo).wrapping_add(g(b, memo)), w),
                Node::Lt(a, b) => (g(a, memo) < g(b, memo)) as u64,
                Node::Eq(a, b) => (g(a, memo) == g(b, memo)) as u64,
                Node::Mul(a, b) => mask(g(a, memo).wrapping_mul(g(b, memo)), w),
                Node::Shl(a, b) => {
                    let s = g(b, memo);
                    if s >= 64 { 0 } else { mask(g(a, memo) << s, w) }
                }
                Node::Shr(a, b) => {
                    let s = g(b, memo);
                    if s >= 64 { 0 } else { g(a, memo) >> s }
                }
                Node::Bit { src, bit } => (g(src, memo) >> bit) & 1,
                Node::Resize { src, .. } => mask(g(src, memo), w),
                Node::Repeat { src, .. } => {
                    if g(src, memo) & 1 == 1 { mask(u64::MAX, w) } else { 0 }
                }
                Node::Mux { c, t, f } => {
                    if g(c, memo) & 1 == 1 { g(t, memo) } else { g(f, memo) }
                }
                Node::Any(a) => (g(a, memo) != 0) as u64,
            };
            memo.insert(n, v);
        }
        memo[&id]
    }

    /// Advance one clock cycle.
    pub fn step(&mut self, d: &Design, inputs: &[u64]) {
        if self.done {
            return;
        }
        let block = &d.blocks[self.state as usize];
        let mut memo = HashMap::new();

        // All right-hand sides read pre-cycle state, so compute before writing.
        let updates: Vec<(u32, u64)> = block
            .assigns
            .iter()
            .map(|&(r, n)| (r, mask(self.eval(d, n, inputs, &mut memo), d.regs[r as usize].width)))
            .collect();

        let next = match block.term {
            Term::Jump(t) => t,
            Term::Branch { cond, then_b, else_b } => {
                if self.eval(d, cond, inputs, &mut memo) & 1 == 1 {
                    then_b
                } else {
                    else_b
                }
            }
            Term::Halt => {
                self.done = true;
                self.state
            }
        };

        for (r, v) in updates {
            self.regs[r as usize] = v;
        }
        self.state = next;
        self.cycles += 1;
    }

    /// Run to completion. Errors if the machine does not halt within `budget`,
    /// which is what an infinite loop in the source looks like from here.
    pub fn run(&mut self, d: &Design, inputs: &[u64], budget: u64) -> Result<u64, String> {
        for _ in 0..budget {
            if self.done {
                return Ok(self.cycles);
            }
            self.step(d, inputs);
        }
        if self.done {
            Ok(self.cycles)
        } else {
            Err(format!("did not halt within {budget} cycles (infinite loop?)"))
        }
    }

    pub fn output(&self, d: &Design, name: &str) -> Option<u64> {
        d.outputs
            .iter()
            .find(|(n, _)| n == name)
            .map(|&(_, r)| self.regs[r as usize])
    }
}

/// Convenience: compile-free execution of a design with named inputs.
pub fn run_design(
    d: &Design,
    inputs: &[(&str, u64)],
    budget: u64,
) -> Result<(Machine, u64), String> {
    let mut vals = vec![0u64; d.inputs.len()];
    for (name, v) in inputs {
        let i = d
            .inputs
            .iter()
            .position(|p| p.name == *name)
            .ok_or_else(|| format!("no such input port `{name}`"))?;
        vals[i] = mask(*v, d.inputs[i].width);
    }
    let mut m = Machine::new(d);
    let cycles = m.run(d, &vals, budget)?;
    Ok((m, cycles))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lower::lower_program;
    use crate::parser::parse;

    fn design(src: &str) -> Design {
        lower_program(&parse(src).unwrap()).unwrap()
    }

    fn run1(src: &str, inputs: &[(&str, u64)], out: &str) -> u64 {
        let d = design(src);
        let (m, _) = run_design(&d, inputs, 100_000).unwrap();
        m.output(&d, out).unwrap()
    }

    #[test]
    fn arithmetic_wraps_at_the_declared_width() {
        let src = "input u8 a; input u8 b; output u8 q; proc main() { q = a + b; }";
        assert_eq!(run1(src, &[("a", 200), ("b", 100)], "q"), 44);
    }

    #[test]
    fn subtraction_borrows_correctly() {
        let src = "input u8 a; input u8 b; output u8 q; proc main() { q = a - b; }";
        assert_eq!(run1(src, &[("a", 5), ("b", 3)], "q"), 2);
        assert_eq!(run1(src, &[("a", 3), ("b", 5)], "q"), 254, "wraps mod 256");
    }

    #[test]
    fn comparisons_are_unsigned() {
        let src = "input u8 a; input u8 b; output u1 q; proc main() { q = a < b; }";
        assert_eq!(run1(src, &[("a", 1), ("b", 200)], "q"), 1);
        assert_eq!(run1(src, &[("a", 200), ("b", 1)], "q"), 0);
    }

    /// The swap test: sequential source semantics inside a single cycle.
    #[test]
    fn assignments_are_sequential_within_a_cycle() {
        let src = "input u8 a; input u8 b; output u8 q; \
                   proc main() { var u8 x = a; var u8 y = b; x = y; y = x; q = y; }";
        // Sequential semantics: x becomes b, then y becomes x (= b).
        assert_eq!(run1(src, &[("a", 7), ("b", 9)], "q"), 9);
    }

    #[test]
    fn while_loop_runs_to_completion() {
        let src = "input u8 a; output u8 q; \
                   proc main() { var u8 n = a; var u8 acc = 0; \
                                 while (n != 0) { acc = acc + n; n = n - 1; } q = acc; }";
        // Triangular number 10 -> 55.
        assert_eq!(run1(src, &[("a", 10)], "q"), 55);
    }

    #[test]
    fn gcd_by_subtraction() {
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
        assert_eq!(run1(src, &[("a", 48), ("b", 18)], "q"), 6);
        assert_eq!(run1(src, &[("a", 17), ("b", 5)], "q"), 1);
        assert_eq!(run1(src, &[("a", 36), ("b", 36)], "q"), 36);
    }

    #[test]
    fn multiplication_and_shifts() {
        let src = "input u8 a; input u8 b; output u8 q; proc main() { q = a * b; }";
        assert_eq!(run1(src, &[("a", 12), ("b", 12)], "q"), 144);
        assert_eq!(run1(src, &[("a", 20), ("b", 20)], "q"), 400 % 256);

        let src = "input u8 a; input u8 b; output u8 q; proc main() { q = a << b; }";
        assert_eq!(run1(src, &[("a", 3), ("b", 2)], "q"), 12);
        let src = "input u8 a; input u8 b; output u8 q; proc main() { q = a >> b; }";
        assert_eq!(run1(src, &[("a", 12), ("b", 2)], "q"), 3);
    }

    #[test]
    fn early_return_from_a_function() {
        let src = r#"
            input u8 a; output u8 q;
            fn clamp(u8 x) -> u8 {
                if (x > 100) { return 100; }
                return x;
            }
            proc main() { q = clamp(a); }
        "#;
        assert_eq!(run1(src, &[("a", 5)], "q"), 5);
        assert_eq!(run1(src, &[("a", 200)], "q"), 100);
    }

    #[test]
    fn infinite_loop_is_reported_not_hung() {
        let d = design("output u8 q; proc main() { while (q >= 0) { q = q + 1; } }");
        let err = run_design(&d, &[], 500).unwrap_err();
        assert!(err.contains("did not halt"), "{err}");
    }

    #[test]
    fn ternary_and_logical_ops() {
        let src = "input u8 a; output u8 q; proc main() { q = (a > 10) ? 1 : 2; }";
        assert_eq!(run1(src, &[("a", 20)], "q"), 1);
        assert_eq!(run1(src, &[("a", 2)], "q"), 2);

        let src = "input u8 a; input u8 b; output u1 q; proc main() { q = (a > 1) && (b > 1); }";
        assert_eq!(run1(src, &[("a", 5), ("b", 5)], "q"), 1);
        assert_eq!(run1(src, &[("a", 5), ("b", 0)], "q"), 0);
    }
}
