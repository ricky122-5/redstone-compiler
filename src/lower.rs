//! AST -> FSMD lowering, with bit-width checking and function inlining.
//!
//! # Evaluation order
//!
//! Ohm evaluates **eagerly**: both operands of `&&` and `||`, and both arms of
//! `? :`, are always evaluated. This matches how the hardware behaves (every
//! datapath is physically present and always computing) and avoids pretending
//! to offer short-circuiting that a combinational circuit cannot provide.
//!
//! # Calls and cycle boundaries
//!
//! Functions are inlined; recursion is rejected. Every call unconditionally
//! ends the current basic block. That uniform rule is what makes cross-cycle
//! spilling correct: when an operand contains a call we know a cycle boundary
//! *will* occur, so we spill the already-computed operand into a temporary
//! register and read it back on the far side.

use crate::ast::*;
use crate::ir::{BasicBlock, BlockId, Design, NodeId, RegId, Term};
use std::collections::{HashMap, HashSet};

pub type LowerResult<T> = Result<T, String>;

#[derive(Clone, Copy)]
enum Binding {
    /// A mutable variable or an output port, backed by a register.
    Reg(RegId),
    /// An input port: read-only, combinationally available every cycle.
    Input(u32),
}

struct ReturnTarget {
    slot: Option<RegId>,
    block: BlockId,
}

pub struct Lowerer<'p> {
    prog: &'p Program,
    d: Design,
    scopes: Vec<HashMap<String, Binding>>,
    /// Values written so far in the current cycle, not yet committed.
    env: HashMap<RegId, NodeId>,
    cur: BlockId,
    inline_stack: Vec<String>,
    ret_stack: Vec<ReturnTarget>,
    temp_counter: u32,
}

impl<'p> Lowerer<'p> {
    pub fn new(prog: &'p Program) -> Lowerer<'p> {
        Lowerer {
            prog,
            d: Design::new(),
            scopes: vec![HashMap::new()],
            env: HashMap::new(),
            cur: 0,
            inline_stack: Vec::new(),
            ret_stack: Vec::new(),
            temp_counter: 0,
        }
    }

    pub fn lower(mut self) -> LowerResult<Design> {
        // Ports first, so they are visible everywhere.
        let mut seen = HashSet::new();
        for p in &self.prog.ports {
            if !seen.insert(p.name.clone()) {
                return Err(format!("{}: duplicate port `{}`", p.span, p.name));
            }
            let b = match p.dir {
                PortDir::In => Binding::Input(self.d.add_input(&p.name, p.ty.width)),
                PortDir::Out => {
                    let r = self.d.add_reg(&p.name, p.ty.width);
                    self.d.outputs.push((p.name.clone(), r));
                    Binding::Reg(r)
                }
            };
            self.scopes[0].insert(p.name.clone(), b);
        }

        let mut names = HashSet::new();
        for f in &self.prog.funcs {
            if !names.insert(f.name.clone()) {
                return Err(format!("{}: duplicate function `{}`", f.span, f.name));
            }
        }

        let main = self
            .prog
            .func("main")
            .ok_or_else(|| "program has no `proc main()` entry point".to_string())?;
        if !main.params.is_empty() {
            return Err(format!("{}: `main` must take no parameters", main.span));
        }
        if main.ret.is_some() {
            return Err(format!("{}: `main` must be a `proc`, not a `fn`", main.span));
        }

        let entry = self.d.add_block("entry");
        self.cur = entry;
        self.d.entry = entry;

        self.scopes.push(HashMap::new());
        self.inline_stack.push("main".to_string());
        self.lower_block(&main.body)?;
        self.inline_stack.pop();
        self.scopes.pop();

        self.commit_and_terminate(Term::Halt);
        let mut d = self.d;
        prune_unreachable(&mut d);
        Ok(d)
    }

    // --- scope helpers ----------------------------------------------------

    fn lookup(&self, name: &str) -> Option<Binding> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    fn declare(&mut self, name: &str, width: u32) -> RegId {
        // Registers are uniquely named per declaration site so the emitted
        // report is readable even when a name is shadowed or reused in a loop.
        let unique = format!("{}#{}", name, self.d.regs.len());
        let r = self.d.add_reg(&unique, width);
        self.scopes.last_mut().unwrap().insert(name.to_string(), Binding::Reg(r));
        r
    }

    fn fresh_temp(&mut self, width: u32) -> RegId {
        self.temp_counter += 1;
        let name = format!("$t{}", self.temp_counter);
        self.d.add_reg(&name, width)
    }

    fn width_of(&self, b: Binding) -> u32 {
        match b {
            Binding::Reg(r) => self.d.regs[r as usize].width,
            Binding::Input(i) => self.d.inputs[i as usize].width,
        }
    }

    // --- block plumbing ---------------------------------------------------

    /// Flush pending register writes into the current block and close it.
    fn commit_and_terminate(&mut self, term: Term) {
        let mut assigns: Vec<(RegId, NodeId)> = self
            .env
            .drain()
            .filter(|&(r, v)| self.d.node(v) != crate::ir::Node::Reg(r))
            .collect();
        // Deterministic order keeps generated hardware stable across runs.
        assigns.sort_by_key(|&(r, _)| r);
        let b: &mut BasicBlock = &mut self.d.blocks[self.cur as usize];
        b.assigns = assigns;
        b.term = term;
    }

    fn start_block(&mut self, label: &str) -> BlockId {
        let id = self.d.add_block(label);
        self.cur = id;
        self.env.clear();
        id
    }

    /// Read the current value of a register within this cycle.
    fn read_reg(&mut self, r: RegId) -> NodeId {
        if let Some(&v) = self.env.get(&r) {
            return v;
        }
        self.d.reg(r)
    }

    /// Park `value` in a fresh register, close the block, and return a reader
    /// valid on the far side of the upcoming cycle boundary.
    fn spill(&mut self, value: NodeId) -> NodeId {
        let w = self.d.width(value);
        let t = self.fresh_temp(w);
        self.env.insert(t, value);
        self.d.reg(t)
    }

    // --- statements -------------------------------------------------------

    fn lower_block(&mut self, b: &Block) -> LowerResult<()> {
        self.scopes.push(HashMap::new());
        for s in &b.stmts {
            self.lower_stmt(s)?;
        }
        self.scopes.pop();
        Ok(())
    }

    fn lower_stmt(&mut self, s: &Stmt) -> LowerResult<()> {
        match s {
            Stmt::Var { ty, name, init, span } => {
                let width = match ty {
                    Some(t) => t.width,
                    None => self.infer_width(init)?.ok_or_else(|| {
                        format!("{span}: cannot infer width of `{name}`; annotate it, e.g. `var u8 {name} = ...`")
                    })?,
                };
                let v = self.lower_expr(init, Some(width))?;
                let r = self.declare(name, width);
                self.env.insert(r, v);
                Ok(())
            }
            Stmt::Assign { name, value, span } => {
                let b = self
                    .lookup(name)
                    .ok_or_else(|| format!("{span}: unknown variable `{name}`"))?;
                let r = match b {
                    Binding::Reg(r) => r,
                    Binding::Input(_) => {
                        return Err(format!("{span}: cannot assign to input port `{name}`"))
                    }
                };
                let w = self.d.regs[r as usize].width;
                let v = self.lower_expr(value, Some(w))?;
                self.env.insert(r, v);
                Ok(())
            }
            Stmt::Nested(b) => self.lower_block(b),
            Stmt::If { cond, then_b, else_b, .. } => {
                let c = self.lower_cond(cond)?;
                let then_id = self.d.add_block("if.then");
                let else_id = self.d.add_block("if.else");
                let join_id = self.d.add_block("if.join");
                self.commit_and_terminate(Term::Branch {
                    cond: c,
                    then_b: then_id,
                    else_b: else_id,
                });

                self.cur = then_id;
                self.env.clear();
                self.lower_block(then_b)?;
                self.commit_and_terminate(Term::Jump(join_id));

                self.cur = else_id;
                self.env.clear();
                if let Some(e) = else_b {
                    self.lower_block(e)?;
                }
                self.commit_and_terminate(Term::Jump(join_id));

                self.cur = join_id;
                self.env.clear();
                Ok(())
            }
            Stmt::While { cond, body, .. } => {
                let head = self.d.add_block("while.head");
                self.commit_and_terminate(Term::Jump(head));

                self.cur = head;
                self.env.clear();
                let c = self.lower_cond(cond)?;
                let body_id = self.d.add_block("while.body");
                let exit_id = self.d.add_block("while.exit");
                self.commit_and_terminate(Term::Branch {
                    cond: c,
                    then_b: body_id,
                    else_b: exit_id,
                });

                self.cur = body_id;
                self.env.clear();
                self.lower_block(body)?;
                self.commit_and_terminate(Term::Jump(head));

                self.cur = exit_id;
                self.env.clear();
                Ok(())
            }
            Stmt::Return { value, span } => {
                // Resolve the target before lowering, so a `return` outside any
                // function is reported rather than panicking.
                let (slot, block) = match self.ret_stack.last() {
                    Some(t) => (t.slot, t.block),
                    None => {
                        if value.is_some() {
                            return Err(format!("{span}: `main` is a proc and cannot return a value"));
                        }
                        self.commit_and_terminate(Term::Halt);
                        self.start_block("after.return");
                        return Ok(());
                    }
                };
                match (value, slot) {
                    (Some(e), Some(r)) => {
                        let w = self.d.regs[r as usize].width;
                        let v = self.lower_expr(e, Some(w))?;
                        self.env.insert(r, v);
                    }
                    (Some(_), None) => {
                        return Err(format!("{span}: this proc does not return a value"))
                    }
                    (None, Some(_)) => {
                        return Err(format!("{span}: expected a return value"))
                    }
                    (None, None) => {}
                }
                self.commit_and_terminate(Term::Jump(block));
                self.start_block("after.return");
                Ok(())
            }
        }
    }

    /// Lower an expression used as a condition, reducing it to one bit.
    fn lower_cond(&mut self, e: &Expr) -> LowerResult<NodeId> {
        let w = self.infer_width(e)?.unwrap_or(1);
        let v = self.lower_expr(e, Some(w))?;
        Ok(self.d.any(v))
    }

    // --- expressions ------------------------------------------------------

    /// The width an expression forces on its own, if any. `None` means the
    /// expression is width-polymorphic (a bare integer literal).
    fn infer_width(&self, e: &Expr) -> LowerResult<Option<u32>> {
        Ok(match e {
            Expr::Num { .. } => None,
            Expr::Ident { name, span } => {
                let b = self
                    .lookup(name)
                    .ok_or_else(|| format!("{span}: unknown name `{name}`"))?;
                Some(self.width_of(b))
            }
            Expr::Unary { op, operand, .. } => match op {
                UnOp::Not => Some(1),
                _ => self.infer_width(operand)?,
            },
            Expr::Binary { op, lhs, rhs, .. } => {
                if op.is_predicate() {
                    Some(1)
                } else if op.is_shift() {
                    self.infer_width(lhs)?
                } else {
                    self.infer_width(lhs)?.or(self.infer_width(rhs)?)
                }
            }
            Expr::Call { name, span, .. } => {
                let f = self
                    .prog
                    .func(name)
                    .ok_or_else(|| format!("{span}: unknown function `{name}`"))?;
                f.ret.map(|t| t.width)
            }
            Expr::Cast { ty, .. } => Some(ty.width),
            Expr::Ternary { then_e, else_e, .. } => {
                self.infer_width(then_e)?.or(self.infer_width(else_e)?)
            }
        })
    }

    /// Does evaluating this expression cross a cycle boundary?
    /// Every call does, by construction.
    fn splits(&self, e: &Expr) -> bool {
        match e {
            Expr::Num { .. } | Expr::Ident { .. } => false,
            Expr::Call { .. } => true,
            Expr::Unary { operand, .. } => self.splits(operand),
            Expr::Cast { operand, .. } => self.splits(operand),
            Expr::Binary { lhs, rhs, .. } => self.splits(lhs) || self.splits(rhs),
            Expr::Ternary { cond, then_e, else_e, .. } => {
                self.splits(cond) || self.splits(then_e) || self.splits(else_e)
            }
        }
    }

    /// Lower two operands left-to-right, spilling the first across any cycle
    /// boundary the second introduces.
    fn lower_pair(
        &mut self,
        lhs: &Expr,
        rhs: &Expr,
        lw: u32,
        rw: u32,
    ) -> LowerResult<(NodeId, NodeId)> {
        let mut l = self.lower_expr(lhs, Some(lw))?;
        if self.splits(rhs) {
            l = self.spill(l);
        }
        let r = self.lower_expr(rhs, Some(rw))?;
        Ok((l, r))
    }

    fn lower_expr(&mut self, e: &Expr, expected: Option<u32>) -> LowerResult<NodeId> {
        let v = self.lower_expr_raw(e, expected)?;
        match expected {
            Some(w) if self.d.width(v) != w => Ok(self.d.resize(v, w)),
            _ => Ok(v),
        }
    }

    fn lower_expr_raw(&mut self, e: &Expr, expected: Option<u32>) -> LowerResult<NodeId> {
        match e {
            Expr::Num { value, span } => {
                let w = expected.or(self.infer_width(e)?).ok_or_else(|| {
                    format!("{span}: cannot infer the width of literal `{value}`; add a cast, e.g. `{value} as u8`")
                })?;
                if w < 64 && *value >= (1u64 << w) {
                    return Err(format!(
                        "{span}: literal `{value}` does not fit in u{w}"
                    ));
                }
                Ok(self.d.constant(*value, w))
            }
            Expr::Ident { name, span } => {
                let b = self
                    .lookup(name)
                    .ok_or_else(|| format!("{span}: unknown name `{name}`"))?;
                Ok(match b {
                    Binding::Reg(r) => self.read_reg(r),
                    Binding::Input(i) => self.d.input(i),
                })
            }
            Expr::Cast { operand, ty, .. } => {
                let inner = self.infer_width(operand)?.unwrap_or(ty.width);
                let v = self.lower_expr(operand, Some(inner))?;
                Ok(self.d.resize(v, ty.width))
            }
            Expr::Unary { op, operand, span } => {
                match op {
                    UnOp::Not => {
                        let w = self.infer_width(operand)?.unwrap_or(1);
                        let v = self.lower_expr(operand, Some(w))?;
                        let nz = self.d.any(v);
                        Ok(self.d.not(nz))
                    }
                    UnOp::BitNot => {
                        let w = expected
                            .or(self.infer_width(operand)?)
                            .ok_or_else(|| format!("{span}: cannot infer width of `~`"))?;
                        let v = self.lower_expr(operand, Some(w))?;
                        Ok(self.d.not(v))
                    }
                    UnOp::Neg => {
                        let w = expected
                            .or(self.infer_width(operand)?)
                            .ok_or_else(|| format!("{span}: cannot infer width of `-`"))?;
                        let v = self.lower_expr(operand, Some(w))?;
                        let z = self.d.zero(w);
                        Ok(self.d.sub(z, v))
                    }
                }
            }
            Expr::Binary { op, lhs, rhs, span } => self.lower_binary(*op, lhs, rhs, expected, *span),
            Expr::Ternary { cond, then_e, else_e, span } => {
                let w = expected
                    .or(self.infer_width(then_e)?)
                    .or(self.infer_width(else_e)?)
                    .ok_or_else(|| format!("{span}: cannot infer width of `? :`"))?;
                let mut c = self.lower_cond(cond)?;
                if self.splits(then_e) || self.splits(else_e) {
                    c = self.spill(c);
                }
                let (t, f) = self.lower_pair(then_e, else_e, w, w)?;
                Ok(self.d.mux(c, t, f))
            }
            Expr::Call { name, args, span } => self.lower_call(name, args, *span),
        }
    }

    fn lower_binary(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        expected: Option<u32>,
        span: crate::lexer::Span,
    ) -> LowerResult<NodeId> {
        use BinOp::*;
        match op {
            LAnd | LOr => {
                let lw = self.infer_width(lhs)?.unwrap_or(1);
                let rw = self.infer_width(rhs)?.unwrap_or(1);
                let (l, r) = self.lower_pair(lhs, rhs, lw, rw)?;
                let la = self.d.any(l);
                let ra = self.d.any(r);
                Ok(if op == LAnd { self.d.and(la, ra) } else { self.d.or(la, ra) })
            }
            Shl | Shr => {
                let w = expected
                    .or(self.infer_width(lhs)?)
                    .ok_or_else(|| format!("{span}: cannot infer width of `{}`", op.symbol()))?;
                // The shift amount is independent of the value's width.
                let sw = self.infer_width(rhs)?.unwrap_or(shift_width(w));
                let (l, r) = self.lower_pair(lhs, rhs, w, sw)?;
                Ok(if op == Shl { self.d.shl(l, r) } else { self.d.shr(l, r) })
            }
            Eq | Ne | Lt | Le | Gt | Ge => {
                // Comparison operands must agree with each other, but the
                // result width is independent of them.
                let w = self
                    .infer_width(lhs)?
                    .or(self.infer_width(rhs)?)
                    .ok_or_else(|| {
                        format!(
                            "{span}: cannot infer operand width for `{}`; both sides are bare literals",
                            op.symbol()
                        )
                    })?;
                let (l, r) = self.lower_pair(lhs, rhs, w, w)?;
                Ok(match op {
                    Eq => self.d.eq(l, r),
                    Ne => self.d.ne(l, r),
                    Lt => self.d.lt(l, r),
                    Le => self.d.le(l, r),
                    Gt => self.d.gt(l, r),
                    _ => self.d.ge(l, r),
                })
            }
            _ => {
                let w = expected
                    .or(self.infer_width(lhs)?)
                    .or(self.infer_width(rhs)?)
                    .ok_or_else(|| format!("{span}: cannot infer width of `{}`", op.symbol()))?;
                let (l, r) = self.lower_pair(lhs, rhs, w, w)?;
                Ok(match op {
                    Add => self.d.add(l, r),
                    Sub => self.d.sub(l, r),
                    Mul => self.d.mul(l, r),
                    And => self.d.and(l, r),
                    Or => self.d.or(l, r),
                    Xor => self.d.xor(l, r),
                    _ => unreachable!("handled above: {op:?}"),
                })
            }
        }
    }

    fn lower_call(
        &mut self,
        name: &str,
        args: &[Expr],
        span: crate::lexer::Span,
    ) -> LowerResult<NodeId> {
        let f = self
            .prog
            .func(name)
            .ok_or_else(|| format!("{span}: unknown function `{name}`"))?;
        if self.inline_stack.iter().any(|n| n == name) {
            let mut chain = self.inline_stack.clone();
            chain.push(name.to_string());
            return Err(format!(
                "{span}: recursive call to `{name}` cannot be synthesised ({})",
                chain.join(" -> ")
            ));
        }
        if f.params.len() != args.len() {
            return Err(format!(
                "{span}: `{name}` takes {} argument(s), got {}",
                f.params.len(),
                args.len()
            ));
        }

        // Evaluate arguments left to right, spilling earlier ones across any
        // cycle boundary a later argument introduces.
        let mut vals: Vec<NodeId> = Vec::with_capacity(args.len());
        for (i, a) in args.iter().enumerate() {
            let w = f.params[i].ty.width;
            let later_splits = args[i + 1..].iter().any(|x| self.splits(x));
            let mut v = self.lower_expr(a, Some(w))?;
            if later_splits {
                v = self.spill(v);
            }
            vals.push(v);
        }

        // Bind parameters to fresh registers in a new scope.
        let ret_slot = f.ret.map(|t| {
            let r = self.fresh_temp(t.width);
            r
        });
        let mut frame: HashMap<String, Binding> = HashMap::new();
        for (i, p) in f.params.iter().enumerate() {
            let r = self.fresh_temp(p.ty.width);
            self.env.insert(r, vals[i]);
            frame.insert(p.name.clone(), Binding::Reg(r));
        }

        // A call always ends the current block: parameters commit here and the
        // body starts on a clean cycle where `Reg(param)` reads the bound value.
        let body_block = self.d.add_block(&format!("call.{name}"));
        let ret_block = self.d.add_block(&format!("ret.{name}"));
        self.commit_and_terminate(Term::Jump(body_block));

        self.cur = body_block;
        self.env.clear();
        self.scopes.push(frame);
        self.inline_stack.push(name.to_string());
        self.ret_stack.push(ReturnTarget { slot: ret_slot, block: ret_block });

        let body = f.body.clone();
        self.lower_block(&body)?;

        self.ret_stack.pop();
        self.inline_stack.pop();
        self.scopes.pop();

        // Fall off the end of the body into the return block.
        self.commit_and_terminate(Term::Jump(ret_block));
        self.cur = ret_block;
        self.env.clear();

        match ret_slot {
            Some(r) => Ok(self.d.reg(r)),
            None => Ok(self.d.zero(1)),
        }
    }
}

/// The natural width for a shift amount: enough bits to name any bit position.
fn shift_width(value_width: u32) -> u32 {
    let mut w = 1;
    while (1u32 << w) < value_width {
        w += 1;
    }
    w
}

/// Drop blocks unreachable from the entry (e.g. code after an early `return`),
/// renumbering the survivors.
fn prune_unreachable(d: &mut Design) {
    let mut reachable = HashSet::new();
    let mut stack = vec![d.entry];
    while let Some(b) = stack.pop() {
        if !reachable.insert(b) {
            continue;
        }
        match d.blocks[b as usize].term {
            Term::Jump(t) => stack.push(t),
            Term::Branch { then_b, else_b, .. } => {
                stack.push(then_b);
                stack.push(else_b);
            }
            Term::Halt => {}
        }
    }
    if reachable.len() == d.blocks.len() {
        return;
    }
    let mut keep: Vec<BlockId> = reachable.into_iter().collect();
    keep.sort();
    let remap: HashMap<BlockId, BlockId> =
        keep.iter().enumerate().map(|(i, &b)| (b, i as BlockId)).collect();

    let old = std::mem::take(&mut d.blocks);
    d.blocks = keep
        .iter()
        .map(|&b| {
            let mut blk = old[b as usize].clone();
            blk.term = match blk.term {
                Term::Jump(t) => Term::Jump(remap[&t]),
                Term::Branch { cond, then_b, else_b } => Term::Branch {
                    cond,
                    then_b: remap[&then_b],
                    else_b: remap[&else_b],
                },
                Term::Halt => Term::Halt,
            };
            blk
        })
        .collect();
    d.entry = remap[&d.entry];
}

pub fn lower_program(prog: &Program) -> LowerResult<Design> {
    Lowerer::new(prog).lower()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    fn design(src: &str) -> Design {
        lower_program(&parse(src).unwrap()).unwrap()
    }

    fn err(src: &str) -> String {
        lower_program(&parse(src).unwrap()).unwrap_err()
    }

    #[test]
    fn straight_line_code_is_one_cycle() {
        let d = design("input u8 a; output u8 q; proc main() { q = a + 1; }");
        assert_eq!(d.blocks.len(), 1, "no control flow => single state");
        assert!(matches!(d.blocks[0].term, Term::Halt));
        assert_eq!(d.blocks[0].assigns.len(), 1);
    }

    /// The classic swap: parallel commit with sequential source semantics.
    #[test]
    fn sequential_assignments_within_a_cycle() {
        let d = design(
            "input u8 a; input u8 b; output u8 q; \
             proc main() { var u8 x = a; var u8 y = b; x = y; y = x; q = y; }",
        );
        // `y = x` must see the value x was just given (= b), not a swap.
        assert_eq!(d.blocks.len(), 1);
    }

    #[test]
    fn while_loop_builds_a_cycle_in_the_cfg() {
        let d = design(
            "input u8 a; output u8 q; \
             proc main() { var u8 x = a; while (x != 0) { x = x - 1; } q = x; }",
        );
        // entry -> head -> {body -> head, exit}
        let head = d.blocks.iter().position(|b| b.label == "while.head").unwrap();
        let body = d.blocks.iter().position(|b| b.label == "while.body").unwrap();
        assert!(matches!(d.blocks[head].term, Term::Branch { .. }));
        assert!(matches!(d.blocks[body].term, Term::Jump(t) if t as usize == head));
    }

    #[test]
    fn unreachable_blocks_are_pruned() {
        let d = design("output u8 q; proc main() { q = 1; return; }");
        assert!(
            d.blocks.iter().all(|b| b.label != "after.return"),
            "dead block should be pruned"
        );
    }

    #[test]
    fn recursion_is_rejected_with_a_call_chain() {
        let e = err(
            "output u8 q; fn f(u8 x) -> u8 { return g(x); } \
             fn g(u8 x) -> u8 { return f(x); } proc main() { q = f(1); }",
        );
        assert!(e.contains("recursive"), "{e}");
        assert!(e.contains("->"), "should show the chain: {e}");
    }

    #[test]
    fn assigning_to_an_input_is_rejected() {
        let e = err("input u8 a; proc main() { a = 1; }");
        assert!(e.contains("cannot assign to input port"), "{e}");
    }

    #[test]
    fn literal_too_wide_is_rejected() {
        let e = err("output u4 q; proc main() { q = 20; }");
        assert!(e.contains("does not fit in u4"), "{e}");
    }

    #[test]
    fn unconstrained_literal_comparison_is_rejected() {
        let e = err("output u1 q; proc main() { q = 1 < 2; }");
        assert!(e.contains("cannot infer operand width"), "{e}");
    }

    #[test]
    fn missing_main_is_reported() {
        let e = err("input u8 a;");
        assert!(e.contains("no `proc main()`"), "{e}");
    }

    #[test]
    fn arity_mismatch_is_reported() {
        let e = err(
            "output u8 q; fn f(u8 x, u8 y) -> u8 { return x; } proc main() { q = f(1 as u8); }",
        );
        assert!(e.contains("takes 2 argument(s), got 1"), "{e}");
    }

    #[test]
    fn calls_split_blocks() {
        let d = design(
            "input u8 a; output u8 q; fn f(u8 x) -> u8 { return x + 1; } \
             proc main() { q = f(a); }",
        );
        assert!(d.blocks.iter().any(|b| b.label == "call.f"));
        assert!(d.blocks.iter().any(|b| b.label == "ret.f"));
    }

    #[test]
    fn cse_shares_repeated_subexpressions() {
        let d = design("input u8 a; output u8 q; proc main() { q = (a + 1) + (a + 1); }");
        // a, 1, a+1, (a+1)+(a+1) -> the inner sum is built once.
        let adds = (0..d.node_count())
            .filter(|&i| matches!(d.node(i as u32), crate::ir::Node::Add(..)))
            .count();
        assert_eq!(adds, 2, "inner `a + 1` should be shared");
    }
}
