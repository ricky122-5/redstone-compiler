//! Hand-written lexer for Ohm.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub line: u32,
    pub col: u32,
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok {
    // literals & names
    Num(u64),
    Ident(String),
    /// `u8`, `u1`, ... - a width type.
    UType(u32),

    // keywords
    Input,
    Output,
    Fn,
    Proc,
    Var,
    If,
    Else,
    While,
    Return,
    As,
    True,
    False,

    // punctuation
    LParen,
    RParen,
    LBrace,
    RBrace,
    Semi,
    Comma,
    Arrow,
    Question,
    Colon,

    // operators
    Assign,
    Plus,
    Minus,
    Star,
    Amp,
    Pipe,
    Caret,
    Tilde,
    Bang,
    Shl,
    Shr,
    EqEq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    AndAnd,
    OrOr,

    Eof,
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Tok::Num(n) => return write!(f, "{n}"),
            Tok::Ident(i) => return write!(f, "{i}"),
            Tok::UType(w) => return write!(f, "u{w}"),
            Tok::Input => "input",
            Tok::Output => "output",
            Tok::Fn => "fn",
            Tok::Proc => "proc",
            Tok::Var => "var",
            Tok::If => "if",
            Tok::Else => "else",
            Tok::While => "while",
            Tok::Return => "return",
            Tok::As => "as",
            Tok::True => "true",
            Tok::False => "false",
            Tok::LParen => "(",
            Tok::RParen => ")",
            Tok::LBrace => "{",
            Tok::RBrace => "}",
            Tok::Semi => ";",
            Tok::Comma => ",",
            Tok::Arrow => "->",
            Tok::Question => "?",
            Tok::Colon => ":",
            Tok::Assign => "=",
            Tok::Plus => "+",
            Tok::Minus => "-",
            Tok::Star => "*",
            Tok::Amp => "&",
            Tok::Pipe => "|",
            Tok::Caret => "^",
            Tok::Tilde => "~",
            Tok::Bang => "!",
            Tok::Shl => "<<",
            Tok::Shr => ">>",
            Tok::EqEq => "==",
            Tok::Ne => "!=",
            Tok::Lt => "<",
            Tok::Le => "<=",
            Tok::Gt => ">",
            Tok::Ge => ">=",
            Tok::AndAnd => "&&",
            Tok::OrOr => "||",
            Tok::Eof => "<eof>",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

pub struct Lexer<'s> {
    src: &'s [u8],
    pos: usize,
    line: u32,
    col: u32,
}

pub type LexResult<T> = Result<T, String>;

impl<'s> Lexer<'s> {
    pub fn new(src: &'s str) -> Self {
        Lexer { src: src.as_bytes(), pos: 0, line: 1, col: 1 }
    }

    fn peek(&self) -> u8 {
        self.src.get(self.pos).copied().unwrap_or(0)
    }

    fn peek2(&self) -> u8 {
        self.src.get(self.pos + 1).copied().unwrap_or(0)
    }

    fn bump(&mut self) -> u8 {
        let c = self.peek();
        self.pos += 1;
        if c == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        c
    }

    fn span(&self) -> Span {
        Span { line: self.line, col: self.col }
    }

    fn skip_trivia(&mut self) -> LexResult<()> {
        loop {
            match self.peek() {
                b' ' | b'\t' | b'\r' | b'\n' => {
                    self.bump();
                }
                b'/' if self.peek2() == b'/' => {
                    while self.pos < self.src.len() && self.peek() != b'\n' {
                        self.bump();
                    }
                }
                b'/' if self.peek2() == b'*' => {
                    let open = self.span();
                    self.bump();
                    self.bump();
                    loop {
                        if self.pos >= self.src.len() {
                            return Err(format!("{open}: unterminated block comment"));
                        }
                        if self.peek() == b'*' && self.peek2() == b'/' {
                            self.bump();
                            self.bump();
                            break;
                        }
                        self.bump();
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    pub fn tokenize(mut self) -> LexResult<Vec<Token>> {
        let mut out = Vec::new();
        loop {
            self.skip_trivia()?;
            let span = self.span();
            if self.pos >= self.src.len() {
                out.push(Token { tok: Tok::Eof, span });
                return Ok(out);
            }
            let tok = self.next_token(span)?;
            out.push(Token { tok, span });
        }
    }

    fn next_token(&mut self, span: Span) -> LexResult<Tok> {
        let c = self.peek();
        if c.is_ascii_digit() {
            return self.number(span);
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            return Ok(self.word());
        }
        self.bump();
        let two = |l: &mut Self, t: Tok| {
            l.bump();
            t
        };
        Ok(match c {
            b'(' => Tok::LParen,
            b')' => Tok::RParen,
            b'{' => Tok::LBrace,
            b'}' => Tok::RBrace,
            b';' => Tok::Semi,
            b',' => Tok::Comma,
            b'?' => Tok::Question,
            b':' => Tok::Colon,
            b'+' => Tok::Plus,
            b'*' => Tok::Star,
            b'^' => Tok::Caret,
            b'~' => Tok::Tilde,
            b'-' if self.peek() == b'>' => two(self, Tok::Arrow),
            b'-' => Tok::Minus,
            b'&' if self.peek() == b'&' => two(self, Tok::AndAnd),
            b'&' => Tok::Amp,
            b'|' if self.peek() == b'|' => two(self, Tok::OrOr),
            b'|' => Tok::Pipe,
            b'=' if self.peek() == b'=' => two(self, Tok::EqEq),
            b'=' => Tok::Assign,
            b'!' if self.peek() == b'=' => two(self, Tok::Ne),
            b'!' => Tok::Bang,
            b'<' if self.peek() == b'<' => two(self, Tok::Shl),
            b'<' if self.peek() == b'=' => two(self, Tok::Le),
            b'<' => Tok::Lt,
            b'>' if self.peek() == b'>' => two(self, Tok::Shr),
            b'>' if self.peek() == b'=' => two(self, Tok::Ge),
            b'>' => Tok::Gt,
            _ => return Err(format!("{span}: unexpected character {:?}", c as char)),
        })
    }

    fn number(&mut self, span: Span) -> LexResult<Tok> {
        let start = self.pos;
        let (radix, skip) = if self.peek() == b'0' && (self.peek2() | 0x20) == b'x' {
            (16, 2)
        } else if self.peek() == b'0' && (self.peek2() | 0x20) == b'b' {
            (2, 2)
        } else {
            (10, 0)
        };
        for _ in 0..skip {
            self.bump();
        }
        let digits_start = self.pos;
        while self.peek().is_ascii_alphanumeric() || self.peek() == b'_' {
            self.bump();
        }
        let text: String = std::str::from_utf8(&self.src[digits_start..self.pos])
            .unwrap()
            .chars()
            .filter(|c| *c != '_')
            .collect();
        if text.is_empty() {
            let raw = std::str::from_utf8(&self.src[start..self.pos]).unwrap();
            return Err(format!("{span}: malformed numeric literal {raw:?}"));
        }
        u64::from_str_radix(&text, radix)
            .map(Tok::Num)
            .map_err(|_| format!("{span}: invalid base-{radix} literal {text:?}"))
    }

    fn word(&mut self) -> Tok {
        let start = self.pos;
        while self.peek().is_ascii_alphanumeric() || self.peek() == b'_' {
            self.bump();
        }
        let s = std::str::from_utf8(&self.src[start..self.pos]).unwrap();
        match s {
            "input" => Tok::Input,
            "output" => Tok::Output,
            "fn" => Tok::Fn,
            "proc" => Tok::Proc,
            "var" => Tok::Var,
            "if" => Tok::If,
            "else" => Tok::Else,
            "while" => Tok::While,
            "return" => Tok::Return,
            "as" => Tok::As,
            "true" => Tok::True,
            "false" => Tok::False,
            "bool" => Tok::UType(1),
            _ => {
                // `uN` is a type keyword, but only when N is a plain number;
                // identifiers like `u8x` or `up` stay identifiers.
                if let Some(rest) = s.strip_prefix('u') {
                    if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
                        if let Ok(w) = rest.parse::<u32>() {
                            return Tok::UType(w);
                        }
                    }
                }
                Tok::Ident(s.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(src: &str) -> Vec<Tok> {
        Lexer::new(src).tokenize().unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn lexes_a_declaration() {
        assert_eq!(
            toks("input u8 a;"),
            vec![Tok::Input, Tok::UType(8), Tok::Ident("a".into()), Tok::Semi, Tok::Eof]
        );
    }

    #[test]
    fn distinguishes_multi_char_operators() {
        assert_eq!(
            toks("< << <= > >> >= == != && || ->"),
            vec![
                Tok::Lt, Tok::Shl, Tok::Le, Tok::Gt, Tok::Shr, Tok::Ge,
                Tok::EqEq, Tok::Ne, Tok::AndAnd, Tok::OrOr, Tok::Arrow, Tok::Eof
            ]
        );
    }

    #[test]
    fn number_bases_and_separators() {
        assert_eq!(toks("42 0xff 0b1010 1_000"),
            vec![Tok::Num(42), Tok::Num(255), Tok::Num(10), Tok::Num(1000), Tok::Eof]);
    }

    #[test]
    fn utype_only_when_suffix_is_numeric() {
        assert_eq!(toks("u8"), vec![Tok::UType(8), Tok::Eof]);
        assert_eq!(toks("bool"), vec![Tok::UType(1), Tok::Eof]);
        assert_eq!(toks("up"), vec![Tok::Ident("up".into()), Tok::Eof]);
        assert_eq!(toks("u8x"), vec![Tok::Ident("u8x".into()), Tok::Eof]);
    }

    #[test]
    fn comments_are_trivia() {
        assert_eq!(toks("1 // line\n /* block\n */ 2"),
            vec![Tok::Num(1), Tok::Num(2), Tok::Eof]);
    }

    #[test]
    fn tracks_line_numbers() {
        let ts = Lexer::new("a\n\n  b").tokenize().unwrap();
        assert_eq!(ts[0].span.line, 1);
        assert_eq!(ts[1].span.line, 3);
        assert_eq!(ts[1].span.col, 3);
    }

    #[test]
    fn reports_unterminated_comment() {
        let err = Lexer::new("/* nope").tokenize().unwrap_err();
        assert!(err.contains("unterminated"), "{err}");
    }
}
