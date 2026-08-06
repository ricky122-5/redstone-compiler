//! Recursive-descent parser with precedence climbing for binary operators.

use crate::ast::*;
use crate::lexer::{Lexer, Span, Tok, Token};

pub type ParseResult<T> = Result<T, String>;

pub struct Parser {
    toks: Vec<Token>,
    pos: usize,
}

/// Binding powers, loosest first. Mirrors C, which is what the syntax leads
/// people to expect.
fn binop_of(t: &Tok) -> Option<(BinOp, u8)> {
    Some(match t {
        Tok::OrOr => (BinOp::LOr, 1),
        Tok::AndAnd => (BinOp::LAnd, 2),
        Tok::Pipe => (BinOp::Or, 3),
        Tok::Caret => (BinOp::Xor, 4),
        Tok::Amp => (BinOp::And, 5),
        Tok::EqEq => (BinOp::Eq, 6),
        Tok::Ne => (BinOp::Ne, 6),
        Tok::Lt => (BinOp::Lt, 7),
        Tok::Le => (BinOp::Le, 7),
        Tok::Gt => (BinOp::Gt, 7),
        Tok::Ge => (BinOp::Ge, 7),
        Tok::Shl => (BinOp::Shl, 8),
        Tok::Shr => (BinOp::Shr, 8),
        Tok::Plus => (BinOp::Add, 9),
        Tok::Minus => (BinOp::Sub, 9),
        Tok::Star => (BinOp::Mul, 10),
        _ => return None,
    })
}

impl Parser {
    pub fn new(src: &str) -> ParseResult<Parser> {
        Ok(Parser { toks: Lexer::new(src).tokenize()?, pos: 0 })
    }

    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }

    fn span(&self) -> Span {
        self.toks[self.pos].span
    }

    fn bump(&mut self) -> Tok {
        let t = self.toks[self.pos].tok.clone();
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        t
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == t {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, t: &Tok) -> ParseResult<()> {
        if self.eat(t) {
            Ok(())
        } else {
            Err(format!("{}: expected `{}`, found `{}`", self.span(), t, self.peek()))
        }
    }

    fn expect_ident(&mut self) -> ParseResult<String> {
        match self.bump() {
            Tok::Ident(s) => Ok(s),
            other => Err(format!("{}: expected identifier, found `{}`", self.span(), other)),
        }
    }

    fn expect_type(&mut self) -> ParseResult<Type> {
        let span = self.span();
        match self.bump() {
            Tok::UType(w) => {
                if w == 0 || w > 32 {
                    Err(format!("{span}: width u{w} out of range (expected u1..u32)"))
                } else {
                    Ok(Type::new(w))
                }
            }
            other => Err(format!("{span}: expected a type like `u8`, found `{other}`")),
        }
    }

    pub fn parse_program(&mut self) -> ParseResult<Program> {
        let mut prog = Program::default();
        loop {
            match self.peek() {
                Tok::Eof => break,
                Tok::Input | Tok::Output => {
                    let dir =
                        if matches!(self.bump(), Tok::Input) { PortDir::In } else { PortDir::Out };
                    let span = self.span();
                    let ty = self.expect_type()?;
                    let name = self.expect_ident()?;
                    self.expect(&Tok::Semi)?;
                    prog.ports.push(Port { dir, ty, name, span });
                }
                Tok::Fn | Tok::Proc => {
                    let is_proc = matches!(self.bump(), Tok::Proc);
                    prog.funcs.push(self.parse_func(is_proc)?);
                }
                other => {
                    return Err(format!(
                        "{}: expected `input`, `output`, `fn`, or `proc`, found `{}`",
                        self.span(),
                        other
                    ))
                }
            }
        }
        Ok(prog)
    }

    fn parse_func(&mut self, is_proc: bool) -> ParseResult<Func> {
        let span = self.span();
        let name = self.expect_ident()?;
        self.expect(&Tok::LParen)?;
        let mut params = Vec::new();
        if !self.eat(&Tok::RParen) {
            loop {
                let pspan = self.span();
                let ty = self.expect_type()?;
                let pname = self.expect_ident()?;
                params.push(Param { ty, name: pname, span: pspan });
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::RParen)?;
        }
        let ret = if self.eat(&Tok::Arrow) {
            if is_proc {
                return Err(format!("{span}: `proc {name}` cannot declare a return type; use `fn`"));
            }
            Some(self.expect_type()?)
        } else {
            None
        };
        if !is_proc && ret.is_none() {
            return Err(format!("{span}: `fn {name}` must declare a return type; use `proc` for none"));
        }
        let body = self.parse_block()?;
        Ok(Func { name, params, ret, body, span })
    }

    fn parse_block(&mut self) -> ParseResult<Block> {
        self.expect(&Tok::LBrace)?;
        let mut stmts = Vec::new();
        while !self.eat(&Tok::RBrace) {
            if matches!(self.peek(), Tok::Eof) {
                return Err(format!("{}: unclosed block at end of file", self.span()));
            }
            stmts.push(self.parse_stmt()?);
        }
        Ok(Block { stmts })
    }

    fn parse_stmt(&mut self) -> ParseResult<Stmt> {
        let span = self.span();
        match self.peek().clone() {
            Tok::Var => {
                self.bump();
                // `var u8 x = e;` or `var x = e;`
                let ty = if matches!(self.peek(), Tok::UType(_)) {
                    Some(self.expect_type()?)
                } else {
                    None
                };
                let name = self.expect_ident()?;
                self.expect(&Tok::Assign)?;
                let init = self.parse_expr()?;
                self.expect(&Tok::Semi)?;
                Ok(Stmt::Var { ty, name, init, span })
            }
            Tok::If => {
                self.bump();
                self.expect(&Tok::LParen)?;
                let cond = self.parse_expr()?;
                self.expect(&Tok::RParen)?;
                let then_b = self.parse_block()?;
                let else_b = if self.eat(&Tok::Else) {
                    // `else if` chains without requiring braces around the `if`.
                    if matches!(self.peek(), Tok::If) {
                        Some(Block { stmts: vec![self.parse_stmt()?] })
                    } else {
                        Some(self.parse_block()?)
                    }
                } else {
                    None
                };
                Ok(Stmt::If { cond, then_b, else_b, span })
            }
            Tok::While => {
                self.bump();
                self.expect(&Tok::LParen)?;
                let cond = self.parse_expr()?;
                self.expect(&Tok::RParen)?;
                let body = self.parse_block()?;
                Ok(Stmt::While { cond, body, span })
            }
            Tok::Return => {
                self.bump();
                let value = if matches!(self.peek(), Tok::Semi) {
                    None
                } else {
                    Some(self.parse_expr()?)
                };
                self.expect(&Tok::Semi)?;
                Ok(Stmt::Return { value, span })
            }
            Tok::LBrace => Ok(Stmt::Nested(self.parse_block()?)),
            Tok::Ident(name) => {
                self.bump();
                self.expect(&Tok::Assign).map_err(|_| {
                    format!("{span}: expected `=` after `{name}`; Ohm has no expression statements")
                })?;
                let value = self.parse_expr()?;
                self.expect(&Tok::Semi)?;
                Ok(Stmt::Assign { name, value, span })
            }
            other => Err(format!("{span}: expected a statement, found `{other}`")),
        }
    }

    pub fn parse_expr(&mut self) -> ParseResult<Expr> {
        self.parse_ternary()
    }

    fn parse_ternary(&mut self) -> ParseResult<Expr> {
        let cond = self.parse_binary(1)?;
        if self.eat(&Tok::Question) {
            let span = cond.span();
            let then_e = self.parse_expr()?;
            self.expect(&Tok::Colon)?;
            // Right-associative, so the else branch re-enters at ternary level.
            let else_e = self.parse_ternary()?;
            return Ok(Expr::Ternary {
                cond: Box::new(cond),
                then_e: Box::new(then_e),
                else_e: Box::new(else_e),
                span,
            });
        }
        Ok(cond)
    }

    fn parse_binary(&mut self, min_bp: u8) -> ParseResult<Expr> {
        let mut lhs = self.parse_unary()?;
        while let Some((op, bp)) = binop_of(self.peek()) {
            if bp < min_bp {
                break;
            }
            let span = self.span();
            self.bump();
            // All our binary operators are left-associative.
            let rhs = self.parse_binary(bp + 1)?;
            lhs = Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs), span };
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> ParseResult<Expr> {
        let span = self.span();
        let op = match self.peek() {
            Tok::Minus => Some(UnOp::Neg),
            Tok::Bang => Some(UnOp::Not),
            Tok::Tilde => Some(UnOp::BitNot),
            _ => None,
        };
        if let Some(op) = op {
            self.bump();
            let operand = self.parse_unary()?;
            return self.parse_casts(Expr::Unary { op, operand: Box::new(operand), span });
        }
        let atom = self.parse_atom()?;
        self.parse_casts(atom)
    }

    fn parse_casts(&mut self, mut e: Expr) -> ParseResult<Expr> {
        while self.eat(&Tok::As) {
            let span = self.span();
            let ty = self.expect_type()?;
            e = Expr::Cast { operand: Box::new(e), ty, span };
        }
        Ok(e)
    }

    fn parse_atom(&mut self) -> ParseResult<Expr> {
        let span = self.span();
        match self.bump() {
            Tok::Num(value) => Ok(Expr::Num { value, span }),
            Tok::True => Ok(Expr::Num { value: 1, span }),
            Tok::False => Ok(Expr::Num { value: 0, span }),
            Tok::LParen => {
                let e = self.parse_expr()?;
                self.expect(&Tok::RParen)?;
                Ok(e)
            }
            Tok::Ident(name) => {
                if self.eat(&Tok::LParen) {
                    let mut args = Vec::new();
                    if !self.eat(&Tok::RParen) {
                        loop {
                            args.push(self.parse_expr()?);
                            if !self.eat(&Tok::Comma) {
                                break;
                            }
                        }
                        self.expect(&Tok::RParen)?;
                    }
                    Ok(Expr::Call { name, args, span })
                } else {
                    Ok(Expr::Ident { name, span })
                }
            }
            other => Err(format!("{span}: expected an expression, found `{other}`")),
        }
    }
}

pub fn parse(src: &str) -> ParseResult<Program> {
    Parser::new(src)?.parse_program()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expr(src: &str) -> Expr {
        Parser::new(src).unwrap().parse_expr().unwrap()
    }

    /// Render an expression fully parenthesised so precedence is visible.
    fn show(e: &Expr) -> String {
        match e {
            Expr::Num { value, .. } => value.to_string(),
            Expr::Ident { name, .. } => name.clone(),
            Expr::Unary { op, operand, .. } => {
                let s = match op {
                    UnOp::Neg => "-",
                    UnOp::Not => "!",
                    UnOp::BitNot => "~",
                };
                format!("({s}{})", show(operand))
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                format!("({} {} {})", show(lhs), op.symbol(), show(rhs))
            }
            Expr::Call { name, args, .. } => {
                let a: Vec<String> = args.iter().map(show).collect();
                format!("{name}({})", a.join(","))
            }
            Expr::Cast { operand, ty, .. } => format!("({} as {ty})", show(operand)),
            Expr::Ternary { cond, then_e, else_e, .. } => {
                format!("({} ? {} : {})", show(cond), show(then_e), show(else_e))
            }
        }
    }

    #[test]
    fn arithmetic_precedence() {
        assert_eq!(show(&expr("1 + 2 * 3")), "(1 + (2 * 3))");
        assert_eq!(show(&expr("1 * 2 + 3")), "((1 * 2) + 3)");
        assert_eq!(show(&expr("1 - 2 - 3")), "((1 - 2) - 3)", "left associative");
    }

    #[test]
    fn bitwise_binds_looser_than_comparison() {
        // Unlike C's famous wart we keep C's actual precedence, so `&` is
        // looser than `==`. This test pins that down deliberately.
        assert_eq!(show(&expr("a & b == c")), "(a & (b == c))");
        assert_eq!(show(&expr("a && b || c")), "((a && b) || c)");
    }

    #[test]
    fn shifts_bind_tighter_than_comparison() {
        assert_eq!(show(&expr("a << 1 < b")), "((a << 1) < b)");
    }

    #[test]
    fn ternary_is_right_associative() {
        assert_eq!(show(&expr("a ? b : c ? d : e")), "(a ? b : (c ? d : e))");
    }

    #[test]
    fn casts_bind_tighter_than_binary_ops() {
        assert_eq!(show(&expr("a as u8 + b")), "((a as u8) + b)");
    }

    #[test]
    fn parses_a_whole_program() {
        let p = parse(
            r#"
            input u8 a;
            input u8 b;
            output u8 q;

            fn gcd(u8 x, u8 y) -> u8 {
                while (y != 0) {
                    if (x > y) { x = x - y; } else { y = y - x; }
                }
                return x;
            }

            proc main() {
                q = gcd(a, b);
            }
            "#,
        )
        .unwrap();
        assert_eq!(p.ports.len(), 3);
        assert_eq!(p.funcs.len(), 2);
        assert_eq!(p.func("gcd").unwrap().params.len(), 2);
        assert_eq!(p.func("gcd").unwrap().ret, Some(Type::new(8)));
        assert!(p.func("main").unwrap().ret.is_none());
    }

    #[test]
    fn else_if_chains_parse() {
        let p = parse("proc main() { if (a) { x = 1; } else if (b) { x = 2; } else { x = 3; } }")
            .unwrap();
        let Stmt::If { else_b: Some(e), .. } = &p.funcs[0].body.stmts[0] else {
            panic!("expected if/else");
        };
        assert!(matches!(e.stmts[0], Stmt::If { .. }));
    }

    #[test]
    fn fn_without_return_type_is_rejected() {
        let err = parse("fn f() { return 1; }").unwrap_err();
        assert!(err.contains("must declare a return type"), "{err}");
    }

    #[test]
    fn proc_with_return_type_is_rejected() {
        let err = parse("proc f() -> u8 { return 1; }").unwrap_err();
        assert!(err.contains("cannot declare a return type"), "{err}");
    }

    #[test]
    fn width_bounds_are_enforced() {
        assert!(parse("input u64 a;").unwrap_err().contains("out of range"));
        assert!(parse("input u0 a;").unwrap_err().contains("out of range"));
    }
}
