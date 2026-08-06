//! Ohm abstract syntax tree.

use crate::lexer::Span;

/// Every value in Ohm is an unsigned integer of an explicit bit width.
/// `bool` is spelled `u1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Type {
    pub width: u32,
}

impl Type {
    pub const BOOL: Type = Type { width: 1 };

    pub fn new(width: u32) -> Type {
        Type { width }
    }
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "u{}", self.width)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortDir {
    In,
    Out,
}

#[derive(Debug, Clone)]
pub struct Port {
    pub dir: PortDir,
    pub ty: Type,
    pub name: String,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub ty: Type,
    pub name: String,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Func {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone, Default)]
pub struct Block {
    pub stmts: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    /// `var u8 x = e;` - the type is optional and inferred when omitted.
    Var { ty: Option<Type>, name: String, init: Expr, span: Span },
    Assign { name: String, value: Expr, span: Span },
    If { cond: Expr, then_b: Block, else_b: Option<Block>, span: Span },
    While { cond: Expr, body: Block, span: Span },
    Return { value: Option<Expr>, span: Span },
    Nested(Block),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    And,
    Or,
    Xor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    LAnd,
    LOr,
}

impl BinOp {
    /// Comparisons and logical connectives always produce `u1`.
    pub fn is_predicate(self) -> bool {
        matches!(
            self,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
                | BinOp::LAnd | BinOp::LOr
        )
    }

    /// Shifts do not require both sides to share a width.
    pub fn is_shift(self) -> bool {
        matches!(self, BinOp::Shl | BinOp::Shr)
    }

    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::And => "&",
            BinOp::Or => "|",
            BinOp::Xor => "^",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::LAnd => "&&",
            BinOp::LOr => "||",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Expr {
    /// An integer literal. Width is unconstrained until type checking pins it
    /// down from context, which is what lets `x + 1` work for any width of `x`.
    Num { value: u64, span: Span },
    Ident { name: String, span: Span },
    Unary { op: UnOp, operand: Box<Expr>, span: Span },
    Binary { op: BinOp, lhs: Box<Expr>, rhs: Box<Expr>, span: Span },
    Call { name: String, args: Vec<Expr>, span: Span },
    Cast { operand: Box<Expr>, ty: Type, span: Span },
    Ternary { cond: Box<Expr>, then_e: Box<Expr>, else_e: Box<Expr>, span: Span },
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Num { span, .. }
            | Expr::Ident { span, .. }
            | Expr::Unary { span, .. }
            | Expr::Binary { span, .. }
            | Expr::Call { span, .. }
            | Expr::Cast { span, .. }
            | Expr::Ternary { span, .. } => *span,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Program {
    pub ports: Vec<Port>,
    pub funcs: Vec<Func>,
}

impl Program {
    pub fn func(&self, name: &str) -> Option<&Func> {
        self.funcs.iter().find(|f| f.name == name)
    }

    pub fn port(&self, name: &str) -> Option<&Port> {
        self.ports.iter().find(|p| p.name == name)
    }
}
