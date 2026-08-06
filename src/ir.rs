//! Word-level intermediate representation: a hash-consed combinational DAG plus
//! a finite state machine over it (an "FSMD").
//!
//! # Execution model
//!
//! Each basic block is exactly **one clock cycle**. Within a block, expressions
//! are pure combinational functions of the *current* register values; every
//! register write commits simultaneously at the end of the cycle. Sequential
//! source semantics are preserved during lowering by threading an environment
//! that maps each variable to the expression computed so far, so `x = y; y = t;`
//! reads the pre-cycle `y` for the first statement and still commits both.
//!
//! Nodes are hash-consed, which gives structural common-subexpression
//! elimination for free: building the same expression twice returns the same id.

use std::collections::HashMap;

pub type NodeId = u32;
pub type RegId = u32;
pub type BlockId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Node {
    Const { value: u64, width: u32 },
    /// Current value of a register (the value at the start of the cycle).
    Reg(RegId),
    /// Value of an input port.
    Input(u32),
    Not(NodeId),
    And(NodeId, NodeId),
    Or(NodeId, NodeId),
    Xor(NodeId, NodeId),
    Add(NodeId, NodeId),
    /// Unsigned `a < b`, result width 1.
    Lt(NodeId, NodeId),
    /// `a == b`, result width 1.
    Eq(NodeId, NodeId),
    Mul(NodeId, NodeId),
    Shl(NodeId, NodeId),
    Shr(NodeId, NodeId),
    /// Bit select: `(src >> bit) & 1`, result width 1.
    Bit { src: NodeId, bit: u32 },
    /// Zero-extend or truncate to `width`.
    Resize { src: NodeId, width: u32 },
    /// `c ? t : f`, where `c` has width 1.
    Mux { c: NodeId, t: NodeId, f: NodeId },
    /// OR-reduction to width 1: "is this value nonzero".
    Any(NodeId),
    /// Concatenate `width`-1 zero bits above a width-1 value... expressed as a
    /// resize, so this variant only carries single-bit values up to a wider type.
    Repeat { src: NodeId, width: u32 },
}

#[derive(Debug, Clone)]
pub struct RegDecl {
    pub name: String,
    pub width: u32,
    /// Registers start at zero on world load; this documents that assumption.
    pub reset: u64,
}

#[derive(Debug, Clone)]
pub struct PortDecl {
    pub name: String,
    pub width: u32,
}

#[derive(Debug, Clone)]
pub enum Term {
    Jump(BlockId),
    Branch { cond: NodeId, then_b: BlockId, else_b: BlockId },
    /// Terminal state: the machine parks here and asserts `done`.
    Halt,
}

#[derive(Debug, Clone)]
pub struct BasicBlock {
    pub label: String,
    /// Register writes committed at the end of this cycle. Order is irrelevant:
    /// every right-hand side reads pre-cycle state.
    pub assigns: Vec<(RegId, NodeId)>,
    pub term: Term,
}

#[derive(Debug, Default)]
pub struct Design {
    nodes: Vec<Node>,
    widths: Vec<u32>,
    hash: HashMap<Node, NodeId>,
    pub regs: Vec<RegDecl>,
    pub inputs: Vec<PortDecl>,
    /// Outputs are registers exposed to the world; the value is the reg id.
    pub outputs: Vec<(String, RegId)>,
    pub blocks: Vec<BasicBlock>,
    pub entry: BlockId,
}

impl Design {
    pub fn new() -> Design {
        Design::default()
    }

    pub fn node(&self, id: NodeId) -> Node {
        self.nodes[id as usize]
    }

    pub fn width(&self, id: NodeId) -> u32 {
        self.widths[id as usize]
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Intern a node, computing its width. Returns an existing id when an
    /// identical node was already built.
    fn intern(&mut self, n: Node, width: u32) -> NodeId {
        if let Some(&id) = self.hash.get(&n) {
            return id;
        }
        let id = self.nodes.len() as NodeId;
        self.nodes.push(n);
        self.widths.push(width);
        self.hash.insert(n, id);
        id
    }

    pub fn add_reg(&mut self, name: &str, width: u32) -> RegId {
        let id = self.regs.len() as RegId;
        self.regs.push(RegDecl { name: name.to_string(), width, reset: 0 });
        id
    }

    pub fn add_input(&mut self, name: &str, width: u32) -> u32 {
        let id = self.inputs.len() as u32;
        self.inputs.push(PortDecl { name: name.to_string(), width });
        id
    }

    pub fn add_block(&mut self, label: &str) -> BlockId {
        let id = self.blocks.len() as BlockId;
        self.blocks.push(BasicBlock {
            label: label.to_string(),
            assigns: Vec::new(),
            term: Term::Halt,
        });
        id
    }

    // --- constructors -----------------------------------------------------

    pub fn constant(&mut self, value: u64, width: u32) -> NodeId {
        let value = mask(value, width);
        self.intern(Node::Const { value, width }, width)
    }

    pub fn zero(&mut self, width: u32) -> NodeId {
        self.constant(0, width)
    }

    pub fn one(&mut self, width: u32) -> NodeId {
        self.constant(1, width)
    }

    pub fn reg(&mut self, r: RegId) -> NodeId {
        let w = self.regs[r as usize].width;
        self.intern(Node::Reg(r), w)
    }

    pub fn input(&mut self, i: u32) -> NodeId {
        let w = self.inputs[i as usize].width;
        self.intern(Node::Input(i), w)
    }

    pub fn not(&mut self, a: NodeId) -> NodeId {
        let w = self.width(a);
        if let Node::Const { value, .. } = self.node(a) {
            return self.constant(!value, w);
        }
        // Double negation cancels.
        if let Node::Not(inner) = self.node(a) {
            return inner;
        }
        self.intern(Node::Not(a), w)
    }

    pub fn and(&mut self, a: NodeId, b: NodeId) -> NodeId {
        let w = self.width(a);
        if let (Node::Const { value: x, .. }, Node::Const { value: y, .. }) =
            (self.node(a), self.node(b))
        {
            return self.constant(x & y, w);
        }
        if a == b {
            return a;
        }
        let (a, b) = order(a, b);
        self.intern(Node::And(a, b), w)
    }

    pub fn or(&mut self, a: NodeId, b: NodeId) -> NodeId {
        let w = self.width(a);
        if let (Node::Const { value: x, .. }, Node::Const { value: y, .. }) =
            (self.node(a), self.node(b))
        {
            return self.constant(x | y, w);
        }
        if a == b {
            return a;
        }
        let (a, b) = order(a, b);
        self.intern(Node::Or(a, b), w)
    }

    pub fn xor(&mut self, a: NodeId, b: NodeId) -> NodeId {
        let w = self.width(a);
        if let (Node::Const { value: x, .. }, Node::Const { value: y, .. }) =
            (self.node(a), self.node(b))
        {
            return self.constant(x ^ y, w);
        }
        if a == b {
            return self.zero(w);
        }
        let (a, b) = order(a, b);
        self.intern(Node::Xor(a, b), w)
    }

    pub fn add(&mut self, a: NodeId, b: NodeId) -> NodeId {
        let w = self.width(a);
        if let (Node::Const { value: x, .. }, Node::Const { value: y, .. }) =
            (self.node(a), self.node(b))
        {
            return self.constant(x.wrapping_add(y), w);
        }
        let (a, b) = order(a, b);
        self.intern(Node::Add(a, b), w)
    }

    /// Two's complement subtraction: `a + ~b + 1`, all at width `w`.
    pub fn sub(&mut self, a: NodeId, b: NodeId) -> NodeId {
        let w = self.width(a);
        if let (Node::Const { value: x, .. }, Node::Const { value: y, .. }) =
            (self.node(a), self.node(b))
        {
            return self.constant(x.wrapping_sub(y), w);
        }
        let nb = self.not(b);
        let one = self.one(w);
        let t = self.add(a, nb);
        self.add(t, one)
    }

    pub fn mul(&mut self, a: NodeId, b: NodeId) -> NodeId {
        let w = self.width(a);
        if let (Node::Const { value: x, .. }, Node::Const { value: y, .. }) =
            (self.node(a), self.node(b))
        {
            return self.constant(x.wrapping_mul(y), w);
        }
        let (a, b) = order(a, b);
        self.intern(Node::Mul(a, b), w)
    }

    pub fn lt(&mut self, a: NodeId, b: NodeId) -> NodeId {
        if let (Node::Const { value: x, .. }, Node::Const { value: y, .. }) =
            (self.node(a), self.node(b))
        {
            return self.constant((x < y) as u64, 1);
        }
        if a == b {
            return self.zero(1);
        }
        self.intern(Node::Lt(a, b), 1)
    }

    pub fn eq(&mut self, a: NodeId, b: NodeId) -> NodeId {
        if let (Node::Const { value: x, .. }, Node::Const { value: y, .. }) =
            (self.node(a), self.node(b))
        {
            return self.constant((x == y) as u64, 1);
        }
        if a == b {
            return self.one(1);
        }
        let (a, b) = order(a, b);
        self.intern(Node::Eq(a, b), 1)
    }

    pub fn shl(&mut self, a: NodeId, b: NodeId) -> NodeId {
        let w = self.width(a);
        if let Node::Const { value: s, .. } = self.node(b) {
            if let Node::Const { value: x, .. } = self.node(a) {
                let r = if s >= 64 { 0 } else { x.wrapping_shl(s as u32) };
                return self.constant(r, w);
            }
        }
        self.intern(Node::Shl(a, b), w)
    }

    pub fn shr(&mut self, a: NodeId, b: NodeId) -> NodeId {
        let w = self.width(a);
        if let Node::Const { value: s, .. } = self.node(b) {
            if let Node::Const { value: x, .. } = self.node(a) {
                let r = if s >= 64 { 0 } else { mask(x, w) >> s };
                return self.constant(r, w);
            }
        }
        self.intern(Node::Shr(a, b), w)
    }

    pub fn bit(&mut self, src: NodeId, bit: u32) -> NodeId {
        if bit >= self.width(src) {
            return self.zero(1);
        }
        if let Node::Const { value, .. } = self.node(src) {
            return self.constant((value >> bit) & 1, 1);
        }
        self.intern(Node::Bit { src, bit }, 1)
    }

    pub fn resize(&mut self, src: NodeId, width: u32) -> NodeId {
        if self.width(src) == width {
            return src;
        }
        if let Node::Const { value, .. } = self.node(src) {
            return self.constant(value, width);
        }
        self.intern(Node::Resize { src, width }, width)
    }

    /// Broadcast a width-1 value into every bit of a `width`-bit word.
    pub fn repeat(&mut self, src: NodeId, width: u32) -> NodeId {
        debug_assert_eq!(self.width(src), 1);
        if let Node::Const { value, .. } = self.node(src) {
            let v = if value & 1 == 1 { u64::MAX } else { 0 };
            return self.constant(v, width);
        }
        self.intern(Node::Repeat { src, width }, width)
    }

    pub fn mux(&mut self, c: NodeId, t: NodeId, f: NodeId) -> NodeId {
        if t == f {
            return t;
        }
        if let Node::Const { value, .. } = self.node(c) {
            return if value & 1 == 1 { t } else { f };
        }
        let w = self.width(t);
        self.intern(Node::Mux { c, t, f }, w)
    }

    /// Reduce to width 1: nonzero test.
    pub fn any(&mut self, a: NodeId) -> NodeId {
        if self.width(a) == 1 {
            return a;
        }
        if let Node::Const { value, .. } = self.node(a) {
            return self.constant((value != 0) as u64, 1);
        }
        self.intern(Node::Any(a), 1)
    }

    // --- derived comparisons ---------------------------------------------

    pub fn ne(&mut self, a: NodeId, b: NodeId) -> NodeId {
        let e = self.eq(a, b);
        self.not(e)
    }

    pub fn le(&mut self, a: NodeId, b: NodeId) -> NodeId {
        // a <= b  <=>  !(b < a)
        let g = self.lt(b, a);
        self.not(g)
    }

    pub fn gt(&mut self, a: NodeId, b: NodeId) -> NodeId {
        self.lt(b, a)
    }

    pub fn ge(&mut self, a: NodeId, b: NodeId) -> NodeId {
        let l = self.lt(a, b);
        self.not(l)
    }

    /// Every node reachable from the design's roots, in topological order
    /// (operands before users).
    pub fn topo_order(&self, roots: &[NodeId]) -> Vec<NodeId> {
        let mut seen = vec![false; self.nodes.len()];
        let mut out = Vec::new();
        // Explicit stack: designs get deep enough that recursion is a real risk.
        let mut stack: Vec<(NodeId, bool)> = roots.iter().map(|&r| (r, false)).collect();
        while let Some((id, expanded)) = stack.pop() {
            if expanded {
                out.push(id);
                continue;
            }
            if seen[id as usize] {
                continue;
            }
            seen[id as usize] = true;
            stack.push((id, true));
            for op in self.operands(id) {
                if !seen[op as usize] {
                    stack.push((op, false));
                }
            }
        }
        out
    }

    pub fn operands(&self, id: NodeId) -> Vec<NodeId> {
        match self.node(id) {
            Node::Const { .. } | Node::Reg(_) | Node::Input(_) => vec![],
            Node::Not(a) | Node::Any(a) => vec![a],
            Node::Bit { src, .. } | Node::Resize { src, .. } | Node::Repeat { src, .. } => vec![src],
            Node::And(a, b)
            | Node::Or(a, b)
            | Node::Xor(a, b)
            | Node::Add(a, b)
            | Node::Lt(a, b)
            | Node::Eq(a, b)
            | Node::Mul(a, b)
            | Node::Shl(a, b)
            | Node::Shr(a, b) => vec![a, b],
            Node::Mux { c, t, f } => vec![c, t, f],
        }
    }

    /// All node roots the design actually needs: block assignments and branch
    /// conditions.
    pub fn roots(&self) -> Vec<NodeId> {
        let mut roots = Vec::new();
        for b in &self.blocks {
            for &(_, n) in &b.assigns {
                roots.push(n);
            }
            if let Term::Branch { cond, .. } = b.term {
                roots.push(cond);
            }
        }
        roots
    }
}

/// Commutative operands are sorted so `a op b` and `b op a` hash-cons together.
fn order(a: NodeId, b: NodeId) -> (NodeId, NodeId) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

pub fn mask(v: u64, width: u32) -> u64 {
    if width >= 64 {
        v
    } else {
        v & ((1u64 << width) - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_expressions_are_shared() {
        let mut d = Design::new();
        let r = d.add_reg("x", 8);
        let a = d.reg(r);
        let k = d.constant(3, 8);
        let e1 = d.add(a, k);
        let e2 = d.add(a, k);
        assert_eq!(e1, e2, "hash-consing gives CSE");
    }

    #[test]
    fn commutative_operands_are_canonicalised() {
        let mut d = Design::new();
        let r = d.add_reg("x", 8);
        let a = d.reg(r);
        let k = d.constant(3, 8);
        assert_eq!(d.add(a, k), d.add(k, a));
        assert_eq!(d.and(a, k), d.and(k, a));
        // Subtraction and comparison are not commutative and must not collapse.
        assert_ne!(d.lt(a, k), d.lt(k, a));
    }

    #[test]
    fn constants_fold() {
        let mut d = Design::new();
        let a = d.constant(5, 8);
        let b = d.constant(3, 8);
        let s = d.add(a, b);
        assert_eq!(d.node(s), Node::Const { value: 8, width: 8 });
        let diff = d.sub(a, b);
        assert_eq!(d.node(diff), Node::Const { value: 2, width: 8 });
        let lt = d.lt(a, b);
        assert_eq!(d.node(lt), Node::Const { value: 0, width: 1 });
    }

    #[test]
    fn constants_wrap_to_their_width() {
        let mut d = Design::new();
        let a = d.constant(200, 8);
        let b = d.constant(100, 8);
        let s = d.add(a, b);
        assert_eq!(d.node(s), Node::Const { value: 44, width: 8 }, "300 mod 256");
    }

    #[test]
    fn double_negation_cancels() {
        let mut d = Design::new();
        let r = d.add_reg("x", 8);
        let a = d.reg(r);
        let n = d.not(a);
        assert_eq!(d.not(n), a);
    }

    #[test]
    fn mux_with_constant_condition_collapses() {
        let mut d = Design::new();
        let r = d.add_reg("x", 8);
        let t = d.reg(r);
        let f = d.constant(9, 8);
        let yes = d.one(1);
        let no = d.zero(1);
        assert_eq!(d.mux(yes, t, f), t);
        assert_eq!(d.mux(no, t, f), f);
    }

    #[test]
    fn topo_order_puts_operands_first() {
        let mut d = Design::new();
        let r = d.add_reg("x", 8);
        let a = d.reg(r);
        let k = d.constant(3, 8);
        let s = d.add(a, k);
        let t = d.mul(s, a);
        let order = d.topo_order(&[t]);
        let pos = |n: NodeId| order.iter().position(|&x| x == n).unwrap();
        assert!(pos(a) < pos(s));
        assert!(pos(k) < pos(s));
        assert!(pos(s) < pos(t));
        assert_eq!(*order.last().unwrap(), t);
    }
}
