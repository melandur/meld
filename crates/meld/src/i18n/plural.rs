//! The C expression of a catalog's `Plural-Forms: … plural=EXPR;` header,
//! parsed once and evaluated for each count: `n`, integer literals,
//! `! * / % + - < <= > >= == != && || ?:` and parentheses, with C
//! precedence. Division by zero gives 0 instead of trapping.
//!
//! Std only: also compiled into `build.rs` and `xtask`, to reject catalogs
//! whose expression doesn't parse.

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    N,
    Num(u64),
    Not(Box<Expr>),
    Binary(Op, Box<Expr>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Mul,
    Div,
    Rem,
    Add,
    Sub,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
}

impl Expr {
    /// Parses `source`, e.g. `n%10==1 && n%100!=11 ? 0 : 1`.
    pub fn parse(source: &str) -> Result<Expr, String> {
        let tokens = tokenize(source)?;
        let mut parser = Parser { tokens, pos: 0 };
        let expr = parser.conditional()?;
        if parser.pos != parser.tokens.len() {
            return Err(format!(
                "unexpected {:?} in plural expression",
                parser.tokens[parser.pos]
            ));
        }
        Ok(expr)
    }

    pub fn eval(&self, n: u64) -> u64 {
        match self {
            Expr::N => n,
            Expr::Num(v) => *v,
            Expr::Not(e) => u64::from(e.eval(n) == 0),
            Expr::Cond(c, a, b) => {
                if c.eval(n) != 0 {
                    a.eval(n)
                } else {
                    b.eval(n)
                }
            }
            Expr::Binary(op, a, b) => {
                let x = a.eval(n);
                // Short-circuit like C, though nothing here has side effects
                match op {
                    Op::And => return u64::from(x != 0 && b.eval(n) != 0),
                    Op::Or => return u64::from(x != 0 || b.eval(n) != 0),
                    _ => {}
                }
                let y = b.eval(n);
                match op {
                    Op::Mul => x.wrapping_mul(y),
                    Op::Div => x.checked_div(y).unwrap_or(0),
                    Op::Rem => x.checked_rem(y).unwrap_or(0),
                    Op::Add => x.wrapping_add(y),
                    Op::Sub => x.wrapping_sub(y),
                    Op::Lt => u64::from(x < y),
                    Op::Le => u64::from(x <= y),
                    Op::Gt => u64::from(x > y),
                    Op::Ge => u64::from(x >= y),
                    Op::Eq => u64::from(x == y),
                    Op::Ne => u64::from(x != y),
                    Op::And | Op::Or => unreachable!(),
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    N,
    Num(u64),
    Op(&'static str),
}

fn tokenize(source: &str) -> Result<Vec<Token>, String> {
    const OPS: [&str; 18] = [
        "&&", "||", "==", "!=", "<=", ">=", "<", ">", "!", "*", "/", "%", "+", "-", "?", ":", "(",
        ")",
    ];
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    'outer: while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == b'n' {
            out.push(Token::N);
            i += 1;
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let v = source[start..i]
                .parse()
                .map_err(|_| "number out of range".to_string())?;
            out.push(Token::Num(v));
            continue;
        }
        for op in OPS {
            if source[i..].starts_with(op) {
                out.push(Token::Op(op));
                i += op.len();
                continue 'outer;
            }
        }
        return Err(format!(
            "unexpected character {:?} in plural expression",
            c as char
        ));
    }
    Ok(out)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek_op(&self) -> Option<&'static str> {
        match self.tokens.get(self.pos) {
            Some(Token::Op(op)) => Some(op),
            _ => None,
        }
    }

    fn expect(&mut self, op: &str) -> Result<(), String> {
        if self.peek_op() == Some(op) {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!("expected {op:?} in plural expression"))
        }
    }

    fn conditional(&mut self) -> Result<Expr, String> {
        let cond = self.binary(0)?;
        if self.peek_op() == Some("?") {
            self.pos += 1;
            let a = self.conditional()?;
            self.expect(":")?;
            let b = self.conditional()?;
            return Ok(Expr::Cond(Box::new(cond), Box::new(a), Box::new(b)));
        }
        Ok(cond)
    }

    /// Binary operators by precedence level, loosest first.
    fn binary(&mut self, level: usize) -> Result<Expr, String> {
        const LEVELS: [&[(&str, Op)]; 6] = [
            &[("||", Op::Or)],
            &[("&&", Op::And)],
            &[("==", Op::Eq), ("!=", Op::Ne)],
            &[("<=", Op::Le), (">=", Op::Ge), ("<", Op::Lt), (">", Op::Gt)],
            &[("+", Op::Add), ("-", Op::Sub)],
            &[("*", Op::Mul), ("/", Op::Div), ("%", Op::Rem)],
        ];
        if level == LEVELS.len() {
            return self.unary();
        }
        let mut left = self.binary(level + 1)?;
        while let Some(op) = self
            .peek_op()
            .and_then(|t| LEVELS[level].iter().find(|(s, _)| *s == t))
        {
            self.pos += 1;
            let right = self.binary(level + 1)?;
            left = Expr::Binary(op.1, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        match self.tokens.get(self.pos).cloned() {
            Some(Token::Op("!")) => {
                self.pos += 1;
                Ok(Expr::Not(Box::new(self.unary()?)))
            }
            Some(Token::Op("(")) => {
                self.pos += 1;
                let e = self.conditional()?;
                self.expect(")")?;
                Ok(e)
            }
            Some(Token::N) => {
                self.pos += 1;
                Ok(Expr::N)
            }
            Some(Token::Num(v)) => {
                self.pos += 1;
                Ok(Expr::Num(v))
            }
            other => Err(format!("unexpected {other:?} in plural expression")),
        }
    }
}

/// `(nplurals, plural expression source)` from a catalog header, e.g.
/// `Plural-Forms: nplurals=2; plural=(n != 1);`.
pub fn from_header(header: &str) -> Option<(usize, String)> {
    let line = header.lines().find(|l| {
        l.trim_start()
            .to_ascii_lowercase()
            .starts_with("plural-forms:")
    })?;
    let value = &line[line.find(':')? + 1..];
    let mut nplurals = None;
    let mut plural = None;
    for part in value.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("nplurals=") {
            nplurals = v.trim().parse().ok();
        } else if let Some(v) = part.strip_prefix("plural=") {
            plural = Some(v.trim().to_string());
        }
    }
    Some((nplurals?, plural?))
}
