//! Rule text to tokens.
//!
//! There is no lexical subtlety to speak of: parentheses, atoms, integers, and comments from a
//! `;` to the end of the line. What the module does carry is a position on every token, because
//! a rule file is written by hand and the parser above it has to be able to say where.

use crate::error::Error;

/// One token and where it was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Spanned<'a> {
    pub(crate) token: Token<'a>,
    pub(crate) line: u32,
    pub(crate) column: u32,
}

/// The four things a rule file is made of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Token<'a> {
    Open,
    Close,
    /// A name. Opcodes wear a dot, as in `add.i64`, and the rest are plain identifiers.
    Atom(&'a str),
    /// A literal. Width is `i128` because a rule may mention a constant that only an `i64`
    /// operand can hold and the sign has to survive the parse.
    Int(i128),
}

/// Whether the character can begin a name. The operator characters are in the set because a
/// specification is written with the solver's own spelling, so `=` and `>=` and `bvadd` are all
/// heads and there is no reason for the reader to treat them differently.
fn starts_atom(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || b"=<>+-*/%&|^!~".contains(&c)
}

/// Whether the character can continue one. The dot is what makes `add.i64` a single token, and
/// the dash is what makes `sign-extend` one.
fn continues_atom(c: u8) -> bool {
    starts_atom(c) || c.is_ascii_digit() || c == b'.'
}

/// Whether a number starts here. This is asked before the atom test rather than after it,
/// because `-` begins both `-1` and the name of subtraction and only the next character says
/// which.
fn starts_number(bytes: &[u8], at: usize) -> bool {
    let c = bytes[at];
    c.is_ascii_digit() || (c == b'-' && bytes.get(at + 1).is_some_and(u8::is_ascii_digit))
}

/// Split the text of one rule file into tokens.
pub(crate) fn tokens<'a>(path: &str, text: &'a str) -> Result<Vec<Spanned<'a>>, Error> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut line = 1;
    let mut column = 1;

    let fail =
        |line, column, message: String| Error { path: path.to_owned(), line, column, message };

    while i < bytes.len() {
        let c = bytes[i];
        let (start_line, start_column) = (line, column);
        match c {
            b'\n' => {
                line += 1;
                column = 1;
                i += 1;
            }
            b' ' | b'\t' | b'\r' => {
                column += 1;
                i += 1;
            }
            b';' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                    column += 1;
                }
            }
            b'(' | b')' => {
                let token = if c == b'(' { Token::Open } else { Token::Close };
                out.push(Spanned { token, line, column });
                i += 1;
                column += 1;
            }
            _ if starts_number(bytes, i) => {
                let from = i;
                if c == b'-' {
                    i += 1;
                    column += 1;
                }
                let radix = if text[i..].starts_with("0x") || text[i..].starts_with("0X") {
                    i += 2;
                    column += 2;
                    16
                } else {
                    10
                };
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                    column += 1;
                }
                let written = &text[from..i];
                let body: String = written
                    .trim_start_matches('-')
                    .trim_start_matches("0x")
                    .trim_start_matches("0X")
                    .chars()
                    .filter(|c| *c != '_')
                    .collect();
                let value = i128::from_str_radix(&body, radix)
                    .map(|v| if written.starts_with('-') { -v } else { v });
                match value {
                    Ok(value) => out.push(Spanned {
                        token: Token::Int(value),
                        line: start_line,
                        column: start_column,
                    }),
                    Err(_) => {
                        let message = format!("`{written}` is not a number that fits in 128 bits");
                        return Err(fail(start_line, start_column, message));
                    }
                }
            }
            _ if starts_atom(c) => {
                let from = i;
                while i < bytes.len() && continues_atom(bytes[i]) {
                    i += 1;
                    column += 1;
                }
                let text = &text[from..i];
                out.push(Spanned {
                    token: Token::Atom(text),
                    line: start_line,
                    column: start_column,
                });
            }
            _ => {
                let shown = text[i..].chars().next().map(String::from).unwrap_or_default();
                let message = format!("`{shown}` cannot appear in a rule");
                return Err(fail(start_line, start_column, message));
            }
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{Token, tokens};

    fn kinds(text: &str) -> Vec<Token<'_>> {
        tokens("t.rules", text).unwrap().into_iter().map(|s| s.token).collect()
    }

    #[test]
    fn an_opcode_and_its_width_are_one_token() {
        assert_eq!(kinds("add.i64"), vec![Token::Atom("add.i64")]);
    }

    #[test]
    fn a_dash_inside_a_name_does_not_start_a_number() {
        assert_eq!(kinds("amode-base-index"), vec![Token::Atom("amode-base-index")]);
    }

    #[test]
    fn numbers_come_in_both_radices_and_both_signs() {
        assert_eq!(kinds("4 -1 0xff"), vec![Token::Int(4), Token::Int(-1), Token::Int(255)]);
    }

    #[test]
    fn a_comment_runs_to_the_end_of_the_line() {
        assert_eq!(kinds("; gone\n(x)"), vec![Token::Open, Token::Atom("x"), Token::Close]);
    }

    #[test]
    fn a_position_is_counted_from_one() {
        let got = tokens("t.rules", "\n  (").unwrap();
        assert_eq!((got[0].line, got[0].column), (2, 3));
    }

    #[test]
    fn something_that_is_not_a_rule_at_all_is_refused() {
        let got = tokens("t.rules", "@").unwrap_err();
        assert_eq!(got.to_string(), "t.rules:1:1: `@` cannot appear in a rule");
    }
}
