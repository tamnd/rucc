//! The debug text of Plan 9 yacc at a syntax error.
//!
//! The mkfiles of 6a and 7a give `-D1` to yacc. Thus at a new syntax error, the parser of yacc
//! prints the items of its state and the name of the token that it saw, after the message of
//! `yyerror`. The parsers of this crate do not use the tables of yacc, so they cannot know the
//! state of yacc. At a syntax error, the parser gives the tokens of the line to `debug`, which
//! runs the parser of yacc on them with no actions, as far as the error.
//!
//! This is correct because after each line, the stack of yacc has only the states of `prog`.
//! The actions do not change the stack, and the lexer has already changed the tokens that an
//! action changes, such as a name that becomes a label.

/// The tables that Plan 9 yacc writes for a grammar, with the names of `y.debug`.
pub(super) struct Tables {
    pub(super) exca: &'static [i16],
    pub(super) act: &'static [i16],
    pub(super) pact: &'static [i16],
    pub(super) pgo: &'static [i16],
    pub(super) r1: &'static [i16],
    pub(super) r2: &'static [i16],
    pub(super) chk: &'static [i16],
    pub(super) def: &'static [i16],
    pub(super) last: i32,
    /// `yylex1`, which gives the code of yacc for a token of the lexer.
    pub(super) code: fn(i32) -> i32,
    /// `yytoknames`. An empty name is a null pointer.
    pub(super) names: &'static [&'static str],
    /// `yystates`. An empty text is a null pointer.
    pub(super) states: &'static [&'static str],
}

/// `YYFLAG` of yaccpar.
const YYFLAG: i32 = -1000;

fn at(t: &[i16], i: i32) -> i32 {
    usize::try_from(i).ok().and_then(|i| t.get(i)).map_or(0, |&v| i32::from(v))
}

impl Tables {
    /// `yytokname`.
    fn tokname(&self, c: i32) -> String {
        if c > 0 {
            if let Some(s) = usize::try_from(c - 1).ok().and_then(|i| self.names.get(i)) {
                if !s.is_empty() {
                    return (*s).to_string();
                }
            }
        }
        format!("<{c}>")
    }

    /// `yystatname`.
    fn statname(&self, s: i32) -> String {
        if let Some(t) = usize::try_from(s).ok().and_then(|i| self.states.get(i)) {
            if !t.is_empty() {
                return (*t).to_string();
            }
        }
        format!("<{s}>\n")
    }

    /// The text that yacc prints after the message of a new syntax error, when the tokens of
    /// the lexer since the start of the line are `toks`. It is `None` when the parser of yacc
    /// finds no error in the tokens.
    pub(super) fn debug(&self, toks: &[i32]) -> Option<String> {
        let mut stack: Vec<i32> = vec![0];
        let mut state = 0;
        let mut next = toks.iter().map(|&t| (self.code)(t));
        let mut ch: i32 = -1;
        loop {
            // yynewstate
            let mut n = at(self.pact, state);
            if n > YYFLAG {
                if ch < 0 {
                    ch = next.next()?;
                }
                n += ch;
                if (0..self.last).contains(&n) {
                    let s = at(self.act, n);
                    if at(self.chk, s) == ch {
                        ch = -1;
                        state = s;
                        stack.push(s);
                        continue;
                    }
                }
            }
            // yydefault
            let mut n = at(self.def, state);
            if n == -2 {
                if ch < 0 {
                    ch = next.next()?;
                }
                let mut i = 0;
                while !(at(self.exca, i) == -1 && at(self.exca, i + 1) == state) {
                    i += 2;
                    if usize::try_from(i).map_or(true, |i| i >= self.exca.len()) {
                        return None;
                    }
                }
                loop {
                    i += 2;
                    let c = at(self.exca, i);
                    if c < 0 || c == ch {
                        break;
                    }
                }
                n = at(self.exca, i + 1);
                if n < 0 {
                    return None;
                }
            }
            if n == 0 {
                return Some(format!("{}saw {}\n", self.statname(state), self.tokname(ch)));
            }
            // The reduction by the rule n.
            let len = usize::try_from(at(self.r2, n)).unwrap_or(0);
            if len >= stack.len() {
                return None;
            }
            stack.truncate(stack.len() - len);
            let lhs = at(self.r1, n);
            let g = at(self.pgo, lhs);
            let j = g + stack[stack.len() - 1] + 1;
            state = at(self.act, j);
            if j >= self.last || at(self.chk, state) != -lhs {
                state = at(self.act, g);
            }
            stack.push(state);
        }
    }
}
