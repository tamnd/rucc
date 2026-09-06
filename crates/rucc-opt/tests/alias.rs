//! Alias analysis against programs whose memory is known, on shapes nobody chose.
//!
//! The analysis in `alias.rs` says two references cannot touch the same byte. That claim is
//! checkable, so this checks it: the generator builds a function where it knows exactly which
//! object every pointer is for and which bytes every access covers, the analysis is asked about
//! every pair, and any no that disagrees with what the generator knows is a miscompilation
//! waiting to be found by somebody else.
//!
//! # What the generator promises
//!
//! The parameters are `restrict` pointers to objects the function cannot see, and the generator
//! keeps the promise `restrict` makes rather than merely writing the metadata down: two
//! parameters with different bases in one clique are given different objects. Every access
//! carries the type node of the object it really touches, so the type-based layer is being told
//! the truth as well. A program that lied in either place would be undefined C, and an analysis
//! is allowed to say anything at all about one of those, so a generator that produced them would
//! be checking nothing.
//!
//! One local has its address handed out and one of the parameters is for it, which is the case
//! the escape layer has to be careful about. Every other local stays inside the function, and an
//! escape layer that missed the store would report those two as separate objects and be wrong.
//!
//! # What is checked
//!
//! **Soundness.** A no is only allowed when the two accesses really do cover no byte in common.
//! This is the whole point and everything else is secondary.
//!
//! **Symmetry.** Asking in the other order gives the same answer. An alias analysis that
//! disagrees with itself is one whose result depends on the order a pass happens to walk in, and
//! the bug it produces looks like the optimizer being non-deterministic rather than like the
//! analysis being wrong.
//!
//! **Repeatability.** A second analysis of the same function answers the same way.
//!
//! **The counts add up.** Every no was attributed to exactly one layer.
//!
//! # What this cannot check
//!
//! Whether the type-based layer is too aggressive, because the only way to catch that would be
//! to generate a program that reads an object through the wrong type, and such a program is
//! undefined. Union punning is the one shape of that C actually promises to keep working, it
//! rests on the order the layers run in rather than on their contents, and the unit test named
//! for it in `alias.rs` is what holds the order in place.

use rucc_base::Interner;
use rucc_ir::{
    Builder, Def, Extra, Flags, Func, Global, InstData, MemInfo, MemOrder, Meta, MetaNode, Module,
    Opcode, Restrict, Signature, TbaaNode, Type, Value,
};
use rucc_opt::{Access, Alias, Answer, Reason};
use rucc_target::{TargetInfo, Triple};

/// How many bytes every object in a generated function is.
const OBJECT: u64 = 64;

/// How many parameters a generated function takes.
///
/// Four is enough for two of them to be in one `restrict` clique, one of them to be for the
/// local that escaped, and one of them to be typed `char` so that the layer that has to let
/// everything through gets asked.
const PARAMS: usize = 4;

/// How many locals a generated function has.
const LOCALS: usize = 3;

/// How many accesses one generated function makes.
const ACCESSES: usize = 12;

#[test]
fn a_no_is_only_ever_said_about_two_references_that_really_do_not_meet() {
    let mut random = Random::new(0x5ce7_c0ff_ee00_0002);
    let mut nos = 0;
    let mut asked = 0;
    for _ in 0..400 {
        let case = Case::new(&mut random);
        let (module, func, accesses) = case.build();
        let mut alias = Alias::new(&func, &module);

        for (i, (a, truth_a)) in accesses.iter().enumerate() {
            for (b, truth_b) in accesses.iter().skip(i) {
                let answer = alias.query(a, b);
                asked += 1;
                if answer.is_no() {
                    nos += 1;
                    assert!(
                        !truth_a.meets(truth_b),
                        "said {answer:?} about {truth_a:?} and {truth_b:?} in {case:?}"
                    );
                }
                assert_eq!(
                    alias.query(b, a).is_no(),
                    answer.is_no(),
                    "the answer changed when the two were swapped, about {truth_a:?} and \
                     {truth_b:?} in {case:?}"
                );
            }
        }

        // Every no went on exactly one layer's tally.
        let counts = alias.counts();
        let total: u64 = Reason::ALL.iter().map(|&r| counts.answered(r)).sum();
        assert_eq!(total, counts.total());
        assert!(counts.queries() >= total);

        // A second analysis of the same function agrees with the first, everywhere.
        let mut again = Alias::new(&func, &module);
        for (a, _) in &accesses {
            for (b, _) in &accesses {
                assert_eq!(again.query(a, b), alias.query(a, b), "not repeatable in {case:?}");
            }
        }
    }

    // A run where nothing was ever disambiguated would pass every assertion above and prove
    // nothing at all, so the run says how much it actually did.
    assert!(nos * 4 > asked, "only {nos} of {asked} pairs were disambiguated, which is too few");
}

/// One function's worth of decisions, and the truth about what it does.
#[derive(Clone, Debug)]
struct Case {
    /// Which object each parameter is a pointer to.
    points_at: [Object; PARAMS],
    /// Which locals had their address handed out, in the order they are created.
    escapes: [bool; LOCALS],
    /// The accesses, as the generator meant them.
    accesses: Vec<Plan>,
}

/// One access, before it is instructions.
#[derive(Clone, Copy, Debug)]
struct Plan {
    /// The pointer it goes through.
    base: Base,
    /// How many bytes into the object it starts.
    offset: u64,
    /// How many bytes it covers.
    size: u64,
    /// Whether the metadata that would disambiguate it is attached, which is what a pointer that
    /// was not declared `restrict` looks like.
    tagged: bool,
    /// Whether it goes through `char` rather than through the object's own type.
    ///
    /// Reading any object a byte at a time is legal C and the type-based layer has to let it
    /// through, so the generator produces it on purpose. It is also the only way this suite can
    /// reach two accesses to one object carrying two different type nodes, which is the shape a
    /// layer that only looked up the tree in one direction would get wrong.
    bytewise: bool,
    /// Whether it writes rather than reads.
    writes: bool,
}

/// Which pointer an access goes through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Base {
    /// A parameter, by its number.
    Param(usize),
    /// A local of this function, by its number.
    Local(usize),
}

/// Which object an access really touches, which is what the generator knows and the analysis
/// has to work out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Object {
    /// One of the objects the caller owns, by number.
    Hidden(usize),
    /// One of this function's own, by number.
    Local(usize),
}

impl Object {
    /// The type every access to it is made through.
    ///
    /// One `char` among them, because the type-based layer has to let an access through `char`
    /// meet anything and a suite where every object had its own type would never find out.
    fn ty(self) -> usize {
        match self {
            Object::Hidden(n) | Object::Local(n) => n % 3,
        }
    }
}

/// What an access really covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Truth {
    object: Object,
    start: u64,
    end: u64,
}

impl Truth {
    /// Whether these two really do cover a byte in common.
    fn meets(&self, other: &Self) -> bool {
        self.object == other.object && self.start < other.end && other.start < self.end
    }
}

impl Case {
    fn new(random: &mut Random) -> Self {
        // Parameters 0, 1 and 2 are `restrict` pointers to three objects the caller owns, which
        // is the promise the analysis is allowed to believe. Parameter 3 is for the local that
        // escaped, which is the case the escape layer must not disambiguate.
        let points_at = [Object::Hidden(0), Object::Hidden(1), Object::Hidden(2), Object::Local(0)];

        // The first local is the one whose address is handed out, because parameter 3 is for it.
        // The rest are decided at random, so the run contains functions where an address left
        // for no reason and functions where none did.
        let mut escapes = [false; LOCALS];
        escapes[0] = true;
        for slot in escapes.iter_mut().skip(1) {
            *slot = random.below(4) == 0;
        }

        let accesses = (0..ACCESSES)
            .map(|_| {
                let base = if random.below(2) == 0 {
                    Base::Param(random.below(PARAMS as u64) as usize)
                } else {
                    Base::Local(random.below(LOCALS as u64) as usize)
                };
                let bytewise = random.below(4) == 0;
                let size = if bytewise { 1 } else { 1 << random.below(4) };
                Plan {
                    base,
                    offset: random.below(OBJECT - size + 1),
                    size,
                    // Most accesses carry their metadata. Some do not, which is what a pointer
                    // nobody declared `restrict` gives, and those must not be disambiguated by a
                    // layer that reads it.
                    tagged: random.below(4) != 0,
                    bytewise,
                    writes: random.below(2) == 0,
                }
            })
            .collect();

        Self { points_at, escapes, accesses }
    }

    /// Which object this plan really touches.
    fn object(&self, plan: &Plan) -> Object {
        match plan.base {
            Base::Param(n) => self.points_at[n],
            Base::Local(n) => Object::Local(n),
        }
    }

    /// The function, and every access in it beside what it really does.
    fn build(&self) -> (Module, Func, Vec<(Access, Truth)>) {
        let mut names = Interner::new();
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let mut module = Module::new(names.intern("t.c"), &target);

        // A `char` root with `int` and `float` under it, which is the shape section 8.2 asks for
        // and the reason an access through `char` meets everything.
        let root = module.add_meta(MetaNode::Tbaa(TbaaNode {
            name: names.intern("char"),
            parent: None,
            offset: 0,
        }));
        let types: [Meta; 3] = [
            root,
            module.add_meta(MetaNode::Tbaa(TbaaNode {
                name: names.intern("int"),
                parent: Some(root),
                offset: 0,
            })),
            module.add_meta(MetaNode::Tbaa(TbaaNode {
                name: names.intern("float"),
                parent: Some(root),
                offset: 0,
            })),
        ];
        module.add_global(Global::new(names.intern("keep"), 8, 8));

        let params = [Type::PTR; PARAMS];
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&params));
        let entry = func.create_block();
        for ty in params {
            func.append_param(entry, ty);
        }
        let incoming: Vec<Value> = func[entry].params.to_vec();

        let mut build = Builder::new(&mut func, entry);
        let locals: Vec<Value> = (0..LOCALS)
            .map(|_| {
                let mem = build.func().add_mem(MemInfo {
                    size: OBJECT,
                    align: 16,
                    order: MemOrder::NotAtomic,
                    tbaa: None,
                    restrict: Restrict::NONE,
                });
                build.value(
                    InstData { extra: Extra::Mem(mem), ..InstData::new(Opcode::Alloca) },
                    Type::PTR,
                )
            })
            .collect();

        // The addresses that leave. Writing one out through a parameter is the plainest way for
        // an address to become something the caller can reach, and it is what makes parameter 3
        // able to be for the first local.
        for (index, &local) in locals.iter().enumerate() {
            if self.escapes[index] {
                build.store(local, incoming[0], mem_of(8, None, Restrict::NONE), Flags::NONE);
            }
        }

        let mut out = Vec::with_capacity(self.accesses.len());
        for plan in &self.accesses {
            let object = self.object(plan);
            let pointer = match plan.base {
                Base::Param(n) => incoming[n],
                Base::Local(n) => locals[n],
            };
            let at = build.iconst(Type::int(64), i128::from(plan.offset));
            let address = build.binary(Opcode::PtrAdd, pointer, at, Flags::NONE);

            let restrict = match (plan.tagged, plan.base) {
                // The clique is one for every parameter and the base is the parameter's own
                // number, which is truthful because no two parameters are for one object.
                (true, Base::Param(n)) => Restrict { clique: 1, base: n as u16 + 1 },
                _ => Restrict::NONE,
            };
            let tbaa = plan.tagged.then(|| if plan.bytewise { root } else { types[object.ty()] });
            let info = mem_of(plan.size, tbaa, restrict);

            let ty = Type::int(u32::try_from(plan.size).unwrap() * 8);
            let inst = if plan.writes {
                let value = build.iconst(ty, 1);
                build.store(value, address, info, Flags::NONE)
            } else {
                let value = build.load(ty, address, info, Flags::NONE);
                match build.func()[value].def {
                    Def::Result { inst, .. } => inst,
                    Def::Param { .. } => unreachable!("a load is not a block parameter"),
                }
            };
            out.push((inst, object, plan));
        }
        build.ret(&[]);

        let alias = Alias::new(&func, &module);
        let accesses = out
            .into_iter()
            .map(|(inst, object, plan)| {
                let access = if plan.writes {
                    alias.writes(inst).expect("a store writes")
                } else {
                    alias.reads(inst).expect("a load reads")
                };
                assert_eq!(access.size, Some(plan.size), "the access is not the size asked for");
                let truth = Truth { object, start: plan.offset, end: plan.offset + plan.size };
                (access, truth)
            })
            .collect();
        drop(alias);

        (module, func, accesses)
    }
}

fn mem_of(size: u64, tbaa: Option<Meta>, restrict: Restrict) -> MemInfo {
    MemInfo {
        size,
        align: u32::try_from(size).unwrap(),
        order: MemOrder::NotAtomic,
        tbaa,
        restrict,
    }
}

#[test]
fn an_answer_names_the_layer_that_gave_it_and_the_layer_has_something_to_say() {
    // Section 8.5, held to in a test so that a layer added later without a sentence to go with
    // it does not get as far as a user asking why their loop was not vectorized.
    for reason in Reason::ALL {
        assert!(!reason.name().is_empty());
        assert!(reason.describe().len() > reason.name().len());
        assert_eq!(Answer::No(reason).reason(), Some(reason));
    }
}

/// The same xorshift the scalar evolution suite uses, for the same reason: a test that generates
/// its own cases has to generate the same ones tomorrow.
struct Random(u64);

impl Random {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn bits(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.bits() % bound
    }
}
