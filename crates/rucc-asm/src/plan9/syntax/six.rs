//! The assembler `6a`: the grammar of `6a/a.y` and the object of `6a/lex.c`.
//!
//! The parser is a recursive descent parser. It reads a token at each point where the parser of
//! yacc reads one, and at no other point. This is necessary because the line of each record is
//! the value of `lineno` when the parser reduces the instruction. When the parser must look at the
//! token after the last operand, it reads the newline, and the record gets the number of the next
//! line. The comment at each function tells where the parser reads the next token.

use rucc_object::plan9::amd64 as a6;

use super::itab6::{
    ITAB, LBREG, LFP, LFREG, LLREG, LMREG, LPC, LSB, LSP, LSREG, LTYPE0, LTYPE1, LTYPE2, LTYPE3,
    LTYPE4, LTYPEC, LTYPED, LTYPEI, LTYPEM, LTYPEN, LTYPER, LTYPERT, LTYPES, LTYPET, LTYPEX,
    LTYPEXC, LXREG,
};
use super::lex::{
    Arch, EOF, Fatal, Hist, LCONST, LFCONST, LLAB, LNAME, LSCONST, LVAR, Lexer, NSYM, R, Reader,
    Tok, ieeedtod,
};

const D_SP: i16 = 20;
const D_NONE: i16 = 111;
const D_BRANCH: i16 = 112;
const D_STATIC: i16 = 114;
const D_AUTO: i16 = 115;
const D_CONST: i16 = 117;
const D_FCONST: i16 = 118;
const D_SCONST: i16 = 119;
const D_ADDR: i16 = 120;
const D_FILE: u8 = 121;
const D_INDIR: i16 = 123;

const T_TYPE: u8 = 1 << 0;
const T_INDEX: u8 = 1 << 1;
const T_OFFSET: u8 = 1 << 2;
const T_FCONST: u8 = 1 << 3;
const T_SYM: u8 = 1 << 4;
const T_SCONST: u8 = 1 << 5;
const T_64: u8 = 1 << 6;

const AMODE: i32 = 664;

const ARCH: Arch = Arch { thechar: b'6', nsymb: 500, include: "/amd64/include", value32: false };

/// An operand, the `Gen` of `6a`.
#[derive(Debug, Clone, Copy)]
struct Gen {
    dval: f64,
    sval: [u8; 8],
    offset: i64,
    sym: Option<usize>,
    typ: i16,
    index: i16,
    scale: i16,
}

const NULLGEN: Gen =
    Gen { dval: 0.0, sval: [0; 8], offset: 0, sym: None, typ: D_NONE, index: D_NONE, scale: 0 };

fn gen_type(typ: i16) -> Gen {
    Gen { typ, ..NULLGEN }
}

/// The two operands of an instruction.
#[derive(Debug, Clone, Copy)]
struct Gen2 {
    from: Gen,
    to: Gen,
}

const NULL2: Gen2 = Gen2 { from: NULLGEN, to: NULLGEN };

/// The stop of the parse of a line.
#[derive(Debug)]
enum Stop {
    /// A syntax error. The parser skips to the end of the line.
    Syntax,
    /// The stop of the assembler.
    Fatal,
}

impl From<Fatal> for Stop {
    fn from(_: Fatal) -> Self {
        Self::Fatal
    }
}

type P<T> = Result<T, Stop>;

/// A short value, as C stores a `vlong` in a `short`.
#[allow(clippy::cast_possible_truncation)]
fn short(v: i64) -> i16 {
    v as i16
}

const fn ch(c: u8) -> i32 {
    c as i32
}

fn is_reg(t: i32) -> bool {
    matches!(t, LBREG | LFREG | LLREG | LMREG | LSP | LSREG | LXREG)
}

fn is_con(t: i32) -> bool {
    t == LCONST || t == LVAR || b"-+~(".iter().any(|&c| t == ch(c))
}

fn is_rem(t: i32) -> bool {
    is_reg(t) || t == LNAME || is_con(t)
}

struct Six<'a> {
    lx: Lexer<'a>,
    la: Option<Tok>,
    pass: i32,
    pc: i32,
    h: [(Option<usize>, i16); NSYM],
    sym: i32,
    out: Vec<u8>,
    errflag: u8,
    pathname: Vec<u8>,
}

impl Six<'_> {
    fn peek(&mut self) -> P<Tok> {
        if let Some(t) = self.la {
            return Ok(t);
        }
        let t = self.lx.yylex()?;
        self.la = Some(t);
        Ok(t)
    }

    /// Reads the next token, which is a shift of yacc.
    fn next(&mut self) -> P<Tok> {
        let t = match self.la.take() {
            Some(t) => t,
            None => self.lx.yylex()?,
        };
        self.errflag = self.errflag.saturating_sub(1);
        Ok(t)
    }

    /// Reads the token `t`, which must be the next token.
    fn expect(&mut self, t: i32) -> P<Tok> {
        if self.peek()?.t == t {
            return self.next();
        }
        Err(Stop::Syntax)
    }

    fn expect_c(&mut self, c: u8) -> P<Tok> {
        self.expect(ch(c))
    }

    fn error(&mut self, msg: &str) -> P<()> {
        Ok(self.lx.yyerror(msg)?)
    }

    fn name(&self, s: usize) -> String {
        String::from_utf8_lossy(&self.lx.syms[s].name).into_owned()
    }

    /// `prog`. The parser reads a token before each line.
    fn prog(&mut self) -> R<()> {
        loop {
            if self.peek().map_err(|_| Fatal)?.t == EOF {
                return Ok(());
            }
            match self.line() {
                Ok(()) => {}
                Err(Stop::Fatal) => return Err(Fatal),
                // The recovery of yacc: the rule `error ';'`. An error before three shifts after
                // the last error has no message.
                Err(Stop::Syntax) => {
                    if self.errflag == 0 {
                        self.lx.syntax_error()?;
                    }
                    self.errflag = 3;
                    loop {
                        let t = self.peek().map_err(|_| Fatal)?;
                        if t.t == EOF {
                            break;
                        }
                        self.la = None;
                        if t.t == ch(b';') {
                            self.errflag -= 1;
                            break;
                        }
                    }
                }
            }
        }
    }

    /// `line`. A label is a line followed by a line, and its action comes before the parser
    /// reads the next token.
    fn line(&mut self) -> P<()> {
        loop {
            let t = self.peek()?;
            match t.t {
                LLAB => {
                    self.next()?;
                    self.expect_c(b':')?;
                    let s = t.sym;
                    if self.lx.syms[s].value != i64::from(self.pc) {
                        let n = self.name(s);
                        self.error(&format!("redeclaration of {n}"))?;
                    }
                    self.lx.syms[s].value = i64::from(self.pc);
                }
                LNAME => {
                    self.next()?;
                    let s = t.sym;
                    let u = self.peek()?;
                    if u.t == ch(b':') {
                        self.next()?;
                        self.lx.syms[s].typ = LLAB;
                        self.lx.syms[s].value = i64::from(self.pc);
                        continue;
                    }
                    self.expect_c(b'=')?;
                    let v = self.expr(1)?;
                    self.lx.syms[s].typ = LVAR;
                    self.lx.syms[s].value = v;
                    self.expect_c(b';')?;
                    return Ok(());
                }
                LVAR => {
                    self.next()?;
                    self.expect_c(b'=')?;
                    let v = self.expr(1)?;
                    let s = t.sym;
                    if self.lx.syms[s].value != v {
                        let n = self.name(s);
                        self.error(&format!("redeclaration of {n}"))?;
                    }
                    self.lx.syms[s].value = v;
                    self.expect_c(b';')?;
                    return Ok(());
                }
                _ if t.t == ch(b';') => {
                    self.next()?;
                    return Ok(());
                }
                LTYPE0 | LTYPE1 | LTYPE2 | LTYPE3 | LTYPE4 | LTYPEC | LTYPED | LTYPEN | LTYPER
                | LTYPET | LTYPES | LTYPEM | LTYPEI | LTYPEXC | LTYPEX | LTYPERT => {
                    self.next()?;
                    let g = self.inst(t.t)?;
                    #[allow(clippy::cast_possible_truncation)]
                    self.outcode(t.lval as i32, &g)?;
                    self.expect_c(b';')?;
                    return Ok(());
                }
                _ => return Err(Stop::Syntax),
            }
        }
    }

    /// The operands of an instruction.
    #[allow(clippy::too_many_lines)]
    fn inst(&mut self, ty: i32) -> P<Gen2> {
        Ok(match ty {
            // nonnon: the parser reads a token to see the comma.
            LTYPE0 => {
                if self.peek()?.t == ch(b',') {
                    self.next()?;
                }
                NULL2
            }
            // nonrem
            LTYPE1 => {
                if self.peek()?.t == ch(b',') {
                    self.next()?;
                }
                Gen2 { from: NULLGEN, to: self.rem()? }
            }
            // rimnon: the parser reads a token after rim to see the comma.
            LTYPE2 => {
                let from = self.rim()?;
                if self.peek()?.t == ch(b',') {
                    self.next()?;
                }
                Gen2 { from, to: NULLGEN }
            }
            LTYPE3 => {
                let from = self.rim()?;
                self.expect_c(b',')?;
                Gen2 { from, to: self.rem()? }
            }
            LTYPE4 => {
                let from = self.rem()?;
                self.expect_c(b',')?;
                Gen2 { from, to: self.rim()? }
            }
            // nonrel
            LTYPER => {
                if self.peek()?.t == ch(b',') {
                    self.next()?;
                }
                Gen2 { from: NULLGEN, to: self.rel()? }
            }
            // spec1, DATA
            LTYPED => {
                let mut from = self.nam()?;
                self.expect_c(b'/')?;
                from.scale = short(self.con()?);
                self.expect_c(b',')?;
                Gen2 { from, to: self.imm()? }
            }
            // spec2, TEXT
            LTYPET => {
                let mut from = self.mem()?;
                self.expect_c(b',')?;
                if self.peek()?.t != ch(b'$') {
                    from.scale = short(self.con()?);
                    self.expect_c(b',')?;
                }
                Gen2 { from, to: self.imm()? }
            }
            // spec3, JMP and CALL
            LTYPEC => {
                if self.peek()?.t == ch(b',') {
                    self.next()?;
                }
                Gen2 { from: NULLGEN, to: self.rom()? }
            }
            // spec4, NOP: nonnon or nonrem
            LTYPEN => {
                if self.peek()?.t == ch(b',') {
                    self.next()?;
                }
                if is_rem(self.peek()?.t) { Gen2 { from: NULLGEN, to: self.rem()? } } else { NULL2 }
            }
            // spec5, SHL and SHR: the parser reads a token after rem to see the colon.
            LTYPES => {
                let mut g = self.rim_rem()?;
                if self.peek()?.t == ch(b':') {
                    self.next()?;
                    let r = self.expect(LLREG)?;
                    if g.from.index != D_NONE {
                        self.error("dp shift with lhs index")?;
                    }
                    g.from.index = short(r.lval);
                }
                g
            }
            // spec6, MOVW and MOVL
            LTYPEM => {
                let mut g = self.rim_rem()?;
                if self.peek()?.t == ch(b':') {
                    self.next()?;
                    let r = self.expect(LSREG)?;
                    if g.to.index != D_NONE {
                        self.error("dp move with lhs index")?;
                    }
                    g.to.index = short(r.lval);
                }
                g
            }
            // spec7
            LTYPEI => {
                let from = self.rim()?;
                let mut to = NULLGEN;
                if self.peek()?.t == ch(b',') {
                    self.next()?;
                    if is_rem(self.peek()?.t) {
                        to = self.rem()?;
                    }
                }
                Gen2 { from, to }
            }
            // spec8, CMPPS and CMPPD
            LTYPEXC => {
                let mut from = self.reg()?;
                self.expect_c(b',')?;
                let to = self.rem()?;
                self.expect_c(b',')?;
                from.offset = self.con()?;
                Gen2 { from, to }
            }
            // spec9, the shuffles
            LTYPEX => {
                let i = self.imm()?;
                self.expect_c(b',')?;
                let from = self.rem()?;
                self.expect_c(b',')?;
                let mut to = self.reg()?;
                if i.typ != D_CONST {
                    self.error("illegal constant")?;
                }
                to.offset = i.offset;
                Gen2 { from, to }
            }
            // spec10, RET and RETF: the parser reads a token to see the dollar.
            LTYPERT => {
                if self.peek()?.t == ch(b'$') {
                    Gen2 { from: self.imm()?, to: NULLGEN }
                } else {
                    NULL2
                }
            }
            _ => return Err(Stop::Syntax),
        })
    }

    fn rim_rem(&mut self) -> P<Gen2> {
        let from = self.rim()?;
        self.expect_c(b',')?;
        Ok(Gen2 { from, to: self.rem()? })
    }

    /// `reg`. The parser does not read the next token.
    fn reg(&mut self) -> P<Gen> {
        let t = self.peek()?;
        if !is_reg(t.t) {
            return Err(Stop::Syntax);
        }
        self.next()?;
        Ok(if t.t == LSP { gen_type(D_SP) } else { gen_type(short(t.lval)) })
    }

    /// `rem`: reg or mem.
    fn rem(&mut self) -> P<Gen> {
        if is_reg(self.peek()?.t) { self.reg() } else { self.mem() }
    }

    /// `rim`: rem or imm.
    fn rim(&mut self) -> P<Gen> {
        if self.peek()?.t == ch(b'$') { self.imm() } else { self.rem() }
    }

    /// `mem`: omem or nmem.
    fn mem(&mut self) -> P<Gen> {
        let t = self.peek()?.t;
        if t == LNAME {
            return self.nmem();
        }
        if is_con(t) {
            return self.omem(false);
        }
        Err(Stop::Syntax)
    }

    /// `rom`, the operand of a jump.
    fn rom(&mut self) -> P<Gen> {
        let t = self.peek()?;
        if t.t == ch(b'*') {
            self.next()?;
            if is_reg(self.peek()?.t) {
                return self.reg();
            }
            if is_con(self.peek()?.t) {
                return self.omem(false);
            }
            return Err(Stop::Syntax);
        }
        if is_reg(t.t) {
            return self.reg();
        }
        match t.t {
            LNAME => {
                self.next()?;
                if self.peek()?.t == ch(b'<') {
                    let g = self.nam_static(t.sym)?;
                    return self.nmem_index(g);
                }
                let off = self.offset()?;
                // The parser reads a token after the offset to see the parenthesis of nam.
                if self.peek()?.t == ch(b'(') {
                    let g = self.nam_pointer(t.sym, off)?;
                    return self.nmem_index(g);
                }
                self.rel_name(t.sym, off)
            }
            LLAB => {
                self.next()?;
                let off = self.offset()?;
                Ok(self.rel_label(t.sym, off))
            }
            _ if is_con(t.t) => self.omem(true),
            _ => Err(Stop::Syntax),
        }
    }

    /// `rel`, the operand of a conditional jump.
    fn rel(&mut self) -> P<Gen> {
        let t = self.peek()?;
        match t.t {
            LNAME => {
                self.next()?;
                let off = self.offset()?;
                self.rel_name(t.sym, off)
            }
            LLAB => {
                self.next()?;
                let off = self.offset()?;
                Ok(self.rel_label(t.sym, off))
            }
            _ if is_con(t.t) => {
                let v = self.con()?;
                self.expect_c(b'(')?;
                self.rel_pc(v)
            }
            _ => Err(Stop::Syntax),
        }
    }

    /// The rest of `con '(' LPC ')'`, after the parenthesis.
    fn rel_pc(&mut self, v: i64) -> P<Gen> {
        self.expect(LPC)?;
        self.expect_c(b')')?;
        Ok(Gen { typ: D_BRANCH, offset: v.wrapping_add(i64::from(self.pc)), ..NULLGEN })
    }

    fn rel_name(&mut self, s: usize, off: i64) -> P<Gen> {
        if self.pass == 2 {
            let n = self.name(s);
            self.error(&format!("undefined label: {n}"))?;
        }
        Ok(Gen { typ: D_BRANCH, sym: Some(s), offset: off, ..NULLGEN })
    }

    fn rel_label(&self, s: usize, off: i64) -> Gen {
        let v = self.lx.syms[s].value.wrapping_add(off);
        Gen { typ: D_BRANCH, sym: Some(s), offset: v, ..NULLGEN }
    }

    /// `imm`. The parser does not read the next token.
    fn imm(&mut self) -> P<Gen> {
        self.expect_c(b'$')?;
        let t = self.peek()?;
        match t.t {
            LSCONST => {
                self.next()?;
                Ok(Gen { typ: D_SCONST, sval: t.sval, ..NULLGEN })
            }
            LFCONST => {
                self.next()?;
                Ok(Gen { typ: D_FCONST, dval: t.dval, ..NULLGEN })
            }
            LNAME => {
                let mut g = self.nam()?;
                g.index = g.typ;
                g.typ = D_ADDR;
                Ok(g)
            }
            _ if t.t == ch(b'(') => {
                self.next()?;
                let u = self.peek()?;
                if u.t == LFCONST {
                    self.next()?;
                    self.expect_c(b')')?;
                    return Ok(Gen { typ: D_FCONST, dval: u.dval, ..NULLGEN });
                }
                let v = self.expr(1)?;
                self.expect_c(b')')?;
                Ok(Gen { typ: D_CONST, offset: v, ..NULLGEN })
            }
            _ if t.t == ch(b'-') => {
                self.next()?;
                let u = self.peek()?;
                if u.t == LFCONST {
                    self.next()?;
                    return Ok(Gen { typ: D_FCONST, dval: -u.dval, ..NULLGEN });
                }
                let v = self.con()?;
                Ok(Gen { typ: D_CONST, offset: v.wrapping_neg(), ..NULLGEN })
            }
            _ => {
                let v = self.con()?;
                Ok(Gen { typ: D_CONST, offset: v, ..NULLGEN })
            }
        }
    }

    /// `omem`. With `rel`, the operand can also be `con '(' LPC ')'`.
    fn omem(&mut self, rel: bool) -> P<Gen> {
        if self.peek()?.t == ch(b'(') {
            self.next()?;
            let u = self.peek()?;
            if u.t == LLREG {
                self.next()?;
                return self.omem_reg(0, u.lval);
            }
            if u.t == LSP {
                self.next()?;
                self.expect_c(b')')?;
                return Ok(gen_type(D_INDIR + D_SP));
            }
            let v = self.expr(1)?;
            self.expect_c(b')')?;
            return self.omem_con(v, rel);
        }
        let v = self.con()?;
        self.omem_con(v, rel)
    }

    /// The rest of omem after con. The parser reads a token to see the parenthesis.
    fn omem_con(&mut self, v: i64, rel: bool) -> P<Gen> {
        if self.peek()?.t != ch(b'(') {
            return Ok(Gen { typ: D_INDIR + D_NONE, offset: v, ..NULLGEN });
        }
        self.next()?;
        let u = self.peek()?;
        if u.t == LLREG {
            self.next()?;
            return self.omem_reg(v, u.lval);
        }
        if u.t == LSP {
            self.next()?;
            self.expect_c(b')')?;
            return Ok(Gen { typ: D_INDIR + D_SP, offset: v, ..NULLGEN });
        }
        if rel && u.t == LPC {
            return self.rel_pc(v);
        }
        Err(Stop::Syntax)
    }

    /// The rest of omem after `con '(' LLREG` or `'(' LLREG`.
    fn omem_reg(&mut self, v: i64, r: i64) -> P<Gen> {
        let t = self.peek()?;
        if t.t == ch(b'*') {
            self.next()?;
            let scale = short(self.con()?);
            self.expect_c(b')')?;
            let g = Gen { typ: D_INDIR + D_NONE, offset: v, index: short(r), scale, ..NULLGEN };
            self.checkscale(scale)?;
            return Ok(g);
        }
        self.expect_c(b')')?;
        let typ = short(i64::from(D_INDIR).wrapping_add(r));
        // The parser reads a token to see a second group.
        if self.peek()?.t != ch(b'(') {
            return Ok(Gen { typ, offset: v, ..NULLGEN });
        }
        self.next()?;
        let x = self.expect(LLREG)?;
        self.expect_c(b'*')?;
        let scale = short(self.con()?);
        self.expect_c(b')')?;
        self.checkscale(scale)?;
        Ok(Gen { typ, offset: v, index: short(x.lval), scale, ..NULLGEN })
    }

    fn checkscale(&mut self, scale: i16) -> P<()> {
        if matches!(scale, 1 | 2 | 4 | 8) {
            return Ok(());
        }
        self.error(&format!("scale must be 1248: {scale}"))
    }

    /// `nmem`. The parser reads a token after nam to see an index.
    fn nmem(&mut self) -> P<Gen> {
        let g = self.nam()?;
        self.nmem_index(g)
    }

    fn nmem_index(&mut self, mut g: Gen) -> P<Gen> {
        if self.peek()?.t != ch(b'(') {
            return Ok(g);
        }
        self.next()?;
        let r = self.expect(LLREG)?;
        self.expect_c(b'*')?;
        let scale = short(self.con()?);
        self.expect_c(b')')?;
        g.index = short(r.lval);
        g.scale = scale;
        self.checkscale(scale)?;
        Ok(g)
    }

    /// `nam`.
    fn nam(&mut self) -> P<Gen> {
        let t = self.expect(LNAME)?;
        if self.peek()?.t == ch(b'<') {
            return self.nam_static(t.sym);
        }
        let off = self.offset()?;
        self.nam_pointer(t.sym, off)
    }

    fn nam_static(&mut self, s: usize) -> P<Gen> {
        self.expect_c(b'<')?;
        self.expect_c(b'>')?;
        let offset = self.offset()?;
        self.expect_c(b'(')?;
        self.expect(LSB)?;
        self.expect_c(b')')?;
        Ok(Gen { typ: D_STATIC, sym: Some(s), offset, ..NULLGEN })
    }

    /// The rest of nam after the offset.
    fn nam_pointer(&mut self, s: usize, offset: i64) -> P<Gen> {
        self.expect_c(b'(')?;
        let t = self.peek()?;
        let typ = match t.t {
            LSB | LFP => short(t.lval),
            LSP => D_AUTO,
            _ => return Err(Stop::Syntax),
        };
        self.next()?;
        self.expect_c(b')')?;
        Ok(Gen { typ, sym: Some(s), offset, ..NULLGEN })
    }

    /// `offset`. The parser reads a token to see a sign.
    fn offset(&mut self) -> P<i64> {
        let t = self.peek()?;
        if t.t == ch(b'+') {
            self.next()?;
            return self.con();
        }
        if t.t == ch(b'-') {
            self.next()?;
            return Ok(self.con()?.wrapping_neg());
        }
        Ok(0)
    }

    /// `con`. The parser does not read the next token.
    fn con(&mut self) -> P<i64> {
        let t = self.peek()?;
        match t.t {
            LCONST => {
                self.next()?;
                Ok(t.lval)
            }
            LVAR => {
                self.next()?;
                Ok(self.lx.syms[t.sym].value)
            }
            _ if t.t == ch(b'-') => {
                self.next()?;
                Ok(self.con()?.wrapping_neg())
            }
            _ if t.t == ch(b'+') => {
                self.next()?;
                self.con()
            }
            _ if t.t == ch(b'~') => {
                self.next()?;
                Ok(!self.con()?)
            }
            _ if t.t == ch(b'(') => {
                self.next()?;
                let v = self.expr(1)?;
                self.expect_c(b')')?;
                Ok(v)
            }
            _ => Err(Stop::Syntax),
        }
    }

    /// `expr`, with the precedence of `a.y`. All the operators are left associative.
    fn expr(&mut self, min: u8) -> P<i64> {
        let mut l = self.con()?;
        loop {
            let t = self.peek()?.t;
            let p = match u8::try_from(t).unwrap_or(0) {
                b'|' => 1,
                b'^' => 2,
                b'&' => 3,
                b'<' | b'>' => 4,
                b'+' | b'-' => 5,
                b'*' | b'/' | b'%' => 6,
                _ => return Ok(l),
            };
            if p < min {
                return Ok(l);
            }
            self.next()?;
            if p == 4 {
                self.expect(t)?;
            }
            let r = self.expr(p + 1)?;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let n = r as u32;
            l = match u8::try_from(t).unwrap_or(0) {
                b'|' => l | r,
                b'^' => l ^ r,
                b'&' => l & r,
                b'<' => l.wrapping_shl(n),
                b'>' => l.wrapping_shr(n),
                b'+' => l.wrapping_add(r),
                b'-' => l.wrapping_sub(r),
                b'*' => l.wrapping_mul(r),
                b'/' => match l.checked_div(r) {
                    Some(v) => v,
                    None => return self.trap(),
                },
                _ => match l.checked_rem(r) {
                    Some(v) => v,
                    None => return self.trap(),
                },
            };
        }
    }

    /// A division by zero, where `6a` stops with a trap.
    fn trap(&mut self) -> P<i64> {
        self.lx.messages.push_str("6a: sys: trap: divide error\n");
        self.lx.nerrors += 1;
        Err(Stop::Fatal)
    }

    // The object, from `6a/lex.c`.

    fn put(&mut self, b: &[u8]) {
        self.out.extend_from_slice(b);
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn put16(&mut self, v: i32) {
        self.put(&[v as u8, (v >> 8) as u8]);
    }

    fn put32(&mut self, v: i32) {
        self.put(&v.to_le_bytes());
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn zname(&mut self, s: usize, t: i16, slot: i32) {
        self.put16(i32::from(a6::ANAME));
        self.put(&[t as u8, slot as u8]);
        let name = self.lx.syms[s].name.clone();
        self.put(&name);
        self.put(&[0]);
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn zaddr(&mut self, a: &Gen, s: i32) {
        let mut t = 0u8;
        if a.index != D_NONE || a.scale != 0 {
            t |= T_INDEX;
        }
        let l = a.offset as i32;
        if a.offset != 0 {
            t |= T_OFFSET;
            if i64::from(l) != a.offset {
                t |= T_64;
            }
        }
        if s != 0 {
            t |= T_SYM;
        }
        match a.typ {
            D_FCONST => t |= T_FCONST,
            D_SCONST => t |= T_SCONST,
            D_NONE => {}
            _ => t |= T_TYPE,
        }
        self.put(&[t]);
        if t & T_INDEX != 0 {
            self.put(&[a.index as u8, a.scale as u8]);
        }
        if t & T_OFFSET != 0 {
            self.put32(l);
            if t & T_64 != 0 {
                self.put32((a.offset >> 32) as i32);
            }
        }
        if t & T_SYM != 0 {
            self.put(&[s as u8]);
        }
        if t & T_FCONST != 0 {
            let (l, h) = ieeedtod(a.dval);
            self.put32(l);
            self.put32(h);
            return;
        }
        if t & T_SCONST != 0 {
            self.put(&a.sval);
            return;
        }
        if t & T_TYPE != 0 {
            self.put(&[a.typ as u8]);
        }
    }

    /// The slot of the symbol of `g`, and a new slot with `ANAME` when it is not in the cache.
    fn slot(&mut self, g: &Gen) -> i32 {
        let Some(s) = g.sym else {
            return 0;
        };
        let mut sf = i32::from(self.lx.syms[s].slot);
        if !(0..NSYM as i32).contains(&sf) {
            sf = 0;
        }
        let t = if g.typ == D_ADDR { g.index } else { g.typ };
        let e = self.h[usize::try_from(sf).unwrap_or(0)];
        if e.1 == t && e.0 == Some(s) {
            return sf;
        }
        let sym = self.sym;
        self.zname(s, t, sym);
        #[allow(clippy::cast_possible_truncation)]
        {
            self.lx.syms[s].slot = sym as i8;
        }
        self.h[usize::try_from(sym).unwrap_or(0)] = (Some(s), t);
        self.sym += 1;
        if self.sym >= NSYM as i32 {
            self.sym = 1;
        }
        sym
    }

    /// `outcode`.
    fn outcode(&mut self, a: i32, g: &Gen2) -> R<()> {
        if self.pass != 1 {
            let (sf, st) = loop {
                let sf = self.slot(&g.from);
                let before = self.sym;
                let st = self.slot(&g.to);
                // `jackpot`: the new slot of `to` took the slot of `from`.
                if st != 0 && self.sym != before && st == sf {
                    continue;
                }
                break (sf, st);
            };
            self.put16(a);
            self.put32(self.lx.lineno);
            self.zaddr(&g.from, sf);
            self.zaddr(&g.to, st);
        }
        if a != i32::from(a6::AGLOBL) && a != i32::from(a6::ADATA) && a != AMODE {
            self.pc = self.pc.wrapping_add(1);
        }
        Ok(())
    }

    /// `cclean`.
    fn cclean(&mut self) -> R<()> {
        self.outcode(i32::from(a6::AEND), &NULL2)
    }

    /// `outhist`.
    fn outhist(&mut self) {
        let hist: Vec<Hist> = self.lx.hist.clone();
        for h in &hist {
            let mut parts: Vec<&[u8]> = Vec::new();
            if let Some(p) = h.name.as_deref() {
                if p.first() != Some(&b'/') && h.offset == 0 && self.pathname.first() == Some(&b'/')
                {
                    parts.push(&self.pathname);
                }
                parts.push(p);
            }
            let mut names: Vec<Vec<u8>> = Vec::new();
            for p in parts {
                let mut rest = p;
                loop {
                    match rest.iter().position(|&c| c == b'/') {
                        Some(0) => {
                            names.push(b"/".to_vec());
                            rest = &rest[1..];
                        }
                        Some(n) => {
                            names.push(rest[..n].to_vec());
                            rest = &rest[n + 1..];
                        }
                        None => {
                            if !rest.is_empty() {
                                names.push(rest.to_vec());
                            }
                            break;
                        }
                    }
                }
            }
            for n in names {
                self.put16(i32::from(a6::ANAME));
                self.put(&[D_FILE, 1, b'<']);
                self.put(&n);
                self.put(&[0]);
            }
            self.put16(i32::from(a6::AHISTORY));
            self.put32(h.line);
            self.zaddr(&NULLGEN, 0);
            let g = Gen { offset: h.offset, ..NULLGEN };
            self.zaddr(&g, 0);
        }
    }

    /// `pinit`, with the state of the parser.
    fn pinit(&mut self, file: &[u8], defines: &[String]) -> R<()> {
        self.lx.pinit(file)?;
        self.la = None;
        self.errflag = 0;
        self.pc = 0;
        self.sym = 1;
        self.h = [(None, 0); NSYM];
        for d in defines {
            self.lx.dodefine(d.as_bytes())?;
        }
        Ok(())
    }

    fn run(&mut self, file: &str, defines: &[String], includes: &[String]) -> R<()> {
        if let Some(i) = file.rfind('/') {
            self.lx.set_dir(&file.as_bytes()[..i]);
        }
        for i in includes {
            self.lx.setinclude(i.as_bytes())?;
        }
        self.lx.machine_include()?;
        self.pass = 1;
        self.pinit(file.as_bytes(), defines)?;
        self.prog()?;
        if self.lx.nerrors != 0 {
            return self.cclean();
        }
        self.pass = 2;
        self.outhist();
        self.pinit(file.as_bytes(), defines)?;
        self.prog()?;
        self.cclean()
    }
}

/// Assembles `file` as `6a` does, and returns the object.
///
/// The name `file` is the argument of `6a`, and `cwd` is the directory where `6a` runs, which goes
/// into the history of the object. The `defines` are the arguments of `-D` and the `includes` are
/// the arguments of `-I`, in their order. The function `read` reads a file from its name, as `6a`
/// gives it to `open`. On an error, the result has the messages of `6a`.
///
/// # Errors
///
/// The messages, when `6a` writes no object.
pub fn assemble(
    file: &str,
    cwd: &str,
    defines: &[String],
    includes: &[String],
    read: Reader<'_>,
) -> Result<Vec<u8>, String> {
    let mut lx = Lexer::new(ARCH, read);
    for &(name, typ, value) in ITAB {
        lx.define(name, typ, value);
    }
    let mut six = Six {
        lx,
        la: None,
        pass: 1,
        pc: 0,
        h: [(None, 0); NSYM],
        sym: 1,
        out: Vec::new(),
        errflag: 0,
        pathname: cwd.as_bytes().to_vec(),
    };
    let r = six.run(file, defines, includes);
    if r.is_err() || six.lx.nerrors != 0 {
        return Err(six.lx.messages);
    }
    Ok(six.out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asm(text: &str) -> Result<Vec<u8>, String> {
        let text = text.as_bytes().to_vec();
        let read = move |n: &str| if n == "t.s" { Some(text.clone()) } else { None };
        assemble("t.s", "/usr/glenda", &[], &[], &read)
    }

    #[test]
    fn an_empty_file_has_the_history_and_the_end() {
        let o = asm("").unwrap();
        let mut want = Vec::new();
        for n in ["/", "usr", "glenda", "t.s"] {
            want.extend_from_slice(&[132, 0, 121, 1, b'<']);
            want.extend_from_slice(n.as_bytes());
            want.push(0);
        }
        want.extend_from_slice(&[56, 0, 1, 0, 0, 0, 0, 0]);
        want.extend_from_slice(&[56, 0, 1, 0, 0, 0, 0, 0]);
        want.extend_from_slice(&[0x4f, 1, 1, 0, 0, 0, 0, 0]);
        assert_eq!(o, want);
    }

    fn op(name: &str) -> u8 {
        let v = ITAB.iter().find(|r| r.0 == name).unwrap().2;
        u8::try_from(v & 0xff).unwrap()
    }

    #[test]
    fn the_line_of_a_record_follows_the_lookahead() {
        // `RET` and `MOVQ` read the newline to see a comma or a colon, so their records have the
        // number of the next line. `ADDQ AX, BX` ends in a register and does not read it.
        let o = asm("RET\nMOVQ AX, BX\nADDQ AX, BX\n").unwrap();
        let n = o.len();
        assert_eq!(&o[n - 8..], [0x4f, 1, 4, 0, 0, 0, 0, 0]);
        let add = &o[n - 18..n - 8];
        assert_eq!(add[0], op("ADDQ"));
        assert_eq!(&add[2..], [3, 0, 0, 0, 1, 16, 1, 19]);
        let mov = &o[n - 28..n - 18];
        assert_eq!(mov[0], op("MOVQ"));
        assert_eq!(&mov[2..], [3, 0, 0, 0, 1, 16, 1, 19]);
        let ret = &o[n - 36..n - 28];
        assert_eq!(ret[0], op("RET"));
        assert_eq!(&ret[2..], [2, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn the_object_is_the_object_of_the_c_program() {
        // `t1.6` is the output of `6a -DDEFD=5 -DVAL -I/tmp/r9syn/sub t1.s` in `/tmp/r9syn`.
        let read = |n: &str| -> Option<Vec<u8>> {
            Some(match n {
                "t1.s" => include_bytes!("testdata/t1.s").to_vec(),
                "inc.h" => include_bytes!("testdata/inc.h").to_vec(),
                "sub/deep.h" => include_bytes!("testdata/sub/deep.h").to_vec(),
                _ => return None,
            })
        };
        let defines = ["DEFD=5".to_string(), "VAL".to_string()];
        let includes = ["/tmp/r9syn/sub".to_string()];
        let o = assemble("t1.s", "/tmp/r9syn", &defines, &includes, &read).unwrap();
        assert_eq!(o, include_bytes!("testdata/t1.6"));
    }

    #[test]
    fn a_syntax_error_has_no_object() {
        let e = asm("MOVQ ,,\n").unwrap_err();
        assert!(e.contains("syntax error"), "{e}");
    }
}
