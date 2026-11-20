//! `#pragma scalar_storage_order`, which is the `scalar_storage_order` attribute written once for
//! every record defined after it.
//!
//! `#pragma scalar_storage_order big-endian` stores the scalars of each `struct` and `union`
//! whose body closes after it in big-endian order, `little-endian` in little-endian order, and
//! `default` goes back to the target's own. A record that writes the attribute keeps what it
//! wrote. It is how a header describing a wire format says so once rather than on every record,
//! and the line is read at the closing brace, as `#pragma pack` is, since that is where gcc lays
//! the record out.
//!
//! gcc 13 reads only the first word, so `big-endian` is the word `big` and whatever follows it,
//! and so is `big` alone. A line with no word, or with another one, gets gcc's `-Wpragmas` warning
//! and is ignored, which leaves the order before it in effect.

use rucc_lex::Token;

use crate::parser::Parser;

impl Parser<'_> {
    /// One `#pragma scalar_storage_order` line, the word `scalar_storage_order` included.
    pub(crate) fn order_line(&mut self, line: &[Token]) {
        let word = line[0].span;
        // gcc's lexer hands a keyword to a pragma as a name, and `default` is one.
        let order = line.get(1).and_then(|token| match token.keyword() {
            Some(keyword) => Some(keyword.as_str()),
            None => token.ident().map(|name| self.cx.interner.resolve(name)),
        });
        let order = match order {
            Some("big") => Some(true),
            Some("little") => Some(false),
            Some("default") => None,
            Some(_) => {
                let what = "expected `big-endian`, `little-endian`, or `default` after \
                            `#pragma scalar_storage_order`";
                return self.warn("E0798", what, word);
            }
            None => {
                let what = "missing `big-endian`, `little-endian`, or `default` after \
                            `#pragma scalar_storage_order`";
                return self.warn("E0798", what, word);
            }
        };
        self.packs.order = order;
    }
}
