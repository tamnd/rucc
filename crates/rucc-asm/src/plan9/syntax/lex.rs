//! The lexer and the macro processor of the Plan 9 assemblers.
//!
//! This is a port of `cc/lexbody` and `cc/macbody` at the pin. The assemblers `6a` and `7a` share
//! these two files, and the output of an assembler depends on their details: the line of each
//! record is the value of `lineno` when the parser reduces the instruction, and the history comes
//! from the files that the lexer opens and closes. Thus the port keeps the order of each read and
//! each count of a newline.

use rucc_base::hash::Map;

/// The end of the input.
pub(super) const EOF: i32 = -1;
/// No character in `peekc`.
const IGN: i32 = -2;
/// The number of slots in the symbol cache of the object.
pub(super) const NSYM: usize = 50;
/// The flag in the argument count of a macro with `...`.
const VARMAC: u8 = 0x80;
/// The largest number of arguments of a macro.
const NARG: usize = 25;
/// The size of the stack of `prfile`.
const HISTSZ: usize = 20;
/// The largest number of include directories.
const NINCLUDE: usize = 10;
/// The size of the name of an include file.
const STRINGSZ: usize = 200;
/// The size of an input buffer, which limits the text of one macro expansion.
const BUFSIZ: usize = 8192;
/// The largest number of input buffers at one time.
const NIO: usize = 1000;
/// The size of the argument text of one macro expansion.
const NMACBUF: usize = 2000;
/// The largest size of a UTF-8 sequence.
const UTFMAX: usize = 4;

/// The first token number of an assembler. The tokens of the grammar have the numbers from here.
pub(super) const FIRST: i32 = 1000;
/// A name with no other meaning.
pub(super) const LNAME: i32 = 900;
/// A label.
pub(super) const LLAB: i32 = 901;
/// A name with a value from `=`.
pub(super) const LVAR: i32 = 902;
/// An integer constant.
pub(super) const LCONST: i32 = 903;
/// A floating constant.
pub(super) const LFCONST: i32 = 904;
/// A string constant of at most 8 bytes.
pub(super) const LSCONST: i32 = 905;

/// The stop of the assembler, after `errorexit`.
#[derive(Debug)]
pub(super) struct Fatal;

/// The result of a step that can stop the assembler.
pub(super) type R<T> = Result<T, Fatal>;

/// The machine parts of the lexer.
#[derive(Debug, Clone, Copy)]
pub(super) struct Arch {
    /// The letter of the assembler, `6` or `7`.
    pub(super) thechar: u8,
    /// The size of the buffer of a name.
    pub(super) nsymb: usize,
    /// The include directory of the machine.
    pub(super) include: &'static str,
    /// `Sym.value` is a `long` of 32 bits.
    pub(super) value32: bool,
}

/// A name of the symbol table.
#[derive(Debug, Clone)]
pub(super) struct Sym {
    pub(super) name: Vec<u8>,
    pub(super) macro_: Option<Vec<u8>>,
    pub(super) value: i64,
    pub(super) typ: i32,
    pub(super) slot: i8,
}

/// An entry of the history.
#[derive(Debug, Clone)]
pub(super) struct Hist {
    pub(super) name: Option<Vec<u8>>,
    pub(super) line: i32,
    pub(super) offset: i64,
}

/// A token and its value.
#[derive(Debug, Clone, Copy)]
pub(super) struct Tok {
    pub(super) t: i32,
    pub(super) sym: usize,
    pub(super) lval: i64,
    pub(super) dval: f64,
    pub(super) sval: [u8; 8],
}

impl Tok {
    fn of(t: i32) -> Self {
        Self { t, sym: 0, lval: 0, dval: 0.0, sval: [0; 8] }
    }
}

/// An input: a file or the text of a macro expansion.
#[derive(Debug)]
struct Io {
    buf: Vec<u8>,
    pos: usize,
    file: bool,
}

/// The reader of a file. It takes the name as the assembler gives it to `open`.
pub type Reader<'a> = &'a dyn Fn(&str) -> Option<Vec<u8>>;

/// The state of the lexer.
pub(super) struct Lexer<'a> {
    arch: Arch,
    io: Vec<Io>,
    pub(super) peekc: i32,
    pub(super) lineno: i32,
    pub(super) syms: Vec<Sym>,
    names: Map<Vec<u8>, usize>,
    pub(super) hist: Vec<Hist>,
    include: Vec<Vec<u8>>,
    symb: Vec<u8>,
    pub(super) nerrors: u32,
    pub(super) messages: String,
    read: Reader<'a>,
}

impl std::fmt::Debug for Lexer<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lexer").field("lineno", &self.lineno).finish_non_exhaustive()
    }
}

fn isspace(c: i32) -> bool {
    matches!(c, 0x20 | 0x09..=0x0d)
}

fn isalpha(c: i32) -> bool {
    (0x41..=0x5a).contains(&c) || (0x61..=0x7a).contains(&c)
}

fn isdigit(c: i32) -> bool {
    (0x30..=0x39).contains(&c)
}

fn isalnum(c: i32) -> bool {
    isalpha(c) || isdigit(c)
}

/// `fullrune` of Plan 9.
fn fullrune(s: &[u8]) -> bool {
    let n = s.len();
    if n > 0 {
        let c = s[0];
        if c < 0x80 {
            return true;
        }
        if n > 1 {
            if c < 0xe0 {
                return true;
            }
            if n > 2 && (c < 0xf0 || n > 3) {
                return true;
            }
        }
    }
    false
}

const fn ch(c: u8) -> i32 {
    c as i32
}

/// The byte of a character that the lexer has read. A character is in 0 to 255, and `EOF` becomes
/// 255, as it does in C.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn byte(c: i32) -> u8 {
    c as u8
}

impl<'a> Lexer<'a> {
    /// `cinit`, without the names of the machine, and the include directory `.`.
    pub(super) fn new(arch: Arch, read: Reader<'a>) -> Self {
        Self {
            arch,
            io: Vec::new(),
            peekc: IGN,
            lineno: 0,
            syms: Vec::new(),
            names: Map::default(),
            hist: Vec::new(),
            include: vec![b".".to_vec()],
            symb: Vec::new(),
            nerrors: 0,
            messages: String::new(),
            read,
        }
    }

    /// Enters a name of the machine.
    pub(super) fn define(&mut self, name: &str, typ: i32, value: i64) {
        let s = self.lookup(name.as_bytes());
        self.syms[s].typ = typ;
        self.syms[s].value = self.value(value);
    }

    /// A value as `Sym.value` keeps it.
    #[allow(clippy::cast_possible_truncation)]
    pub(super) fn value(&self, v: i64) -> i64 {
        if self.arch.value32 { i64::from(v as i32) } else { v }
    }

    /// `setinclude`.
    pub(super) fn setinclude(&mut self, p: &[u8]) -> R<()> {
        if self.include[1..].iter().any(|i| i == p) {
            return Ok(());
        }
        if self.include.len() >= NINCLUDE {
            self.yyerror(&format!("ninclude too small {NINCLUDE}"))?;
            return Err(Fatal);
        }
        self.include.push(p.to_vec());
        Ok(())
    }

    /// Sets `include[0]`, the directory of the file.
    pub(super) fn set_dir(&mut self, dir: &[u8]) {
        self.include[0] = dir.to_vec();
    }

    /// The include directory of the machine, as `assemble` adds it.
    pub(super) fn machine_include(&mut self) -> R<()> {
        let p = self.arch.include.as_bytes().to_vec();
        self.setinclude(&p)
    }

    /// `lookup`, which makes the name when it is not there.
    pub(super) fn lookup(&mut self, name: &[u8]) -> usize {
        if let Some(&s) = self.names.get(name) {
            return s;
        }
        let s = self.syms.len();
        self.syms.push(Sym { name: name.to_vec(), macro_: None, value: 0, typ: LNAME, slot: 0 });
        self.names.insert(name.to_vec(), s);
        s
    }

    /// `slookup`.
    fn slookup(&mut self, name: &[u8]) -> R<usize> {
        if name.len() >= self.arch.nsymb - 1 {
            let s = String::from_utf8_lossy(name).into_owned();
            self.yyerror(&format!("symbol too long: {s}"))?;
            return Err(Fatal);
        }
        Ok(self.lookup(name))
    }

    /// `yyerror`, with the file and line in front.
    pub(super) fn yyerror(&mut self, msg: &str) -> R<()> {
        let at = self.prfile(self.lineno);
        self.messages.push_str(&at);
        self.messages.push_str(msg);
        self.messages.push('\n');
        self.nerrors += 1;
        if self.nerrors > 10 {
            self.messages.push_str("too many errors\n");
            return Err(Fatal);
        }
        Ok(())
    }

    /// The message of yacc for a syntax error.
    pub(super) fn syntax_error(&mut self) -> R<()> {
        let s = String::from_utf8_lossy(&self.symb).into_owned();
        self.yyerror(&format!("syntax error, last name: {s}"))
    }

    /// `prfile`.
    fn prfile(&self, l: i32) -> String {
        let mut a: Vec<Hist> = Vec::new();
        let mut n: i64 = 0;
        for h in &self.hist {
            if l < h.line {
                break;
            }
            if h.name.is_some() {
                if h.offset == 0 {
                    if (0..HISTSZ as i64).contains(&n) {
                        set(&mut a, n, h.clone());
                    }
                    n += 1;
                    continue;
                }
                if n > 0 && n < HISTSZ as i64 {
                    if at(&a, n - 1).offset == 0 {
                        set(&mut a, n, h.clone());
                        n += 1;
                    } else {
                        set(&mut a, n - 1, h.clone());
                    }
                }
                continue;
            }
            n -= 1;
            if (0..HISTSZ as i64).contains(&n) {
                let d = h.line - at(&a, n).line;
                for e in a.iter_mut().take(usize::try_from(n).unwrap_or(0)) {
                    e.line += d;
                }
            }
        }
        let n = usize::try_from(n.min(HISTSZ as i64)).unwrap_or(0);
        let mut out = String::new();
        for h in a.iter().take(n) {
            let name = String::from_utf8_lossy(h.name.as_deref().unwrap_or_default());
            let line = i64::from(l) - i64::from(h.line) + h.offset + 1;
            out.push_str(&format!("{name}:{line} "));
        }
        out
    }

    /// `GETC`: the next byte of the input, or `EOF`. The end of a file pops the history.
    fn getc_raw(&mut self) -> i32 {
        loop {
            let Some(top) = self.io.last_mut() else {
                return EOF;
            };
            if top.pos < top.buf.len() {
                let c = top.buf[top.pos];
                top.pos += 1;
                return i32::from(c);
            }
            let file = top.file;
            self.io.pop();
            if file {
                self.linehist(None, 0);
            }
        }
    }

    /// `newfile`: reads the file and pushes it, and adds it to the history.
    fn newfile(&mut self, name: Vec<u8>, text: Option<Vec<u8>>) -> R<()> {
        let text = match text {
            Some(t) => t,
            None => match (self.read)(&String::from_utf8_lossy(&name)) {
                Some(t) => t,
                None => {
                    let s = String::from_utf8_lossy(&name).into_owned();
                    self.yyerror(&format!(
                        "{}a: file does not exist: {s}",
                        char::from(self.arch.thechar)
                    ))?;
                    return Err(Fatal);
                }
            },
        };
        self.push_io(text, true)?;
        self.linehist(Some(name), 0);
        Ok(())
    }

    fn push_io(&mut self, buf: Vec<u8>, file: bool) -> R<()> {
        if self.io.len() >= NIO {
            self.yyerror("macro/io expansion too deep")?;
            return Err(Fatal);
        }
        self.io.push(Io { buf, pos: 0, file });
        Ok(())
    }

    /// `pinit`: the start of a pass.
    pub(super) fn pinit(&mut self, file: &[u8]) -> R<()> {
        self.lineno = 1;
        self.newfile(file.to_vec(), None)?;
        self.peekc = IGN;
        for s in &mut self.syms {
            s.macro_ = None;
        }
        Ok(())
    }

    /// `getc`: a character with the count of each newline. The end of the input is an error.
    fn getc(&mut self) -> R<i32> {
        let c = self.peekc;
        if c != IGN {
            self.peekc = IGN;
            if c == ch(b'\n') {
                self.lineno += 1;
            }
            return Ok(c);
        }
        let c = self.getc_raw();
        if c == ch(b'\n') {
            self.lineno += 1;
        }
        if c == EOF {
            self.yyerror("End of file")?;
            return Err(Fatal);
        }
        Ok(c)
    }

    /// `getnsc`: the next character that is not a space, or a newline.
    fn getnsc(&mut self) -> R<i32> {
        loop {
            let c = self.getc()?;
            if !isspace(c) || c == ch(b'\n') {
                return Ok(c);
            }
        }
    }

    /// `unget`.
    fn unget(&mut self, c: i32) {
        self.peekc = c;
        if c == ch(b'\n') {
            self.lineno -= 1;
        }
    }

    /// `escchar`: a character of a string or a character constant, or `EOF` at the end.
    fn escchar(&mut self, e: i32) -> R<i32> {
        loop {
            let mut c = self.getc()?;
            if c == ch(b'\n') {
                self.yyerror("newline in string")?;
                return Ok(EOF);
            }
            if c != ch(b'\\') {
                if c == e {
                    return Ok(EOF);
                }
                return Ok(c);
            }
            c = self.getc()?;
            if (ch(b'0')..=ch(b'7')).contains(&c) {
                let mut l = c - ch(b'0');
                c = self.getc()?;
                if (ch(b'0')..=ch(b'7')).contains(&c) {
                    l = l * 8 + c - ch(b'0');
                    c = self.getc()?;
                    if (ch(b'0')..=ch(b'7')).contains(&c) {
                        l = l * 8 + c - ch(b'0');
                        return Ok(l);
                    }
                }
                self.peekc = c;
                return Ok(l);
            }
            return Ok(match byte(c) {
                b'\n' => continue,
                b'n' => ch(b'\n'),
                b't' => ch(b'\t'),
                b'b' => 0x08,
                b'r' => ch(b'\r'),
                b'f' => 0x0c,
                b'a' => 0x07,
                b'v' => 0x0b,
                b'z' => 0,
                _ => c,
            });
        }
    }

    fn toolong(&mut self) -> R<Tok> {
        let s = String::from_utf8_lossy(&self.symb).into_owned();
        self.yyerror(&format!("token too long: {s}..."))?;
        Err(Fatal)
    }

    /// `yylex`.
    pub(super) fn yylex(&mut self) -> R<Tok> {
        let mut c = self.peekc;
        if c == IGN {
            c = self.getc_raw();
        } else {
            self.peekc = IGN;
        }
        loop {
            if c == EOF {
                self.peekc = EOF;
                return Ok(Tok::of(-1));
            }
            if isspace(c) {
                if c == ch(b'\n') {
                    self.lineno += 1;
                    return Ok(Tok::of(ch(b';')));
                }
                c = self.getc_raw();
                continue;
            }
            if isalpha(c) || c == ch(b'_') || c == ch(b'@') {
                self.symb.clear();
                match self.aloop(c)? {
                    Some(t) => return Ok(t),
                    None => {
                        c = self.getc_raw();
                        continue;
                    }
                }
            }
            if isdigit(c) {
                return self.number(c);
            }
            match byte(c) {
                b'#' => {
                    self.domacro()?;
                    c = self.getc_raw();
                }
                b'.' => {
                    c = self.getc_raw();
                    if isalpha(c) {
                        self.symb.clear();
                        self.symb.push(b'.');
                        match self.aloop(c)? {
                            Some(t) => return Ok(t),
                            None => {
                                c = self.getc_raw();
                                continue;
                            }
                        }
                    }
                    if isdigit(c) {
                        self.symb.clear();
                        self.symb.push(b'.');
                        return self.casedot(c);
                    }
                    self.peekc = c;
                    return Ok(Tok::of(ch(b'.')));
                }
                b'"' => {
                    let mut t = Tok::of(LSCONST);
                    let mut c1 = 0usize;
                    loop {
                        let c = self.escchar(ch(b'"'))?;
                        if c == EOF {
                            break;
                        }
                        if c1 < 8 {
                            t.sval[c1] = byte(c);
                        }
                        c1 += 1;
                    }
                    if c1 > 8 {
                        self.yyerror("string constant too long")?;
                    }
                    return Ok(t);
                }
                b'\'' => {
                    let mut c = self.escchar(ch(b'\''))?;
                    if c == EOF {
                        c = ch(b'\'');
                    }
                    if self.escchar(ch(b'\''))? != EOF {
                        self.yyerror("missing '")?;
                    }
                    let mut t = Tok::of(LCONST);
                    t.lval = i64::from(c);
                    return Ok(t);
                }
                b'/' => {
                    let c1 = self.getc_raw();
                    if c1 == ch(b'/') {
                        loop {
                            c = self.getc_raw();
                            if c == ch(b'\n') {
                                break;
                            }
                            if c == EOF {
                                self.yyerror("eof in comment")?;
                                return Err(Fatal);
                            }
                        }
                        continue;
                    }
                    if c1 == ch(b'*') {
                        'comment: loop {
                            c = self.getc_raw();
                            while c == ch(b'*') {
                                c = self.getc_raw();
                                if c == ch(b'/') {
                                    break 'comment;
                                }
                            }
                            if c == EOF {
                                self.yyerror("eof in comment")?;
                                return Err(Fatal);
                            }
                            if c == ch(b'\n') {
                                self.lineno += 1;
                            }
                        }
                        c = self.getc_raw();
                        continue;
                    }
                    self.peekc = c1;
                    return Ok(Tok::of(c));
                }
                _ => return Ok(Tok::of(c)),
            }
        }
    }

    /// The rest of a name, from `aloop`. `None` is a macro expansion, after which the lexer
    /// reads again.
    fn aloop(&mut self, mut c: i32) -> R<Option<Tok>> {
        loop {
            if self.symb.len() >= self.arch.nsymb - 1 {
                return self.toolong().map(Some);
            }
            self.symb.push(byte(c));
            c = self.getc_raw();
            if !(isalnum(c) || c == ch(b'_') || c == ch(b'$')) {
                break;
            }
        }
        self.peekc = c;
        let name = self.symb.clone();
        let s = self.lookup(&name);
        if self.syms[s].macro_.is_some()
            && let Some(mut buf) = self.macexpand(s, BUFSIZ - 1)?
        {
            if self.peekc != IGN {
                buf.push(byte(self.peekc));
                self.peekc = IGN;
            }
            self.push_io(buf, false)?;
            return Ok(None);
        }
        let sym = &self.syms[s];
        let mut t = Tok::of(sym.typ);
        if matches!(sym.typ, LNAME | LVAR | LLAB) {
            t.sym = s;
        } else {
            t.lval = sym.value;
        }
        Ok(Some(t))
    }

    fn number(&mut self, mut c: i32) -> R<Tok> {
        let old = std::mem::take(&mut self.symb);
        let mut lval: i64 = 0;
        'dc: {
            if c != ch(b'0') {
                break 'dc;
            }
            self.symb.push(byte(c));
            c = self.getc_raw();
            let mut c1 = 3;
            if c == ch(b'x') || c == ch(b'X') {
                c1 = 4;
                c = self.getc_raw();
            } else if c < ch(b'0') || c > ch(b'7') {
                break 'dc;
            }
            loop {
                if isdigit(c) {
                    if c > ch(b'7') && c1 == 3 {
                        break;
                    }
                    lval = (lval << c1).wrapping_add(i64::from(c - ch(b'0')));
                    c = self.getc_raw();
                    continue;
                }
                if c1 == 3 {
                    break;
                }
                if (ch(b'A')..=ch(b'F')).contains(&c) {
                    c += ch(b'a') - ch(b'A');
                }
                if (ch(b'a')..=ch(b'f')).contains(&c) {
                    lval = (lval << c1).wrapping_add(i64::from(c - ch(b'a') + 10));
                    c = self.getc_raw();
                    continue;
                }
                break;
            }
            // The C lexer does not end `symb` here, so the rest of the last name stays in it.
            self.symb.extend_from_slice(old.get(1..).unwrap_or_default());
            return Ok(self.ncu(c, lval));
        }
        loop {
            if !isdigit(c) {
                break;
            }
            if self.symb.len() >= self.arch.nsymb - 1 {
                return self.toolong();
            }
            self.symb.push(byte(c));
            c = self.getc_raw();
        }
        if c == ch(b'.') {
            return self.casedot(c);
        }
        if c == ch(b'e') || c == ch(b'E') {
            return self.casee();
        }
        Ok(self.ncu(c, strtoull(&self.symb)))
    }

    fn ncu(&mut self, mut c: i32, lval: i64) -> Tok {
        while c == ch(b'U') || c == ch(b'u') || c == ch(b'l') || c == ch(b'L') {
            c = self.getc_raw();
        }
        self.peekc = c;
        let mut t = Tok::of(LCONST);
        t.lval = lval;
        t
    }

    fn casedot(&mut self, mut c: i32) -> R<Tok> {
        loop {
            if self.symb.len() >= self.arch.nsymb - 1 {
                return self.toolong();
            }
            self.symb.push(byte(c));
            c = self.getc_raw();
            if !isdigit(c) {
                break;
            }
        }
        if c == ch(b'e') || c == ch(b'E') {
            return self.casee();
        }
        Ok(self.caseout(c))
    }

    fn casee(&mut self) -> R<Tok> {
        if self.symb.len() >= self.arch.nsymb - 1 {
            return self.toolong();
        }
        self.symb.push(b'e');
        let mut c = self.getc_raw();
        if c == ch(b'+') || c == ch(b'-') {
            if self.symb.len() >= self.arch.nsymb - 1 {
                return self.toolong();
            }
            self.symb.push(byte(c));
            c = self.getc_raw();
        }
        while isdigit(c) {
            if self.symb.len() >= self.arch.nsymb - 1 {
                return self.toolong();
            }
            self.symb.push(byte(c));
            c = self.getc_raw();
        }
        Ok(self.caseout(c))
    }

    fn caseout(&mut self, c: i32) -> Tok {
        self.peekc = c;
        let mut t = Tok::of(LFCONST);
        t.dval = atof(&self.symb);
        t
    }

    // The macro processor, from `cc/macbody`.

    /// `getnsn`: a decimal number, or -1.
    fn getnsn(&mut self) -> R<i64> {
        let mut c = self.getnsc()?;
        if !isdigit(c) {
            return Ok(-1);
        }
        let mut n: i64 = 0;
        while isdigit(c) {
            n = n.wrapping_mul(10).wrapping_add(i64::from(c - ch(b'0')));
            c = self.getc()?;
        }
        self.unget(c);
        Ok(n)
    }

    /// `nextsym`: a name into `symb`.
    fn nextsym(&mut self, mut c: i32) -> R<()> {
        let lim = self.arch.nsymb - UTFMAX;
        self.symb.clear();
        loop {
            if c >= 0x80 {
                let cp = self.symb.len();
                let mut rune = Vec::new();
                loop {
                    if cp <= lim {
                        rune.push(byte(c));
                    }
                    if fullrune(&rune) {
                        break;
                    }
                    c = self.getc()?;
                }
                self.symb.extend_from_slice(&rune);
            } else if self.symb.len() <= lim {
                self.symb.push(byte(c));
            }
            c = self.getc()?;
            if c >= 0x80 || isalnum(c) || c == ch(b'_') {
                continue;
            }
            self.unget(c);
            break;
        }
        if self.symb.len() > lim {
            let s = String::from_utf8_lossy(&self.symb).into_owned();
            self.yyerror(&format!("symbol too large: {s}"))?;
        }
        Ok(())
    }

    /// `getsym`.
    fn getsym(&mut self) -> R<Option<usize>> {
        let c = self.getnsc()?;
        if c < 0x80 && !isalpha(c) && c != ch(b'_') {
            self.unget(c);
            return Ok(None);
        }
        self.nextsym(c)?;
        let name = self.symb.clone();
        Ok(Some(self.lookup(&name)))
    }

    /// `getsymdots`.
    fn getsymdots(&mut self, dots: &mut bool) -> R<Option<usize>> {
        if let Some(s) = self.getsym()? {
            return Ok(Some(s));
        }
        let c = self.getnsc()?;
        if c != ch(b'.') {
            self.unget(c);
            return Ok(None);
        }
        if self.getc()? != ch(b'.') || self.getc()? != ch(b'.') {
            self.yyerror("bad dots in macro")?;
        }
        *dots = true;
        self.slookup(b"__VA_ARGS__").map(Some)
    }

    /// `getcom`: the next character after the spaces and the comments.
    fn getcom(&mut self) -> R<i32> {
        let mut c;
        loop {
            c = self.getnsc()?;
            if c != ch(b'/') {
                break;
            }
            c = self.getc()?;
            if c == ch(b'/') {
                while c != ch(b'\n') {
                    c = self.getc()?;
                }
                break;
            }
            if c != ch(b'*') {
                break;
            }
            c = self.getc()?;
            loop {
                if c == ch(b'*') {
                    c = self.getc()?;
                    if c != ch(b'/') {
                        continue;
                    }
                    c = self.getc()?;
                    break;
                }
                if c == ch(b'\n') {
                    self.yyerror("comment across newline")?;
                    break;
                }
                c = self.getc()?;
            }
            if c == ch(b'\n') {
                break;
            }
        }
        Ok(c)
    }

    /// `dodefine`, for `-D`.
    pub(super) fn dodefine(&mut self, cp: &[u8]) -> R<()> {
        let mut symb = cp.to_vec();
        if symb.len() >= self.arch.nsymb {
            let s = String::from_utf8_lossy(cp).into_owned();
            self.yyerror(&format!("macro too long: {s}"))?;
            symb.truncate(self.arch.nsymb - 1);
        }
        if let Some(eq) = symb.iter().position(|&b| b == b'=') {
            let s = self.lookup(&symb[..eq]);
            let mut m = vec![0];
            m.extend_from_slice(&symb[eq + 1..]);
            m.push(0);
            self.syms[s].macro_ = Some(m);
        } else {
            let s = self.lookup(&symb);
            self.syms[s].macro_ = Some(vec![0, b'1', 0]);
        }
        Ok(())
    }

    /// `domacro`, after `#`.
    fn domacro(&mut self) -> R<()> {
        let s = match self.getsym()? {
            Some(s) => s,
            None => self.slookup(b"endif")?,
        };
        let name = self.syms[s].name.clone();
        match name.as_slice() {
            b"ifdef" => self.macif(0),
            b"ifndef" => self.macif(1),
            b"else" => self.macif(2),
            b"line" => self.maclin(),
            b"define" => self.macdef(),
            b"include" => self.macinc(),
            b"undef" => self.macund(),
            b"pragma" => self.macprag(),
            b"endif" => self.macend(),
            _ => {
                let n = String::from_utf8_lossy(&name).into_owned();
                self.yyerror(&format!("unknown #: {n}"))?;
                self.macend()
            }
        }
    }

    /// `macund`.
    fn macund(&mut self) -> R<()> {
        let s = self.getsym()?;
        self.macend()?;
        match s {
            None => self.yyerror("syntax in #undef"),
            Some(s) => {
                self.syms[s].macro_ = None;
                Ok(())
            }
        }
    }

    /// `macdef`.
    #[allow(clippy::too_many_lines)]
    fn macdef(&mut self) -> R<()> {
        let Some(s) = self.getsym()? else {
            return self.macdef_bad(None);
        };
        if self.syms[s].macro_.is_some() {
            let n = String::from_utf8_lossy(&self.syms[s].name).into_owned();
            self.yyerror(&format!("macro redefined: {n}"))?;
        }
        let mut c = self.getc()?;
        let mut n: i32 = -1;
        let mut dots = false;
        let mut args: Vec<Vec<u8>> = Vec::new();
        if c == ch(b'(') {
            n += 1;
            c = self.getnsc()?;
            if c != ch(b')') {
                self.unget(c);
                loop {
                    let Some(a) = self.getsymdots(&mut dots)? else {
                        return self.macdef_bad(Some(s));
                    };
                    if args.len() >= NARG {
                        let nm = String::from_utf8_lossy(&self.syms[s].name).into_owned();
                        self.yyerror(&format!("too many arguments in #define: {nm}"))?;
                        return self.macdef_bad(Some(s));
                    }
                    args.push(self.syms[a].name.clone());
                    n += 1;
                    c = self.getnsc()?;
                    if c == ch(b')') {
                        break;
                    }
                    if c != ch(b',') || dots {
                        return self.macdef_bad(Some(s));
                    }
                }
            }
            c = self.getc()?;
        }
        if isspace(c) && c != ch(b'\n') {
            c = self.getnsc()?;
        }
        let mut base: Vec<u8> = vec![0];
        let mut ischr: i32 = 0;
        loop {
            if c >= 0x80 || isalpha(c) || c == ch(b'_') {
                self.nextsym(c)?;
                c = self.getc()?;
                match args.iter().position(|a| *a == self.symb) {
                    None => base.extend_from_slice(&self.symb),
                    Some(i) => {
                        base.push(b'#');
                        base.push(b'a' + u8::try_from(i).unwrap_or(0));
                    }
                }
                continue;
            }
            if ischr != 0 {
                if c == ch(b'\\') {
                    base.push(byte(c));
                    c = self.getc()?;
                } else if c == ischr {
                    ischr = 0;
                }
            } else {
                if c == ch(b'"') || c == ch(b'\'') {
                    base.push(byte(c));
                    ischr = c;
                    c = self.getc()?;
                    continue;
                }
                if c == ch(b'/') {
                    c = self.getc()?;
                    if c == ch(b'/') {
                        c = self.getc()?;
                        while c != ch(b'\n') {
                            c = self.getc()?;
                        }
                        continue;
                    }
                    if c == ch(b'*') {
                        c = self.getc()?;
                        loop {
                            if c == ch(b'*') {
                                c = self.getc()?;
                                if c != ch(b'/') {
                                    continue;
                                }
                                c = self.getc()?;
                                break;
                            }
                            c = self.getc()?;
                        }
                        continue;
                    }
                    base.push(b'/');
                    continue;
                }
            }
            if c == ch(b'\\') {
                c = self.getc()?;
                if c == ch(b'\n') {
                    c = self.getc()?;
                    continue;
                } else if c == ch(b'\r') {
                    c = self.getc()?;
                    if c == ch(b'\n') {
                        c = self.getc()?;
                        continue;
                    }
                }
                base.push(b'\\');
                continue;
            }
            if c == ch(b'\n') {
                break;
            }
            if c == ch(b'#') && n > 0 {
                base.push(byte(c));
            }
            base.push(byte(c));
            c = self.getc_raw();
            if c == ch(b'\n') {
                self.lineno += 1;
            }
            if c == EOF {
                let nm = String::from_utf8_lossy(&self.syms[s].name).into_owned();
                self.yyerror(&format!("eof in a macro: {nm}"))?;
                break;
            }
        }
        loop {
            base.push(0);
            if base.len() & 3 == 0 {
                break;
            }
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let mut nargs = (n + 1) as u8;
        if dots {
            nargs |= VARMAC;
        }
        base[0] = nargs;
        self.syms[s].macro_ = Some(base);
        Ok(())
    }

    fn macdef_bad(&mut self, s: Option<usize>) -> R<()> {
        match s {
            None => self.yyerror("syntax in #define")?,
            Some(s) => {
                let n = String::from_utf8_lossy(&self.syms[s].name).into_owned();
                self.yyerror(&format!("syntax in #define: {n}"))?;
            }
        }
        self.macend()
    }

    /// `macexpand`: the text of the macro `s`, or `None` when it does not expand.
    #[allow(clippy::too_many_lines)]
    fn macexpand(&mut self, s: usize, blen: usize) -> R<Option<Vec<u8>>> {
        let mac = self.syms[s].macro_.clone().unwrap_or_default();
        let name = String::from_utf8_lossy(&self.syms[s].name).into_owned();
        let body = |m: &[u8]| -> Vec<u8> {
            let b = &m[1..];
            let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
            b[..end].to_vec()
        };
        if mac.first().copied().unwrap_or(0) == 0 {
            let b = body(&mac);
            if b.len() >= blen {
                self.yyerror(&format!("too much text in macro expansion: {name}"))?;
                return Ok(None);
            }
            return Ok(Some(b));
        }
        #[allow(clippy::cast_possible_wrap)]
        let nargs = i32::from((mac[0] & !VARMAC) as i8) - 1;
        let dots = mac[0] & VARMAC != 0;
        let mut c = self.getnsc()?;
        if c != ch(b'(') {
            self.unget(c);
            return Ok(None);
        }
        let mut n: i32 = 0;
        let mut args: Vec<Vec<u8>> = Vec::new();
        c = self.getc()?;
        if c != ch(b')') {
            self.unget(c);
            let mut l = 0;
            let mut used = 0usize;
            let ecp = NMACBUF - UTFMAX;
            let mut cur: Vec<u8> = Vec::new();
            n += 1;
            loop {
                if used >= ecp {
                    return self.toobig(&name);
                }
                c = self.getc()?;
                for &q in b"\"'" {
                    if c == ch(q) {
                        loop {
                            if used >= ecp {
                                return self.toobig(&name);
                            }
                            cur.push(byte(c));
                            used += 1;
                            c = self.getc()?;
                            if c == ch(b'\\') {
                                cur.push(byte(c));
                                used += 1;
                                c = self.getc()?;
                                continue;
                            }
                            if c == ch(b'\n') {
                                self.yyerror(&format!("syntax in macro expansion: {name}"))?;
                                return Ok(None);
                            }
                            if c == ch(q) {
                                break;
                            }
                        }
                    }
                }
                if c == ch(b'/') {
                    c = self.getc()?;
                    if c == ch(b'*') {
                        loop {
                            c = self.getc()?;
                            if c == ch(b'*') {
                                c = self.getc()?;
                                if c == ch(b'/') {
                                    break;
                                }
                            }
                        }
                        cur.push(b' ');
                        used += 1;
                        continue;
                    } else if c == ch(b'/') {
                        loop {
                            c = self.getc()?;
                            if c == ch(b'\n') {
                                break;
                            }
                        }
                    } else {
                        self.unget(c);
                        c = ch(b'/');
                    }
                }
                if l == 0 {
                    if c == ch(b',') {
                        if n == nargs && dots {
                            cur.push(b',');
                            used += 1;
                            continue;
                        }
                        args.push(std::mem::take(&mut cur));
                        used += 1;
                        n += 1;
                        if n > nargs {
                            break;
                        }
                        continue;
                    }
                    if c == ch(b')') {
                        break;
                    }
                }
                if c == ch(b'\n') {
                    c = ch(b' ');
                }
                cur.push(byte(c));
                used += 1;
                if c == ch(b'(') {
                    l += 1;
                }
                if c == ch(b')') {
                    l -= 1;
                }
            }
            args.push(cur);
        }
        if n != nargs {
            self.yyerror(&format!("argument mismatch expanding: {name}"))?;
            return Ok(None);
        }
        let mut b: Vec<u8> = Vec::new();
        let eb = blen - 1;
        let mut i = 1;
        loop {
            if b.len() >= eb {
                return self.toobig(&name);
            }
            let mut c = mac.get(i).copied().unwrap_or(0);
            i += 1;
            if c == b'\n' {
                c = b' ';
            }
            if c != b'#' {
                if c == 0 {
                    break;
                }
                b.push(c);
                continue;
            }
            let c = mac.get(i).copied().unwrap_or(0);
            i += 1;
            if c == 0 {
                self.yyerror(&format!("syntax in macro expansion: {name}"))?;
                return Ok(None);
            }
            if c == b'#' {
                b.push(c);
                continue;
            }
            let k = i32::from(c) - i32::from(b'a');
            if k < 0 || k >= n {
                continue;
            }
            let a = &args[usize::try_from(k).unwrap_or(0)];
            if b.len() + a.len() > eb {
                return self.toobig(&name);
            }
            b.extend_from_slice(a);
        }
        Ok(Some(b))
    }

    fn toobig(&mut self, name: &str) -> R<Option<Vec<u8>>> {
        self.yyerror(&format!("too much text in macro expansion: {name}"))?;
        Ok(None)
    }

    /// `macinc`.
    fn macinc(&mut self) -> R<()> {
        let mut c0 = self.getnsc()?;
        let mut c = c0;
        if c0 != ch(b'"') {
            if c0 != ch(b'<') {
                return self.macinc_bad(c);
            }
            c0 = ch(b'>');
        }
        let mut s = Vec::new();
        loop {
            c = self.getc()?;
            if c == c0 {
                break;
            }
            if c == ch(b'\n') {
                return self.macinc_bad(c);
            }
            if s.len() >= STRINGSZ - 1 {
                self.yyerror("name too long for #include")?;
                break;
            }
            s.push(byte(c));
        }
        c = self.getcom()?;
        if c != ch(b'\n') {
            return self.macinc_bad(c);
        }
        let mut found = None;
        for i in 0..self.include.len() {
            if i == 0 && c0 == ch(b'>') {
                continue;
            }
            let mut p = self.include[i].clone();
            p.push(b'/');
            p.extend_from_slice(&s);
            p.truncate(self.arch.nsymb - 1);
            while p.starts_with(b"./") {
                p.drain(..2);
            }
            if let Some(t) = (self.read)(&String::from_utf8_lossy(&p)) {
                found = Some((p, t));
                break;
            }
        }
        match found {
            Some((p, t)) => self.newfile(p, Some(t)),
            None => self.newfile(s, None),
        }
    }

    fn macinc_bad(&mut self, c: i32) -> R<()> {
        self.unget(c);
        self.yyerror("syntax in #include")?;
        self.macend()
    }

    /// `maclin`.
    fn maclin(&mut self) -> R<()> {
        let n = self.getnsn()?;
        let mut c = self.getc()?;
        if n < 0 {
            return self.maclin_bad(c);
        }
        let name;
        loop {
            if c == ch(b' ') || c == ch(b'\t') {
                c = self.getc()?;
                continue;
            }
            if c == ch(b'"') {
                let mut s = Vec::new();
                loop {
                    c = self.getc()?;
                    if c == ch(b'"') {
                        break;
                    }
                    s.push(byte(c));
                }
                c = self.getcom()?;
                if c != ch(b'\n') {
                    return self.maclin_bad(c);
                }
                name = s;
                break;
            }
            if c == ch(b'\n') {
                name = b"<noname>".to_vec();
                break;
            }
            return self.maclin_bad(c);
        }
        #[allow(clippy::cast_possible_truncation)]
        let n = i64::from(n as i32);
        self.linehist(Some(name), n);
        Ok(())
    }

    fn maclin_bad(&mut self, c: i32) -> R<()> {
        self.unget(c);
        self.yyerror("syntax in #line")?;
        self.macend()
    }

    /// `macif`: 0 is `#ifdef`, 1 is `#ifndef` and 2 is `#else`.
    fn macif(&mut self, f: i32) -> R<()> {
        if f != 2 {
            let Some(s) = self.getsym()? else {
                return self.macif_bad();
            };
            if self.getcom()? != ch(b'\n') {
                return self.macif_bad();
            }
            if self.syms[s].macro_.is_some() ^ (f == 1) {
                return Ok(());
            }
        }
        let mut bol = true;
        let mut l = 0;
        loop {
            let c = self.getc()?;
            if c != ch(b'#') {
                if !isspace(c) {
                    bol = false;
                }
                if c == ch(b'\n') {
                    bol = true;
                }
                continue;
            }
            if !bol {
                continue;
            }
            let Some(s) = self.getsym()? else {
                continue;
            };
            let name = self.syms[s].name.as_slice();
            if name == b"endif" {
                if l > 0 {
                    l -= 1;
                    continue;
                }
                return self.macend();
            }
            if name == b"ifdef" || name == b"ifndef" {
                l += 1;
                continue;
            }
            if l == 0 && f != 2 && name == b"else" {
                return self.macend();
            }
        }
    }

    fn macif_bad(&mut self) -> R<()> {
        self.yyerror("syntax in #if(n)def")?;
        self.macend()
    }

    /// `macprag`. Only `#pragma lib` has an effect, an entry of the history.
    fn macprag(&mut self) -> R<()> {
        let s = self.getsym()?;
        let lib = s.is_some_and(|s| self.syms[s].name == b"lib");
        if !lib {
            while self.getnsc()? != ch(b'\n') {}
            return Ok(());
        }
        let mut c0 = self.getnsc()?;
        let mut c = c0;
        if c0 != ch(b'"') {
            if c0 != ch(b'<') {
                return self.macprag_bad(c);
            }
            c0 = ch(b'>');
        }
        let mut name = Vec::new();
        loop {
            c = self.getc()?;
            if c == c0 {
                break;
            }
            if c == ch(b'\n') {
                return self.macprag_bad(c);
            }
            name.push(byte(c));
        }
        c = self.getcom()?;
        if c != ch(b'\n') {
            return self.macprag_bad(c);
        }
        self.hist.push(Hist { name: Some(name), line: self.lineno, offset: -1 });
        Ok(())
    }

    fn macprag_bad(&mut self, c: i32) -> R<()> {
        self.unget(c);
        self.yyerror("syntax in #pragma lib")?;
        self.macend()
    }

    /// `macend`: the rest of the line.
    fn macend(&mut self) -> R<()> {
        loop {
            let c = self.getnsc()?;
            if c < 0 || c == ch(b'\n') {
                return Ok(());
            }
        }
    }

    /// `linehist`. The assemblers never set `newflag`, so a `#line` with the same name as the
    /// last `#line` overwrites it.
    fn linehist(&mut self, f: Option<Vec<u8>>, offset: i64) {
        if let Some(e) = self.hist.last_mut()
            && offset != 0
            && e.offset != 0
            && let (Some(n), Some(en)) = (&f, &e.name)
            && n == en
        {
            e.line = self.lineno;
            e.offset = offset;
            return;
        }
        self.hist.push(Hist { name: f, line: self.lineno, offset });
    }
}

fn at(a: &[Hist], n: i64) -> Hist {
    usize::try_from(n).ok().and_then(|i| a.get(i)).cloned().unwrap_or(Hist {
        name: None,
        line: 0,
        offset: 0,
    })
}

fn set(a: &mut Vec<Hist>, n: i64, h: Hist) {
    let Ok(i) = usize::try_from(n) else {
        return;
    };
    while a.len() <= i {
        a.push(Hist { name: None, line: 0, offset: 0 });
    }
    a[i] = h;
}

/// `strtoull` in base 10, which saturates.
#[allow(clippy::cast_possible_wrap)]
fn strtoull(s: &[u8]) -> i64 {
    let mut v: u64 = 0;
    for &c in s {
        if !c.is_ascii_digit() {
            break;
        }
        match v.checked_mul(10).and_then(|v| v.checked_add(u64::from(c - b'0'))) {
            Some(n) => v = n,
            None => return -1,
        }
    }
    v as i64
}

/// `atof`: the value of the longest prefix that is a number.
fn atof(s: &[u8]) -> f64 {
    let s = String::from_utf8_lossy(s);
    let mut end = s.len();
    while end > 0 {
        if let Ok(v) = s[..end].parse::<f64>() {
            return v;
        }
        end -= 1;
    }
    0.0
}

/// `frexp`: the fraction in [0.5, 1) and the exponent.
fn frexp(x: f64) -> (f64, i32) {
    if x == 0.0 || !x.is_finite() {
        return (x, 0);
    }
    let (x, adj) = if x.abs() < f64::MIN_POSITIVE { (x * 2f64.powi(54), -54) } else { (x, 0) };
    let bits = x.to_bits();
    #[allow(clippy::cast_possible_truncation)]
    let e = ((bits >> 52) & 0x7ff) as i32;
    let fr = f64::from_bits((bits & !(0x7ff << 52)) | (1022 << 52));
    (fr, e - 1022 + adj)
}

/// `modf`: the fraction and the integer part.
fn modf(x: f64) -> (f64, f64) {
    let i = x.trunc();
    (x - i, i)
}

/// `ieeedtod`: the low and the high word of a double, as the assemblers compute it.
#[allow(clippy::cast_possible_truncation)]
pub(super) fn ieeedtod(native: f64) -> (i32, i32) {
    if native < 0.0 {
        let (l, h) = ieeedtod(-native);
        return (l, h | i32::MIN);
    }
    if native == 0.0 {
        return (0, 0);
    }
    let (fr, exp) = frexp(native);
    let f = 2_097_152.0;
    let (fr, ho) = modf(fr * f);
    let mut h = ho as i32;
    h &= 0xfffff;
    h |= exp.wrapping_add(1022).wrapping_shl(20);
    let f = 65536.0;
    let (fr, ho) = modf(fr * f);
    let mut l = ho as i32;
    l = l.wrapping_shl(16);
    l |= (fr * f) as i32;
    (l, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ieeedtod_is_the_bits_of_a_normal_double() {
        for v in [1.0f64, 0.5, 3.25, 1e300, 1.0e-300, 123_456.789, f64::MAX] {
            let (l, h) = ieeedtod(v);
            let bits = v.to_bits();
            assert_eq!(l as u32, bits as u32, "{v}");
            assert_eq!(h as u32, (bits >> 32) as u32, "{v}");
        }
        let (l, h) = ieeedtod(-2.0);
        assert_eq!((l, h as u32), (0, 0xc000_0000));
    }

    #[test]
    fn strtoull_saturates() {
        assert_eq!(strtoull(b"18446744073709551615"), -1);
        assert_eq!(strtoull(b"18446744073709551616"), -1);
        assert_eq!(strtoull(b"42"), 42);
    }

    #[test]
    fn atof_takes_the_longest_number() {
        assert!((atof(b"1e") - 1.0).abs() < f64::EPSILON);
        assert!((atof(b"2.5e+") - 2.5).abs() < f64::EPSILON);
        assert!((atof(b".5") - 0.5).abs() < f64::EPSILON);
    }
}
