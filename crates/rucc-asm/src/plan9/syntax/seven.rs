//! The assembler `7a`: the grammar of `7a/a.y` and the object of `7a/lex.c`.
//!
//! The parser is the parser of Plan 9 yacc, and it uses the LALR tables in `tab7`. It reads a
//! token only in a state that has a shift or more than one reduction, as yacc does. This is
//! necessary because the line of each record is the value of `lineno` when the parser reduces the
//! instruction. The recovery after a syntax error is also the recovery of yacc.

use rucc_object::plan9::arm64 as a7;

use super::itab7::ITAB;
use super::lex::{Arch, Fatal, Hist, Lexer, NSYM, R, Reader, Tok, ieeedtod};
use super::tab7::{
    CHECK, DEFACT, DEFGOTO, PACT, PGOTO, R1, R2, TABLE, YYFINAL, YYLAST, YYNTOKENS, YYPACT_NINF,
    YYTABLE_NINF, symbol,
};
use super::ydebug7::TABLES;

const D_NONE: i16 = 1;
const D_STATIC: i16 = 3;
const D_BRANCH: i16 = 6;
const D_OREG: i16 = 7;
const D_XPRE: i16 = 8;
const D_XPOST: i16 = 9;
const D_CONST: i16 = 10;
const D_DCONST: i16 = 11;
const D_FCONST: i16 = 12;
const D_SCONST: i16 = 13;
const D_REG: i16 = 14;
const D_SP: i16 = 15;
const D_FREG: i16 = 16;
const D_VREG: i16 = 17;
const D_SPR: i16 = 18;
const D_FILE: u8 = 19;
const D_OCONST: i16 = 20;
const D_SHIFT: i16 = 22;
const D_EXTREG: i16 = 27;
const D_ROFF: i16 = 28;
const D_COND: i16 = 29;
const D_VLANE: i16 = 30;
const D_VSET: i16 = 31;

const NREG: i32 = 32;
const REGSP: i64 = 31;

/// The depth of the stack of the parser of Plan 9 yacc.
const YYMAXDEPTH: usize = 150;

/// The symbol `error` of the tables.
const YYERRCODE: i32 = 1;

const ARCH: Arch = Arch { thechar: b'7', nsymb: 8192, include: "/arm64/include", value32: true };

/// An operand, the `Gen` of `7a`.
#[derive(Debug, Clone, Copy)]
struct Gen {
    sym: Option<usize>,
    offset: i64,
    typ: i16,
    reg: i16,
    name: i16,
    dval: f64,
    sval: [u8; 8],
    /// The operand is a value of the union of yacc that is not a `Gen`. The rule
    /// `vlane: vset '[' con ']'` makes it, and `7a` reads a bad pointer from it.
    bad: bool,
}

#[allow(clippy::cast_possible_truncation)]
const NULLGEN: Gen = Gen {
    sym: None,
    offset: 0,
    typ: D_NONE,
    reg: NREG as i16,
    name: D_NONE,
    dval: 0.0,
    sval: [0; 8],
    bad: false,
};

fn gen_type(typ: i16) -> Gen {
    Gen { typ, ..NULLGEN }
}

/// A value on the stack of the parser, the union of `7a/a.y`.
#[derive(Debug, Clone, Copy)]
struct V {
    lval: i64,
    dval: f64,
    sval: [u8; 8],
    sym: usize,
    opd: Gen,
}

const NULLV: V = V { lval: 0, dval: 0.0, sval: [0; 8], sym: 0, opd: NULLGEN };

impl V {
    fn tok(t: &Tok) -> Self {
        V { lval: t.lval, dval: t.dval, sval: t.sval, sym: t.sym, ..NULLV }
    }

    fn of(g: Gen) -> Self {
        V { opd: g, ..NULLV }
    }

    fn lval(v: i64) -> Self {
        V { lval: v, ..NULLV }
    }
}

/// A short value, as C stores a `vlong` in a `short`.
#[allow(clippy::cast_possible_truncation)]
fn short(v: i64) -> i16 {
    v as i16
}

/// An `int` value, as C passes a `vlong` to an `int`.
#[allow(clippy::cast_possible_truncation)]
fn int(v: i64) -> i32 {
    v as i32
}

/// The division of the machine. On arm64, a division by zero gives zero.
fn div(a: i64, b: i64) -> i64 {
    if b == 0 { 0 } else { a.wrapping_div(b) }
}

/// The remainder of the machine, `a - (a/b)*b`.
fn rem(a: i64, b: i64) -> i64 {
    if b == 0 { a } else { a.wrapping_rem(b) }
}

#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
fn shl(a: i64, b: i64) -> i64 {
    a.wrapping_shl(b as u32)
}

#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
fn shr(a: i64, b: i64) -> i64 {
    a.wrapping_shr(b as u32)
}

/// `1<<r` as an `int` of C, with the shift of the machine.
#[allow(clippy::cast_sign_loss)]
fn bit(r: i16) -> i32 {
    1i32.wrapping_shl(r as u32)
}

#[allow(clippy::cast_sign_loss)]
fn at<T: Copy>(a: &[T], i: i32) -> T {
    a[i as usize]
}

struct Seven<'a> {
    lx: Lexer<'a>,
    pass: i32,
    pc: i32,
    h: [(Option<usize>, i16); NSYM],
    sym: i32,
    out: Vec<u8>,
    pathname: Vec<u8>,
}

impl Seven<'_> {
    fn name(&self, s: usize) -> String {
        String::from_utf8_lossy(&self.lx.syms[s].name).into_owned()
    }

    /// `yyparse`, the parser of Plan 9 yacc over the tables of bison.
    fn parse(&mut self) -> R<()> {
        let mut ss: Vec<i32> = Vec::new();
        let mut vs: Vec<V> = Vec::new();
        let mut state = 0;
        let mut val = NULLV;
        let mut la: Option<(i32, Tok)> = None;
        let mut errflag = 0u8;
        'stack: loop {
            if ss.len() >= YYMAXDEPTH {
                self.lx.yyerror("yacc stack overflow")?;
                return Ok(());
            }
            ss.push(state);
            vs.push(val);
            'newstate: loop {
                let n = at(&PACT, state);
                let mut rule = None;
                if i32::from(n) != YYPACT_NINF {
                    let tok = match la {
                        Some((s, _)) => s,
                        None => {
                            let t = self.lx.yylex()?;
                            let s = symbol(t.t);
                            la = Some((s, t));
                            s
                        }
                    };
                    let i = i32::from(n) + tok;
                    if (0..=YYLAST).contains(&i) && i32::from(at(&CHECK, i)) == tok {
                        let a = i32::from(at(&TABLE, i));
                        if a > 0 {
                            if a == YYFINAL {
                                return Ok(());
                            }
                            errflag = errflag.saturating_sub(1);
                            if let Some((_, t)) = la.take() {
                                val = V::tok(&t);
                            }
                            state = a;
                            continue 'stack;
                        }
                        if a != 0 && a != YYTABLE_NINF {
                            rule = Some(-a);
                        }
                    } else {
                        rule = Some(i32::from(at(&DEFACT, state)));
                    }
                } else {
                    rule = Some(i32::from(at(&DEFACT, state)));
                }
                match rule {
                    Some(r) if r != 0 => {
                        #[allow(clippy::cast_sign_loss)]
                        let len = at(&R2, r) as usize;
                        // A rule of a mid-rule action sees the two symbols before it.
                        let base = if r == 4 || r == 6 { vs.len() - 2 } else { vs.len() - len };
                        val = if len > 0 { vs[vs.len() - len] } else { NULLV };
                        let args: Vec<V> = vs[base..].to_vec();
                        self.action(r, &args, &mut val)?;
                        ss.truncate(ss.len() - len);
                        vs.truncate(vs.len() - len);
                        if ss.len() == 1 {
                            // The reduction to prog: the start of a line.
                            self.lx.line_start(usize::from(la.is_some()));
                        }
                        let lhs = i32::from(at(&R1, r)) - YYNTOKENS;
                        let top = *ss.last().unwrap_or(&0);
                        let i = i32::from(at(&PGOTO, lhs)) + top;
                        state = if (0..=YYLAST).contains(&i) && i32::from(at(&CHECK, i)) == top {
                            i32::from(at(&TABLE, i))
                        } else {
                            i32::from(at(&DEFGOTO, lhs))
                        };
                        continue 'stack;
                    }
                    _ => {}
                }
                // A syntax error.
                if errflag == 3 {
                    if matches!(la, Some((0, _))) {
                        return Ok(());
                    }
                    la = None;
                    continue 'newstate;
                }
                if errflag == 0 {
                    self.lx.syntax_error(&TABLES)?;
                }
                errflag = 3;
                while let Some(&s) = ss.last() {
                    let n = at(&PACT, s);
                    if i32::from(n) != YYPACT_NINF {
                        let i = i32::from(n) + YYERRCODE;
                        if (0..=YYLAST).contains(&i) && i32::from(at(&CHECK, i)) == YYERRCODE {
                            let a = i32::from(at(&TABLE, i));
                            if a > 0 {
                                state = a;
                                val = NULLV;
                                continue 'stack;
                            }
                        }
                    }
                    ss.pop();
                    vs.pop();
                }
                return Ok(());
            }
        }
    }

    /// The action of the rule `r` of `7a/a.y`. The value `a[k-1]` is `$k`, and `v` is `$$`, which
    /// is `$1` before the action.
    #[allow(clippy::too_many_lines)]
    fn action(&mut self, r: i32, a: &[V], v: &mut V) -> R<()> {
        let c = |k: usize| a[k - 1].lval;
        let g = |k: usize| a[k - 1].opd;
        let op = || int(a[0].lval);
        let null = NULLGEN;
        let nreg = NREG;
        match r {
            // line: LLAB ':' @1 line
            4 => {
                let s = a[0].sym;
                if self.lx.syms[s].value != i64::from(self.pc) {
                    let n = self.name(s);
                    self.lx.yyerror(&format!("redeclaration of {n}"))?;
                }
                self.lx.syms[s].value = i64::from(self.pc);
            }
            // line: LNAME ':' @2 line
            6 => {
                let s = a[0].sym;
                self.lx.syms[s].typ = super::lex::LLAB;
                self.lx.syms[s].value = i64::from(self.pc);
            }
            // line: LNAME '=' expr ';'
            8 => {
                let s = a[0].sym;
                self.lx.syms[s].typ = super::lex::LVAR;
                self.lx.syms[s].value = self.lx.value(c(3));
            }
            // line: LVAR '=' expr ';'
            9 => {
                let s = a[0].sym;
                if self.lx.syms[s].value != c(3) {
                    let n = self.name(s);
                    self.lx.yyerror(&format!("redeclaration of {n}"))?;
                }
                self.lx.syms[s].value = self.lx.value(c(3));
            }
            13 | 25 | 37 | 39 | 65 => self.outcode(op(), &null, nreg, &null)?,
            14 => self.outcode(op(), &g(2), int(c(4)), &g(6))?,
            15 | 26 => self.outcode(op(), &g(2), int(c(4)), &null)?,
            16 | 17 | 18 | 19 | 27 | 28 | 33 | 44 | 47 | 49 | 50 | 56 | 62 => {
                self.outcode(op(), &g(2), nreg, &g(4))?;
            }
            20 => {
                let k = Gen { offset: c(5), ..gen_type(D_CONST) };
                self.outcode4(op(), &g(2), nreg, &k, &g(7))?;
            }
            21 | 22 | 23 | 24 | 42 | 43 | 48 => self.outcode(op(), &null, nreg, &g(3))?,
            29 | 32 | 53 | 55 => {
                self.outcode4(op(), &g(2), i32::from(g(6).reg), &g(4), &g(8))?;
            }
            30 | 31 | 51 | 57 => self.outcode(op(), &g(2), i32::from(g(4).reg), &g(6))?,
            34 => self.outcode(op(), &g(3), nreg, &g(5))?,
            35 | 36 | 59 => self.outcode4(op(), &g(2), int(c(6)), &g(4), &g(8))?,
            38 => self.outcode(op(), &null, nreg, &g(2))?,
            40 | 41 | 60 | 63 => self.outcode(op(), &g(2), nreg, &null)?,
            45 | 46 | 58 => self.outcode(op(), &g(2), int(c(4)), &g(6))?,
            52 => self.outcode(op(), &g(2), i32::from(g(4).reg), &null)?,
            54 => self.outcode4(op(), &g(2), i32::from(g(4).reg), &g(6), &g(8))?,
            61 => self.outcode(op(), &g(4), i32::from(g(2).reg), &null)?,
            64 => self.outcode(op(), &g(2), int(c(6)), &g(4))?,
            // cond: LCOND
            66 => *v = V::of(Gen { reg: short(c(1)), ..gen_type(D_COND) }),
            // sysarg: con ',' con ',' con ',' con
            69 => {
                let o = shl(c(1), 16) | shl(c(3), 12) | shl(c(5), 8) | shl(c(7), 5);
                *v = V::of(Gen { offset: o, ..gen_type(D_CONST) });
            }
            // rel: con '(' LPC ')'
            71 => {
                let o = c(1).wrapping_add(i64::from(self.pc));
                *v = V::of(Gen { offset: o, ..gen_type(D_BRANCH) });
            }
            // rel: LNAME offset
            72 => {
                let s = a[0].sym;
                if self.pass == 2 {
                    let n = self.name(s);
                    self.lx.yyerror(&format!("undefined label: {n}"))?;
                }
                *v = V::of(Gen { sym: Some(s), offset: c(2), ..gen_type(D_BRANCH) });
            }
            // rel: LLAB offset
            73 => {
                let s = a[0].sym;
                let o = self.lx.syms[s].value.wrapping_add(c(2));
                *v = V::of(Gen { sym: Some(s), offset: o, ..gen_type(D_BRANCH) });
            }
            // ximm: '$' con, and imm: '$' con
            74 | 103 => *v = V::of(Gen { offset: c(2), ..gen_type(D_CONST) }),
            75 => *v = V::of(Gen { typ: D_CONST, ..g(2) }),
            76 => *v = V::of(Gen { typ: D_OCONST, ..g(4) }),
            77 => *v = V::of(Gen { sval: a[1].sval, ..gen_type(D_SCONST) }),
            79 => *v = V::of(Gen { dval: a[1].dval, ..gen_type(D_FCONST) }),
            80 => *v = V::of(Gen { dval: -a[2].dval, ..gen_type(D_FCONST) }),
            83 | 120 => *v = V::of(Gen { offset: c(1), ..gen_type(D_SPR) }),
            84 => *v = V::of(Gen { offset: c(1), ..gen_type(D_OREG) }),
            89 | 94 => *v = V::of(Gen { reg: short(c(2)), ..gen_type(D_OREG) }),
            92 => *v = V::of(Gen { typ: D_OREG, reg: short(c(3)), ..g(1) }),
            95 => *v = V::of(Gen { reg: short(c(3)), offset: c(1), ..gen_type(D_OREG) }),
            96 => *v = V::of(Gen { reg: short(c(3)), offset: c(1), ..gen_type(D_XPRE) }),
            97 => *v = V::of(Gen { reg: short(c(2)), offset: c(4), ..gen_type(D_XPOST) }),
            // The C program also sets `xreg`, which is not in the object.
            98 | 99 => {
                let o = if r == 99 { c(5) | 1 << 16 } else { c(5) };
                *v = V::of(Gen { reg: short(c(2)), offset: o, ..gen_type(D_ROFF) });
            }
            104 | 110 => *v = V::of(Gen { reg: short(c(1)), ..gen_type(D_REG) }),
            105 => *v = V::of(Gen { reg: short(REGSP), ..gen_type(D_SP) }),
            106..=109 => {
                let n = i64::from(r - 106);
                let rr = if r == 107 { c(1) & 0x1f } else { c(1) };
                let o = shl(rr, 16) | shl(c(4), 10) | n << 22;
                *v = V::of(Gen { offset: o, ..gen_type(D_SHIFT) });
            }
            111 => {
                let o = shl(c(1), 16) | shl(c(2), 13);
                *v = V::of(Gen { reg: short(c(1)), offset: o, ..gen_type(D_EXTREG) });
            }
            112 => {
                if c(5) < 0 || c(5) > 4 {
                    self.lx.yyerror("shift value out of range")?;
                }
                let o = shl(c(1), 16) | shl(c(2), 13) | shl(c(5), 10);
                *v = V::of(Gen { reg: short(c(1)), offset: o, ..gen_type(D_EXTREG) });
            }
            113 => *v = V::lval(3 << 8 | c(1)),
            114 => *v = V::lval(shl(c(2), 8) | c(1)),
            // scon: con
            115 => {
                if c(1) < 0 || c(1) >= 64 {
                    self.lx.yyerror("shift value out of range")?;
                }
                *v = V::lval(c(1) & 0x3f);
            }
            // sreg: LR '(' expr ')'
            117 => {
                if c(3) < 0 || c(3) >= i64::from(NREG) {
                    self.lx.messages.push_str("register value out of range\n");
                }
                *v = V::lval(c(3));
            }
            119 => *v = V::lval(REGSP),
            121 => *v = V::of(Gen { offset: c(3), ..gen_type(short(c(1))) }),
            124 => *v = V::of(Gen { reg: short(c(1)), ..gen_type(D_FREG) }),
            125 => *v = V::of(Gen { reg: short(c(3)), ..gen_type(D_FREG) }),
            129 => *v = V::of(Gen { offset: c(1), ..gen_type(D_VSET) }),
            130 => v.opd = Gen { typ: D_VLANE, offset: c(3), ..g(1) },
            131 => v.opd = Gen { typ: D_VLANE, offset: c(3), bad: true, ..g(1) },
            132 => *v = a[1],
            133 => *v = V::lval(i64::from(bit(g(1).reg))),
            134 => {
                let (x, y) = (g(1).reg, g(3).reg);
                let mut m = 0i32;
                for i in x..=y {
                    m |= bit(i);
                }
                for i in y..=x {
                    m |= bit(i);
                }
                *v = V::lval(i64::from(m));
            }
            135 => *v = V::lval(i64::from(bit(g(1).reg)) | c(3)),
            136 => *v = V::of(Gen { reg: short(c(1)), ..gen_type(D_VREG) }),
            137 => *v = V::of(Gen { reg: short(c(3)), ..gen_type(D_VREG) }),
            // name: con '(' pointer ')'
            138 => *v = V::of(Gen { name: short(c(3)), offset: c(1), ..gen_type(D_OREG) }),
            139 => {
                let s = Some(a[0].sym);
                *v = V::of(Gen { name: short(c(4)), sym: s, offset: c(2), ..gen_type(D_OREG) });
            }
            140 => {
                let s = Some(a[0].sym);
                *v = V::of(Gen { name: D_STATIC, sym: s, offset: c(4), ..gen_type(D_OREG) });
            }
            141 => *v = V::lval(0),
            142 | 150 => *v = V::lval(c(2)),
            143 | 149 => *v = V::lval(c(2).wrapping_neg()),
            148 => *v = V::lval(self.lx.syms[a[0].sym].value),
            151 => *v = V::lval(!c(2)),
            152 => *v = V::lval(c(2)),
            154 => *v = V::lval(c(1).wrapping_add(c(3))),
            155 => *v = V::lval(c(1).wrapping_sub(c(3))),
            156 => *v = V::lval(c(1).wrapping_mul(c(3))),
            157 => *v = V::lval(div(c(1), c(3))),
            158 => *v = V::lval(rem(c(1), c(3))),
            159 => *v = V::lval(shl(c(1), c(4))),
            160 => *v = V::lval(shr(c(1), c(4))),
            161 => *v = V::lval(c(1) & c(3)),
            162 => *v = V::lval(c(1) ^ c(3)),
            163 => *v = V::lval(c(1) | c(3)),
            _ => {}
        }
        Ok(())
    }

    // The object, from `7a/lex.c`.

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
        self.put16(i32::from(a7::ANAME));
        self.put(&[t as u8, slot as u8]);
        let name = self.lx.syms[s].name.clone();
        self.put(&name);
        self.put(&[0]);
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn zaddr(&mut self, a: &Gen, s: i32) -> R<()> {
        let mut typ = a.typ;
        if typ == D_CONST && i64::from(a.offset as i32) != a.offset {
            typ = D_DCONST;
        }
        self.put(&[typ as u8, a.reg as u8, s as u8, a.name as u8]);
        match typ {
            D_NONE | D_REG | D_SP | D_FREG | D_VREG | D_COND => {}
            D_DCONST => {
                self.put32(a.offset as i32);
                self.put32((a.offset >> 32) as i32);
            }
            D_OREG | D_XPRE | D_XPOST | D_CONST | D_BRANCH | D_SHIFT | D_EXTREG | D_ROFF
            | D_SPR => self.put32(a.offset as i32),
            D_SCONST => self.put(&a.sval),
            D_FCONST => {
                let (l, h) = ieeedtod(a.dval);
                self.put32(l);
                self.put32(h);
            }
            _ => {
                self.lx.messages.push_str(&format!("unknown type {typ}\n"));
                return Err(Fatal);
            }
        }
        Ok(())
    }

    /// `outsim`: the slot of the symbol of `g`, and a new slot with `ANAME` when it is not in the
    /// cache.
    fn outsim(&mut self, g: &Gen) -> R<i32> {
        if g.bad {
            self.lx.messages.push_str("7a: sys: trap: fault read\n");
            return Err(Fatal);
        }
        let Some(s) = g.sym else {
            return Ok(0);
        };
        let mut sno = i32::from(self.lx.syms[s].slot);
        if !(0..NSYM as i32).contains(&sno) {
            sno = 0;
        }
        let t = g.name;
        let e = self.h[usize::try_from(sno).unwrap_or(0)];
        if e.1 == t && e.0 == Some(s) {
            return Ok(sno);
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
        Ok(sym)
    }

    /// The end of a loop of `outsim` that does not end in `7a`. Two operands with the same symbol
    /// and the same name get the same slot each time.
    fn no_end(&mut self) -> R<()> {
        self.lx
            .messages
            .push_str("7a: two operands have the same symbol, and outcode does not end\n");
        Err(Fatal)
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn head(&mut self, a: i32, reg: i32) {
        self.put16(a);
        self.put(&[reg as u8]);
        self.put32(self.lx.lineno);
    }

    fn counts(a: i32) -> bool {
        a != i32::from(a7::AGLOBL) && a != i32::from(a7::ADATA)
    }

    /// `outcode`.
    fn outcode(&mut self, a: i32, g1: &Gen, reg: i32, g2: &Gen) -> R<()> {
        if Self::counts(a) {
            self.pc = self.pc.wrapping_add(1);
        }
        if self.pass == 1 {
            return Ok(());
        }
        let mut n = 0;
        let (sf, st) = loop {
            let sf = self.outsim(g1)?;
            let st = self.outsim(g2)?;
            if !(sf != 0 && st == sf) {
                break (sf, st);
            }
            n += 1;
            if n > NSYM {
                self.no_end()?;
            }
        };
        self.head(a, reg);
        self.zaddr(g1, sf)?;
        self.zaddr(g2, st)
    }

    /// `outcode4`, with the third operand.
    fn outcode4(&mut self, a: i32, g1: &Gen, reg: i32, g2: &Gen, g3: &Gen) -> R<()> {
        if Self::counts(a) {
            self.pc = self.pc.wrapping_add(1);
        }
        if self.pass == 1 {
            return Ok(());
        }
        let mut n = 0;
        let (s1, s2, s3) = loop {
            let s1 = self.outsim(g1)?;
            let s2 = self.outsim(g2)?;
            let s3 = self.outsim(g3)?;
            let again = s1 != 0 && (s2 != 0 && s1 == s2 || s3 != 0 && s1 == s3)
                || s2 != 0 && s3 != 0 && s2 == s3;
            if !again {
                break (s1, s2, s3);
            }
            n += 1;
            if n > NSYM {
                self.no_end()?;
            }
        };
        let flag = if g2.typ == D_NONE { 0 } else { 0x40 };
        self.head(a, reg | flag);
        self.zaddr(g1, s1)?;
        if flag != 0 {
            self.zaddr(g2, s2)?;
        }
        self.zaddr(g3, s3)
    }

    /// `cclean`.
    fn cclean(&mut self) -> R<()> {
        self.outcode(i32::from(a7::AEND), &NULLGEN, NREG, &NULLGEN)
    }

    /// `outhist`.
    fn outhist(&mut self) -> R<()> {
        let hist: Vec<Hist> = self.lx.hist.clone();
        for h in &hist {
            for n in h.names(&self.pathname) {
                self.put16(i32::from(a7::ANAME));
                self.put(&[D_FILE, 1, b'<']);
                self.put(&n);
                self.put(&[0]);
            }
            self.put16(i32::from(a7::AHISTORY));
            self.put(&[0]);
            self.put32(h.line);
            self.zaddr(&NULLGEN, 0)?;
            let g = Gen { offset: h.offset, ..NULLGEN };
            self.zaddr(&g, 0)?;
        }
        Ok(())
    }

    /// `pinit`, with the state of the parser.
    fn pinit(&mut self, file: &[u8], defines: &[String]) -> R<()> {
        self.lx.pinit(file)?;
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
        self.parse()?;
        if self.lx.nerrors != 0 {
            return self.cclean();
        }
        self.pass = 2;
        self.outhist()?;
        self.pinit(file.as_bytes(), defines)?;
        self.parse()?;
        self.cclean()
    }
}

/// Assembles `file` as `7a` does, and returns the object.
///
/// The arguments are the arguments of [`assemble6`](super::assemble6).
///
/// # Errors
///
/// The messages, when `7a` writes no object.
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
    let mut seven = Seven {
        lx,
        pass: 1,
        pc: 0,
        h: [(None, 0); NSYM],
        sym: 1,
        out: Vec::new(),
        pathname: cwd.as_bytes().to_vec(),
    };
    let r = seven.run(file, defines, includes);
    if r.is_err() || seven.lx.nerrors != 0 {
        return Err(seven.lx.messages);
    }
    Ok(seven.out)
}

#[cfg(test)]
mod tests {
    use super::super::tab7::RULES;
    use super::*;

    fn asm(text: &str) -> Result<Vec<u8>, String> {
        let text = text.as_bytes().to_vec();
        let read = move |n: &str| if n == "/t.s" { Ok(text.clone()) } else { Err(String::new()) };
        assemble("/t.s", "/", &[], &[], &read)
    }

    /// The records of an object, as the opcode and the line of each record that is not a name.
    fn records(o: &[u8]) -> Vec<(u16, i32)> {
        let mut r = Vec::new();
        let mut i = 0;
        let addr = |i: &mut usize| {
            let t = i16::from(o[*i]);
            *i += 4;
            *i += match t {
                D_DCONST | D_FCONST | D_SCONST => 8,
                D_OREG | D_XPRE | D_XPOST | D_CONST | D_BRANCH | D_SHIFT | D_EXTREG | D_ROFF
                | D_SPR => 4,
                _ => 0,
            };
        };
        while i < o.len() {
            let a = u16::from_le_bytes([o[i], o[i + 1]]);
            if a == a7::ANAME {
                i += 4;
                while o[i] != 0 {
                    i += 1;
                }
                i += 1;
                continue;
            }
            let flag = o[i + 2] & 0x40 != 0;
            let line = i32::from_le_bytes([o[i + 3], o[i + 4], o[i + 5], o[i + 6]]);
            i += 7;
            addr(&mut i);
            if flag {
                addr(&mut i);
            }
            addr(&mut i);
            r.push((a, line));
        }
        r
    }

    #[test]
    fn the_rules_are_the_rules_of_the_grammar() {
        assert_eq!(RULES[4], "@1: ");
        assert_eq!(RULES[72], "rel: LNAME offset");
        assert_eq!(RULES[131], "vlane: vset '[' con ']'");
        assert_eq!(RULES[163], "expr: expr '|' expr");
    }

    #[test]
    fn an_empty_file_has_the_history_and_the_end() {
        let o = asm("").unwrap();
        assert_eq!(records(&o), vec![(a7::AHISTORY, 1), (a7::AHISTORY, 1), (a7::AEND, 1)]);
    }

    #[test]
    fn the_line_of_a_record_follows_the_lookahead() {
        // RET reads the token after it to find a comma, and MOV does not.
        let o = asm("RET\nMOV R1, R2\nMOV R1, R2\n").unwrap();
        let r = records(&o);
        let ops: Vec<i32> = r[2..].iter().map(|x| x.1).collect();
        assert_eq!(ops, vec![2, 2, 3, 4]);
    }

    #[test]
    fn the_object_is_the_object_of_the_c_program() {
        // `t7.7` is the output of `7a t7.s` in `/tmp/r9s7b`.
        let read = |n: &str| -> Result<Vec<u8>, String> {
            Ok(match n {
                "t7.s" => include_bytes!("testdata/t7.s").to_vec(),
                "inc7.h" => include_bytes!("testdata/inc7.h").to_vec(),
                _ => return Err(String::new()),
            })
        };
        let o = assemble("t7.s", "/tmp/r9s7b", &[], &[], &read).unwrap();
        assert_eq!(o, include_bytes!("testdata/t7.7"));
    }

    #[test]
    fn the_same_symbol_on_two_operands_is_an_error() {
        // The C program does not stop on this line.
        let e = asm("GLOBL z<>+0(SB), $8\nDATA z<>+0(SB)/8, $z<>(SB)\n").unwrap_err();
        assert!(e.contains("does not end"), "{e}");
    }

    #[test]
    fn a_syntax_error_has_no_object() {
        let e = asm("MOVD R1,,\nRET\n").unwrap_err();
        assert!(e.contains("syntax error"), "{e}");
    }

    #[test]
    fn a_label_has_the_pc() {
        let o = asm("TEXT f(SB), $0\nl: B l\nRET\n").unwrap();
        assert!(records(&o).iter().any(|x| x.0 == a7::ATEXT));
    }

    #[test]
    fn a_syntax_error_has_the_debug_text_of_yacc() {
        // The text and the state are those of 7a at the pin, which yacc made with -D1.
        let e = asm("MOVL addr+0(FP), AX\n").unwrap_err();
        assert!(
            e.ends_with(
                "syntax error, last name: addr\nline:  LNAME.: $$5 line \nline:  LNAME.= expr ; \nsaw LNAME\n"
            ),
            "{e}"
        );
    }
}
