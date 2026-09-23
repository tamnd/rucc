//! Taking out a safety check whose answer is already known.
//!
//! Design: `spec/safe-memory/07-check-elimination.md` section 7.3, which is the first half of the
//! Tier E budget. `rucc-safety` puts a bounds check and a lifetime check in front of every access
//! and does not try to be clever about it, on purpose: a walk that inserts everything is a walk
//! anybody can read, and every check that is not needed is meant to be taken out here instead.
//! This pass takes them out, and it does the case document 07 expects to be worth the most and to
//! be the easiest to get right, which is a second access to bytes an earlier access already had
//! checked. Five kinds are the pass's business, the bounds check and the lifetime check and the
//! initialization check and the type check in front of an access and the derivation check after a
//! walk, because they are emitted together and taking out one of five is a fifth of a saving.
//!
//! The four in front of an access are four different claims and they get four fact sets. Which
//! bytes are inside one storage instance, which of them are in an instance that is alive, which of
//! them have been written, and which of them agree with a type. A check that passes establishes
//! exactly one of the four, they are killed by different things, and reporting them as one number
//! would hide which of the four a kept check is still being paid for.
//!
//! # The two halves
//!
//! Section 7.7 asks for the pass and the condition to be separate things, and they are. What is in
//! this file is a walk: which check runs before which, which pointer was computed from which, and
//! how far apart two addresses are. Nothing here decides whether that is enough. The condition
//! under which a check may go is a rule in `rules/safety.rules`, a solver has to agree with it
//! before this crate finishes building, and `crate::rules::safety` is the table it compiles into.
//!
//! The split is worth the trouble because the two halves fail differently. A walk that gets the
//! context wrong is a bug of the ordinary kind, and section 14.3's differential check accounting,
//! which runs the instrumented program with every check and again with the discharged ones gone,
//! is what looks for it. A removal condition that is wrong is arithmetic that is off at the ends of
//! the type. It gives the right answer on every test anybody writes and lets one access through in
//! the one case nobody thought of, and nothing observes that until somebody exploits it.
//!
//! # What it establishes and what it asks
//!
//! Walking the dominator tree from the entry, the pass carries a set of facts. A `check_bounds`
//! that stays is a fact, because a check that passes says the bytes it was about lie inside one
//! storage instance, and a check that fails does not return. A fact is remembered as the pointer's
//! base and the constant offset from it, which is what a chain of `ptr_add` over constants comes
//! to, plus how many bytes the access covers.
//!
//! At the next `check_bounds`, the pointer is normalized the same way. When a fact shares its base,
//! the distance between the two accesses is the difference of the two offsets, and that is a number
//! this pass has rather than a claim it makes: both addresses are the same value plus a constant.
//! The question of whether the later bytes are inside the earlier ones is then handed to the table,
//! which answers it in sixty four bit arithmetic rather than in the offsets, and the check goes
//! only if the answer is yes.
//!
//! A check whose extent is an operand is left out of all of this, in both directions. Section 7.4's
//! hoisted check covers as many bytes as its loop runs times, and every range compared here is a
//! pair of numbers, so such a check is neither read as a fact nor asked about. Reading its payload
//! would be worse than skipping it, since the size there is one element of the walk rather than the
//! range the check is about, and a fact recorded from it would be smaller than the truth in one
//! direction and a question asked from it smaller in the other.
//!
//! The capability operand has to be the `cap_of` of the check's own pointer, which is the shape
//! `rucc-safety` emits and the shape the argument needs. The check being removed asks whether its
//! bytes are inside the instance that owns its own pointer, its pointer is inside the range the
//! earlier check established, and that range is inside one instance, so the answer is yes. A check
//! whose capability came from somewhere else is asking about a different instance and is left
//! alone. Nothing is required of the earlier check's capability, because all that is used of it is
//! that the check passed, and a check that passed put its bytes inside one instance whatever
//! capability it named.
//!
//! # The conjunct that is not about bytes
//!
//! A `check_bounds` tests two things, because document 06 section 6.3 put the access alignment on
//! it rather than in a check of its own: that the bytes are inside one instance, and that the
//! address starts where an access of that alignment may start. Everything above is about the
//! first. A check that goes takes the second away with it, so nothing goes until something has
//! answered it, which is `aligned` below and which reads the object the address came from and the
//! steps taken from it. An access that assumes nothing about where it starts has nothing to
//! answer, and a member of a packed record is exactly that.
//!
//! A global is settled elsewhere and arrives as [`Flags::ALIGNED`], for the reason
//! [`Flags::STATIC`] beside it exists: how aligned a global is lives on the module and a pass is
//! given a function. Without that the gate would cost seventeen times what it costs, which is the
//! measurement in the changelog and is what says the flag earns its bit.
//!
//! What is left is a pointer this function was handed, one it loaded out of memory, and one a call
//! gave back, and for those the answer is the same one the bytes get: a check that stays is a check
//! that runs, and a check that runs tests the alignment and refuses when it does not hold. So the
//! first access through a pointer somebody handed in proves for nothing what every later access
//! through the same value needs, and `Scope::aligns` carries it.
//!
//! That fact is easier to carry than a range and it is worth saying why, because the section below
//! spends a page arguing about what a call does to a range. A range is about storage and storage
//! can be freed and handed back out smaller. An alignment is about the number in the value, and
//! nothing in a function changes the number an SSA value holds, so it crosses a call, it crosses
//! inline assembly and it crosses a `meta_end`. Dominance is the only thing that bounds it.
//!
//! What is still not answered is counted rather than argued about, the same as everything else
//! here, so `-fopt-info-missed` says what the rest of the `!aligned` fact of section 6.2.4 would be
//! worth. On the SQLite amalgamation that row is 6046 checks at 1178 sites, down from 10729 at 1413
//! once a check's own answer is carried, and the checks in the assembly went from 28473 to 23790.
//!
//! # The fact nobody had to check for
//!
//! Section 7.2 lists four sources of a discharge and puts the frontend first, because the majority
//! of accesses in real C are to a local or a global at a constant offset and the bounds of either
//! are not something anybody has to find out. An `alloca` of a fixed size makes one storage
//! instance of that many bytes and says so in its payload, so the range from its address to that
//! many further along is inside one instance for exactly the reason a passing `check_bounds` says
//! its own range is. When the address a check is about normalizes to such an `alloca`, that range
//! is the fact, and the question put to the table is the same question with the same rule
//! answering it.
//!
//! Two things make it worth more than a fact a check established. It is there before anything has
//! run, so the first access to a local is discharged rather than only the second. And no call takes
//! it away: a callee cannot free a frame slot, whatever it does to whatever the slot points at, so
//! this fact is asked separately rather than kept in the set the walk throws away at the first call
//! it cannot see through.
//!
//! Only the fixed size form. A variable length array is an `alloca` with an operand and a payload
//! whose size field reads zero, and reading it anyway would discharge every check in the array.
//!
//! A global is the same fact about the other half of section 7.2's sentence, and it arrives here
//! differently for one reason: how big a global is lives on the module and this pass is given one
//! function. So `crate::extents` works it out over the module before the pipeline starts, asks the
//! same rule, and writes the answer onto the check as [`Flags::STATIC`], which is what
//! `crate::nofree` does with what a call reaches and for the same reason. What is read here is what
//! the IR says, the same way the pass reads an opcode.
//!
//! It answers a lifetime check as well as a bounds check, which a local does not. What a local
//! gives is an extent, and how long it stays alive is the block it was declared in, which is a
//! question this pass has nothing to say about. A global has static storage duration and is alive
//! wherever the question is asked.
//!
//! # The walk that stops at a step it cannot read
//!
//! Everything above needs the address to be a base and a constant, and an array index is not a
//! constant. The walk stops at the first `ptr_add` whose step is a value, and what comes out is a
//! fact about a base whose size nobody knows, which answers nothing.
//!
//! Section 7.2's third source is what gets past it. Document 10's ranges know something about the
//! step even though it is not a number: an index the program has already tested against a length,
//! or one whose low bits are all that is used, is bounded. So the walk carries on, adding the low
//! end of the step's range to the offset and the width of the range to the size, and what it ends
//! up with is the range of addresses the access can land in.
//!
//! Whether an object holding all of that range holds the one address the access actually uses is
//! its own rule, `reached.i64`, which leaves the distance opaque so that one answer covers every
//! value the step could take. It is a rule of its own rather than the containment rule asked about
//! the far end of the range, and the reason is section 7.7's: turning a range of addresses into one
//! containment question is arithmetic on the thing being proved, and a pass doing that quietly is
//! what the split between the walk and the rule exists to stop.
//!
//! What the range is asked of is the list above and not a shorter one: the local an `alloca`
//! declares, the object an allocator made where the program has tested it, and the ranges checks
//! that already ran established. The allocation was missing from that list until tamnd/rucc#880,
//! which is what left a loop walking an index into its own `malloc` with every check it started
//! with however plainly the call said how many bytes it made.
//!
//! The range is only ever asked with and never recorded. What a check proves when it runs is that
//! the address the program used was inside the object, and nothing at all about the rest of a
//! range this pass made up around it. So a check discharged this way records the narrow fact, the
//! bytes the access really wanted, which is the thing that was proved and is what a second check
//! of the same bytes is answered by.
//!
//! The ranges are built only for a function that has a walk by a value in it, because they cost a
//! copy of the control flow graph and a function without one would never ask them anything.
//!
//! # The lifetime half, and what it borrows from the other one
//!
//! A `check_live` that stays is a fact too, and a smaller one than it looks: it says the storage
//! instance holding its own address is alive, and it says nothing about the address four bytes
//! along, because that address might be in a different instance. On its own that fact discharges
//! only a second lifetime check of the very same address, and the shape `rucc-safety` emits is a
//! lifetime check per field rather than per object, so on its own it would almost never fire.
//!
//! What makes it fire is the bounds fact sitting next to it. A `check_bounds` that passed put its
//! whole range inside one instance, so if the lifetime check's address is in that range, the
//! instance that was found alive is the instance the whole range is in, and the whole range is
//! alive. So a lifetime fact is recorded as the widest checked range containing its address, and a
//! later lifetime check is asked about as a single byte. The question of whether that byte is in
//! that range is the same question the bounds half asks, put to the same rule.
//!
//! The order the two arrive in is what makes this work rather than a coincidence to be careful
//! about: `rucc-safety` emits the bounds check first and the lifetime check second, so the range is
//! established by the time there is a lifetime fact to widen. A lifetime check that arrives with no
//! range around it keeps the narrow fact, which is correct and worth little.
//!
//! # The derivation half, which is one question rather than two
//!
//! `rucc-safety` puts a `check_deriv` after every `ptr_add` off a pointer, and what it asks is not
//! about a range at all: it asks whether the pointer that came out is still in the storage instance
//! the pointer that went in belongs to. The runtime has some slack in it for a pointer that walked
//! exactly off either end, and none of that slack is used here, because the case this pass answers
//! is the one where both ends are plainly inside something.
//!
//! What answers it is one fact holding both ends. A `check_bounds` that passed put its whole range
//! inside one instance, so if the address that went in and the address that came out are both in
//! that range, the second is in the instance the first belongs to, which is the question. It has to
//! be one fact and not one for each end: two facts saying two addresses are each inside some
//! instance say nothing about whether it is the same instance, and that is the only thing being
//! asked. A local is a fact of exactly this shape and is asked the same way.
//!
//! Both ends are asked about as a single byte, the way a lifetime check is, and for the same reason.
//! Nothing here is claiming anything about how many bytes are readable at either address.
//!
//! A `check_deriv` that stays leaves no fact behind. What it establishes is that two addresses share
//! an instance, which is not a range of bytes and does not fit in what this walk carries, and the
//! `covered.i64` rule has nothing to say about it. Recording it would mean a second kind of fact and
//! a second rule, and the pointer it is about nearly always gets a `check_bounds` of its own a few
//! instructions later that establishes the range properly.
//!
//! # What a call throws away, and which calls throw away nothing
//!
//! Section 7.3 says nothing kills a bounds fact except a redefinition of the capability, which in
//! SSA is never. This pass is stricter than that about the lifetime half and not about the bounds
//! half: a call, or anything else this pass cannot see through, drops every lifetime fact it is
//! carrying, and a call keeps the bounds facts and marks them as having had one run over them.
//!
//! The case a call is about is a `free` and then an allocation of something smaller at the same
//! address. The range established before the call is no longer inside one instance after it, and
//! what document 07 leaves that to is the lifetime judgement rather than this one. The next section
//! is the argument that the lifetime judgement is now enough, and what a mark on a fact buys.
//!
//! A `meta_end` and a `meta_transfer` drop both halves, and so does inline assembly. Nothing emits
//! either of the first two yet, so most of this costs nothing today and is the difference between
//! conservative and wrong on the day the instrumentation starts ending lifetimes. `crate::nofree`
//! treats them the same way. Assembly is with them rather than with the calls because the argument
//! below rests on the runtime owning the planes, and a block of assembly can write over one without
//! the runtime having been asked.
//!
//! The two facts nobody had to check for go across a call untouched, and neither is an exception to
//! the paragraph above because neither is in the set being thrown away. A callee cannot free a
//! frame slot and cannot free a global, so a check the declaration answers is answered on the far
//! side of any call at all.
//!
//! A call that says it reaches nothing which can free is the exception, and it is not this pass
//! being trusting. `crate::nofree` works the answer out over the whole module before the pipeline
//! starts and writes it onto the call site as [`Flags::NOFREE`], because the fact belongs to the
//! callee and a pass is given one function. Reading it here is reading what the IR says, the same
//! way the pass reads an opcode. Nothing else about a call is believed: the lifetime facts still go
//! across an unmarked call, a call through an address, and inline assembly.
//!
//! Freeing nothing is not writing nothing, and the initialization facts and the type facts are about
//! what was written. A callee that frees nothing can still store a `float` over bytes this function
//! last stored an `int` through, or `memcpy` bytes nothing wrote over ones something did, and either
//! makes a fact from before the call false while leaving every lifetime alone. So a marked call keeps
//! the bounds and lifetime facts and gives up the other two, unless `crate::purity` says the callee
//! writes no memory at all, which is `strlen` and its kind and whatever the module's own analysis
//! worked out.
//!
//! What the strictness still costs is measured rather than guessed. A check that a fact would have
//! covered if a call had not intervened is counted, so `-fopt-info-missed` says per function what
//! is left to win. On the SQLite amalgamation 3.53.4 at `-O2 -fsafety=detect` that is 3978 bounds
//! checks at 605 sites, 2679 lifetime checks at 596 sites and 2538 derivation checks at 534 sites,
//! against 28473 bounds checks and 20423 lifetime checks that survive the whole pipeline.
//!
//! # Why keeping the bounds half is allowed
//!
//! The eighth box of tamnd/rucc#1241 asked this pass to stop throwing the bounds facts away, on the
//! grounds that the lifetime check at the access compares a version and will refuse the case the
//! paragraph above is about. It is done and the reason belongs here rather than in the issue,
//! because what it turns on is what this file does.
//!
//! The claim is that a bounds fact may cross a call when every access that then uses it is guarded
//! by a lifetime check that refuses once the instance has changed. Three things have to hold for
//! that. The first two hold since the runtime started reading the version a recovery found and
//! started moving the aux with the bytes at a copy, and the third is what `Known::since` is for.
//!
//! The first holds. `rucc_safety::check` emits the two checks as a pair off one capability, so the
//! only question is whether this pass took the lifetime half out again, and there are five ways it
//! does. `REMOVED_LIVE_STATIC` is a global, `REMOVED_LIVE_LOCAL` is a frame slot of this
//! function, and `REMOVED_LIVE_HANDED` is an object every caller hands in, which `crate::params`
//! only ever says of a caller's frame slot or of a global this module vouches for. None of those
//! three can be freed by anybody, so a bounds fact about one does not go stale in the first place.
//! `REMOVED_LIVE` comes out of `Scope::alive`, which is thrown away at the call, so it cannot
//! fire on the far side of one. `REMOVED_LIVE_RANGE` is the frame slot rule or `Scope::alive`
//! widened, so it is those two again. On the far side of a call the lifetime check is therefore
//! either still standing or about an object no callee can end.
//!
//! The second holds and did not when this section was first written. A lifetime check that is still
//! standing refuses a changed instance only when the version the capability carries is about the
//! pointer the access went through, and `rucc_safe_rt::check`'s `stale` used to take the weaker
//! reading for every recovered capability, which is every pointer a `cap_of` could not trace back to
//! an allocator call. A recovery that walked the planes answers now, so a pointer a function was
//! handed is covered. A pointer it loaded out of memory is covered too, and that took a second
//! thing: the capability for one of those comes out of the aux slot beside the word, a slot holds a
//! displacement from the pointer it was written beside rather than an address, and a `memcpy` used
//! to move the word and leave the slot. `rucc_safe_rt::check::relocate` moves the aux across at
//! every copy a wrapper interposes, which is what tamnd/rucc#1148 wanted. What is left uncovered is
//! a pointer stored by code this compiler did not build, and an object a foreign writer has touched
//! is the case `rucc_safe_rt::layout::Meta::HANDED` already stands apart.
//!
//! The third is a hazard the relaxation introduces rather than one it inherits, and it is what the
//! mark is for. A lifetime fact is widened by `widened` out of the bounds facts standing at the
//! time, so bounds facts that survive a call would otherwise widen lifetime facts established after
//! it. A bounds fact saying a range is inside one instance, taken before a `free` and an allocation
//! of something smaller at the same address, would then widen a lifetime check that passed on the
//! new instance into a claim that the whole of the old range is alive, and the far end of that
//! range is storage the new instance does not own. So `Known::since` marks where the facts a call
//! has run over end, `widened` reads only the ones after it, and the marked ones answer a bounds
//! check and nothing else.
//!
//! The derivation rule reads only the unmarked ones too, and that one is caution rather than
//! necessity. What a `check_deriv` asks is whether two addresses share an instance, and a marked
//! fact answers the question it was established for rather than that one. Letting it read them
//! would still refuse every case that matters, because the access through a pointer it wrongly let
//! through has a lifetime check of its own that the version compare refuses, but the report would
//! arrive at the access as judgement J1 instead of at the derivation as J2, and a derivation
//! nothing is ever read through would go unreported. So it reads the unmarked ones and the cost is
//! counted in the `PAST_A_CALL_DERIV` row.
//!
//! What it comes to, on the SQLite amalgamation 3.53.4 at `-O2 -fsafety=detect`, before against
//! after. 414 bounds checks go, which is 28887 down to 28473, and the assembly shrinks by 107
//! kilobytes. 131 derivation checks arrive, 22754 up to 22885, and they are the other half of the
//! paragraph above: a bounds check that is removed establishes nothing, so a check the marked fact
//! answered no longer pushes a fact of its own, and the derivation rule was reading that. Lifetime
//! checks do not move at all, which is the point. Net it is 283 fewer checks in the object.
//!
//! Why it is only 414 is worth reading, because it says where the next piece of work is and it is
//! not here. A bounds check has to pass the alignment guard before any rule may take it out, and
//! the guard is only asked once a rule has answered, so the row counting what it costs only counts
//! checks something was ready to remove. That row goes from 8464 checks to 10729. Those 2265 are
//! bounds checks a fact that crossed a call now answers and the alignment guard then keeps anyway,
//! and they are five times the number that got out. The alignment question is `settles` and
//! `aligned` in this file, and it is the binding constraint on the bounds half now rather than the
//! call is.

use std::collections::{HashMap, HashSet};

use rucc_ir::{Block, Def, Extra, Flags, Func, Inst, Meta, Opcode, Type, Value};

use crate::purity::{Callee, Facts};
use crate::range::query::Ranges;
use crate::rules::{Piece, Subject, Table, safety};
use crate::{Analyses, Analysis, Cfg, Fuel, Pass, Preserved, Stats, copy, heap};

/// Recorded once for each bounds check taken out.
const REMOVED: &str = "bounds check removed, a dominating check covers the same bytes";

/// Recorded once for each bounds check taken out because it was inside a local.
const REMOVED_LOCAL: &str = "bounds check removed, its bytes are inside a local this function \
                             declares";

/// Recorded once for each bounds check taken out because it was inside a global.
const REMOVED_STATIC: &str = "bounds check removed, its bytes are inside an object of static \
                              storage duration";

/// Recorded once for each bounds check taken out because every caller hands in the object.
const REMOVED_HANDED: &str = "bounds check removed, its bytes are inside an object every call to \
                              this function hands it";

/// Recorded once for each bounds check taken out because an allocator made the object.
const REMOVED_MADE: &str = "bounds check removed, its bytes are inside an object an allocator made \
                            and this function has tested";

/// Recorded once for each bounds check taken out because a range answered the step it walked by.
const REMOVED_RANGE: &str = "bounds check removed, every address the walk can reach is inside the \
                             object it started from";

/// Recorded for a bounds check removed by carrying its walk past the step the constant reader
/// stopped at, all the way back to the pointer its capability names.
const REMOVED_MIDWAY: &str = "bounds check removed, its walk was carried on to the pointer its \
                              capability names and every address it can reach is held";

/// Recorded once for each lifetime check taken out.
const REMOVED_LIVE: &str = "lifetime check removed, a dominating check covers the same storage";

/// Recorded once for each lifetime check taken out because it was inside a global.
const REMOVED_LIVE_STATIC: &str =
    "lifetime check removed, its storage lives as long as the program does";

/// Recorded once for each lifetime check taken out because every caller hands in the object.
const REMOVED_LIVE_HANDED: &str = "lifetime check removed, its storage is an object every call to \
                                   this function hands it";

/// Recorded once for each lifetime check taken out because it was inside a frame slot.
const REMOVED_LIVE_LOCAL: &str =
    "lifetime check removed, its storage is a frame slot of this function";

/// Recorded once for each lifetime check taken out because a range answered the step it walked by.
const REMOVED_LIVE_RANGE: &str = "lifetime check removed, every address the walk can reach is in \
                                  storage a check found alive";

/// The same as [`REMOVED_MIDWAY`], for a lifetime check.
const REMOVED_MIDWAY_LIVE: &str = "lifetime check removed, its walk was carried on to the pointer \
                                   its capability names and every address it can reach is alive";

/// Recorded once for each init check taken out because one in front of it said the same bytes had
/// been written.
const REMOVED_INIT: &str = "initialization check removed, a dominating check covers the same bytes";

/// Recorded once for each init check taken out because a store in front of it wrote those bytes.
const REMOVED_STORED: &str =
    "initialization check removed, a dominating store wrote every byte it reads";

/// Recorded for an init check that would have gone if there had been fuel for it.
const NO_FUEL_INIT: &str = "initialization check kept, the pass ran out of fuel";

/// Recorded once for each init check a dominating one answered before something that could hand
/// the storage back out ran in between.
const PAST_A_CALL_INIT: &str = "initialization check kept, a dominating check covers its bytes and \
                                something that could end the storage ran in between";

/// Recorded for an init check whose pointer this pass cannot read as a base and a constant.
const UNKNOWN_SHAPE_INIT: &str =
    "initialization check left alone, its pointer is not a base and a constant";

/// Recorded for an init check nothing in front of it had anything to say about.
const NOTHING_WROTE_IT: &str =
    "initialization check kept, nothing dominating it says those bytes have been written";

/// Recorded once for each type check taken out because one in front of it asked the same question
/// of the same bytes.
const REMOVED_TYPE: &str =
    "type check removed, a dominating check covers the same bytes at the same type";

/// Recorded for a type check that would have gone if there had been fuel for it.
const NO_FUEL_TYPE: &str = "type check kept, the pass ran out of fuel";

/// Recorded once for each type check a dominating one answered before something that could hand the
/// storage back out ran in between.
const PAST_A_CALL_TYPE: &str = "type check kept, a dominating check covers its bytes at its type \
                                and something that could end the storage ran in between";

/// Recorded for a type check whose pointer this pass cannot read as a base and a constant, or whose
/// payload names no plane entry to ask about.
const UNKNOWN_SHAPE_TYPE: &str =
    "type check left alone, its pointer is not a base and a constant or it names no type";

/// Recorded for a type check nothing in front of it had anything to say about.
const NOTHING_TYPED_IT: &str =
    "type check kept, nothing dominating it says those bytes agree with that type";

/// Recorded for a bounds check that would have gone if there had been fuel for it.
const NO_FUEL: &str = "bounds check kept, the pass ran out of fuel";

/// Recorded for a lifetime check that would have gone if there had been fuel for it.
const NO_FUEL_LIVE: &str = "lifetime check kept, the pass ran out of fuel";

/// Recorded once for each derivation check taken out because a range answered the step it walked by.
const REMOVED_DERIV_RANGE: &str = "derivation check removed, every address either end can reach is \
                                   inside one checked range";

/// Recorded for a bounds check a call cost, which is the honest price of the paragraph above.
///
/// This one is worth reading rather than skipping. It is the number of checks that are still being
/// paid for because `crate::nofree` could not vouch for a call, so it says per function what the
/// rest of section 7.5's summary work would be worth before anybody writes it.
const PAST_A_CALL: &str =
    "bounds check kept, a call between it and the check that covers it might free";

/// The same, for a lifetime check. Section 8.8 is about this number rather than the one above.
const PAST_A_CALL_LIVE: &str =
    "lifetime check kept, a call between it and the check that covers it might free";

/// Recorded for a bounds check kept because nothing here says where the access starts.
///
/// The alignment conjunct of judgement J1 rides on `check_bounds`, so taking the check out takes
/// the alignment test with it. Recorded only for a check a rule had already answered the bytes of,
/// so the number is what the gate costs rather than how many checks have an alignment, which makes
/// it what the `!aligned` fact of `spec/safe-memory/06-instrumentation.md` section 6.2.4 would be
/// worth.
///
/// What is left in this row is the pointers no check has run on yet, since one that has is answered
/// by [`Scope::proved`]. So it is now a count of first accesses rather than of all of them, and
/// what would take it down further is the front end saying what a pointer is aligned to at the
/// point it makes one.
const UNKNOWN_ALIGNMENT: &str =
    "bounds check kept, nothing here says the address is aligned to what the access assumes";

/// Recorded for a bounds check whose address reached something saying an alignment that was short.
///
/// The other half of the row above, and it is separate because the two are worth different things.
/// That one is a value nothing here says anything about, and a fact from somewhere else could take
/// it. This one already has an answer and the answer is no, so a fact about where a pointer starts
/// would change nothing.
///
/// Two shapes end up here and they are not the same, which is worth knowing before anybody reads
/// the number as a target. One is `(int *)(p + 1)`, where the object is known and the step is a
/// constant that lands a byte into it. That is row S7 and the check has to stay. The other is a
/// step nobody can read, where [`divides`] answers one because every number divides by one, so the
/// walk comes back saying a byte and means it does not know. Telling those apart is what a better
/// [`divides`] would do and this row is where the work would show up.
const LOST_ALIGNMENT: &str =
    "bounds check kept, what the address was computed from says less alignment than it assumes";

/// Recorded for a bounds check whose operands this pass cannot read.
const UNKNOWN_SHAPE: &str = "bounds check left alone, its pointer is not a base and a constant";

/// Recorded for a bounds check whose capability names neither its address nor the base of it.
///
/// The other way [`about`] gives up, and it is a different thing entirely from the row above. The
/// capability here is readable and it names a value the address really was walked off, just not one
/// of the two [`its_own`] accepts: `rucc_safety::origin` shares one capability down a whole
/// derivation chain, and [`normal`] stops walking at the first step it cannot read, so a chain with
/// a step like `i * 4` in it leaves the capability naming something further back than the base.
/// Splitting this row into the three below it is what #1390 asked for, and the three sit in the
/// order the pass fails at them. This one is the walk not getting back to the named pointer at all,
/// which on the amalgamation is nothing, and the other two are the range rules refusing the walk it
/// did get back.
const MIDWAY_CAPABILITY: &str =
    "bounds check left alone, its capability names a pointer further back than its base";

/// Recorded for a midway bounds check where nothing at all is known about the pointer named.
///
/// The one that matters. [`beyond`] answered a range of addresses off the pointer the capability
/// names, every rule was asked, and not one of them has ever heard of that pointer: it is not a
/// local this function declared and no bounds check standing here is about it. So the range is not
/// too wide and the walk is not wrong, there is simply no extent for the thing the capability was
/// taken at, and no arrangement of the rules already here will produce one.
const MIDWAY_NO_EXTENT: &str =
    "bounds check left alone, nothing here says how far the object its capability names runs";

/// Recorded for a midway bounds check whose walk reaches outside what is known about the pointer.
///
/// The honest refusals. Something is known about the pointer the capability names and the addresses
/// the walk can reach are not all inside it, which is either a range that could be tighter or an
/// access that really can go out of the object.
const MIDWAY_OVER: &str =
    "bounds check left alone, its walk can reach outside what is known about the object";

/// Recorded for a bounds check about a range the program worked out.
const COMPUTED_EXTENT: &str =
    "bounds check left alone, how many bytes it covers is a number only the program has";

/// Recorded for a plane check about a range the program worked out.
const COMPUTED_EXTENT_PLANE: &str =
    "plane check left alone, how many bytes it covers is a number only the program has";

/// Recorded for a lifetime check whose operands this pass cannot read.
const UNKNOWN_SHAPE_LIVE: &str =
    "lifetime check left alone, its pointer is not a base and a constant";

/// The same as [`MIDWAY_CAPABILITY`], for a lifetime check.
const MIDWAY_CAPABILITY_LIVE: &str =
    "lifetime check left alone, its capability names a pointer further back than its base";

/// The same as [`MIDWAY_NO_EXTENT`], for a lifetime check.
const MIDWAY_NO_EXTENT_LIVE: &str =
    "lifetime check left alone, nothing here says how far the object its capability names runs";

/// The same as [`MIDWAY_OVER`], for a lifetime check.
const MIDWAY_OVER_LIVE: &str =
    "lifetime check left alone, its walk can reach outside what is known about the object";

/// Recorded once for each derivation check taken out.
const REMOVED_DERIV: &str =
    "derivation check removed, one checked range holds both the pointer and where it walked to";

/// Recorded once for each derivation check taken out because it walked inside a local.
const REMOVED_DERIV_LOCAL: &str =
    "derivation check removed, it walks inside a local this function declares";

/// Recorded once for each derivation check taken out because it walked inside a global.
const REMOVED_DERIV_STATIC: &str =
    "derivation check removed, it walks inside an object of static storage duration";

/// Recorded once for each derivation check taken out because every caller hands in the object.
const REMOVED_DERIV_HANDED: &str = "derivation check removed, it walks inside an object every call \
                                    to this function hands it";

/// Recorded once for each derivation check taken out because an allocator made the object.
const REMOVED_DERIV_MADE: &str = "derivation check removed, it walks inside an object an allocator \
                                  made and this function has tested";

/// Recorded for a derivation check that would have gone if there had been fuel for it.
const NO_FUEL_DERIV: &str = "derivation check kept, the pass ran out of fuel";

/// Recorded for a derivation check a call cost.
const PAST_A_CALL_DERIV: &str =
    "derivation check kept, a call between it and the range that holds both ends might free";

/// Recorded for a derivation check naming a capability that is not the one it is about.
const NOT_ITS_CAPABILITY_DERIV: &str = "derivation check left alone, the capability it names is not the one the pointer that went in \
     carries";

/// Recorded for a derivation check whose two ends are not off one value.
const TWO_BASES_DERIV: &str =
    "derivation check left alone, its two pointers are not built on one base";

/// Recorded for a derivation check whose walk can reach past the end of the local it starts in.
const OVER_THE_LOCAL_DERIV: &str =
    "derivation check left alone, the walk can reach past the end of the local it starts in";

/// Recorded for a derivation check on a pointer this function loaded out of memory.
const NO_EXTENT_LOADED: &str = "derivation check left alone, nothing here says how big the object \
                                is and the pointer to it was loaded from memory";

/// Recorded for a derivation check on a pointer this function was handed.
const NO_EXTENT_HANDED: &str = "derivation check left alone, nothing here says how big the object \
                                is and the pointer to it was handed to this function";

/// Recorded for a derivation check on a pointer into a global.
const NO_EXTENT_GLOBAL: &str = "derivation check left alone, nothing here says how big the object \
                                is and the pointer to it is into a global";

/// Recorded for a derivation check on a pointer a call handed back.
const NO_EXTENT_RETURNED: &str = "derivation check left alone, nothing here says how big the \
                                  object is and the pointer to it came back from a call";

/// Recorded for a derivation check on a pointer none of the shapes above describes.
const NO_EXTENT_OTHER: &str =
    "derivation check left alone, nothing here says how big the object its pointers are in is";

/// The pass. It holds nothing, because everything it works out is about one function.
/// Which of the places a fact comes from a run of this pass may ask.
///
/// Everything is asked normally and there is one pass in the pipeline. The others are here for the
/// measurement `spec/safe-memory/13-performance.md` section 13.5 asks for and
/// `spec/safe-memory/17-open-questions.md` question 3 is: how much each source discharges on its
/// own, and how much the same sources discharge together. A number for a source on its own cannot
/// be read off the remarks of a full run, because the rules are asked in an order and whichever one
/// answers first is the one the remark names, so the second source to be asked about a check two of
/// them could answer looks like it answered nothing.
///
/// The four are document 07 section 7.2's four, with the caveat the measurement found: the ranges
/// are not a fourth kind of fact but a way of asking the other three about a subscript instead of
/// about an address written out in the program.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sources {
    /// How big an object is, read off whatever made it. A global's extent comes from
    /// `crate::extents`, a local's from its `alloca`, an allocation's from the call `crate::heap`
    /// marked. Section 7.2's first source.
    objects: bool,
    /// What a check that has already run established, carried down the dominator tree. Section 7.3,
    /// and the one the literature calls redundant check elimination.
    dominance: bool,
    /// What every caller of this function guarantees about what it was handed, from
    /// `crate::params`. Section 7.5.
    summaries: bool,
    /// The value ranges and the recurrences, which widen the one address a check names into the
    /// range of addresses a walk can reach so that the other three can be asked about a subscript.
    /// Section 7.4, and the half of the PICO result this pass holds. The other half is
    /// [`crate::hoist`] and [`crate::split`], which are passes of their own and have flags of their
    /// own.
    ranges: bool,
}

impl Sources {
    /// Every one of them, which is what the pipeline runs.
    pub const ALL: Self = Self { objects: true, dominance: true, summaries: true, ranges: true };
    /// What an object says about itself and nothing else.
    pub const OBJECTS: Self =
        Self { objects: true, dominance: false, summaries: false, ranges: false };
    /// What an earlier check established and nothing else.
    pub const DOMINANCE: Self =
        Self { objects: false, dominance: true, summaries: false, ranges: false };
    /// What every caller guarantees and nothing else.
    pub const SUMMARIES: Self =
        Self { objects: false, dominance: false, summaries: true, ranges: false };
    /// Every fact, asked only about addresses written out in the program.
    pub const NARROW: Self =
        Self { objects: true, dominance: true, summaries: true, ranges: false };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Discharge {
    /// What `-f<name>` and `-fno-<name>` reach this run by.
    name: &'static str,
    /// Which places it may take a fact from. See [`Sources`].
    sources: Sources,
}

/// The pass the pipeline runs, which asks everything.
pub static DISCHARGE: Discharge = Discharge { name: "discharge", sources: Sources::ALL };

/// The same pass asking an object how big it is and nothing else.
pub static OBJECTS: Discharge = Discharge { name: "discharge-objects", sources: Sources::OBJECTS };

/// The same pass asking what an earlier check established and nothing else.
pub static DOMINANCE: Discharge =
    Discharge { name: "discharge-dominance", sources: Sources::DOMINANCE };

/// The same pass asking what every caller guarantees and nothing else.
pub static SUMMARIES: Discharge =
    Discharge { name: "discharge-summaries", sources: Sources::SUMMARIES };

/// The same pass asking every fact, about addresses written out in the program only.
pub static NARROW: Discharge = Discharge { name: "discharge-narrow", sources: Sources::NARROW };

/// The same pass asking everything, under a name of its own.
///
/// [`DISCHARGE`] already asks everything, so this looks like a duplicate and is not. A pass the level
/// did not choose goes on the end of the pipeline, so a run of `-fno-discharge -fdischarge-objects`
/// asks its question in a different place from where the shipped pass asks it, and the two numbers
/// are not comparable. This one is turned on the same way as the others and lands in the same place,
/// so the sum of the parts and the whole are measured under one arrangement. What it costs against
/// [`DISCHARGE`] is what the position is worth, which is a number the measurement wants anyway.
pub static EVERY: Discharge = Discharge { name: "discharge-every", sources: Sources::ALL };

impl Pass for Discharge {
    fn name(&self) -> &'static str {
        self.name
    }

    fn describe(&self) -> &'static str {
        "a bounds, lifetime or derivation check whose answer is already known is removed"
    }

    fn preserves(&self) -> Preserved {
        // Instructions go and blocks do not. A check is not a terminator and removing one leaves
        // every edge where it was. What it does not leave where it was is the liveness, because
        // the check was reading something and now nothing is.
        Preserved::ALL.without(Analysis::Liveness)
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        let Some(entry) = func.entry() else { return stats };
        let wrote = writers(func, an.purity());
        let dom = an.dominators(func);
        let graph = an.cfg(func);
        let kills = Kills::of(func, &wrote);

        // The graph is built for two reasons and neither is the common one, so a function with
        // neither pays for no copy of it. The ranges want it when there is a walk the constant
        // reader gives up on, and the allocation rule wants it to find where the program has tested
        // what an allocator gave it.
        let walks = self.sources.ranges && walks_by_a_value(func);
        let cfg = (walks
            || joins_a_pointer(func, entry)
            || (self.sources.objects && heap::allocates(func)))
        .then(|| an.cfg(func));
        let mut ranges = cfg.filter(|_| walks).map(|cfg| Ranges::new(&*func, cfg, dom));

        // One answer per allocation rather than one per check, because a function that reads twenty
        // fields of the same object asks the same question about the same pointer twenty times.
        let mut checked: HashMap<Value, HashSet<Block>> = HashMap::new();

        // Whether anything in here says a lifetime is over. Read once over the whole function
        // rather than carried down the walk, because what the frame slot rule needs is that no
        // `meta_end` runs before the check on any path, and a fact carried down the dominator
        // tree only ever says something about the paths that go through one block.
        let ends = ends_a_lifetime(func);

        // The walk is a stack rather than recursion because the dominator tree of a long chain of
        // blocks is as deep as the function is long, and a pass is not a place to find that out.
        // Each block carries its own copy of what holds at its start, which is what makes a fact a
        // call killed in one arm of a branch still hold in the other.
        let mut going: Vec<(Inst, &'static str)> = Vec::new();
        let mut work = vec![(entry, Scope::default())];
        while let Some((block, mut scope)) = work.pop() {
            for inst in func.insts(block).collect::<Vec<Inst>>() {
                match opaque(func, &wrote, inst) {
                    Some(Opaque::Called) => {
                        scope.called();
                        continue;
                    }
                    Some(Opaque::Wrote) => {
                        scope.wrote();
                        continue;
                    }
                    Some(Opaque::Everything) => {
                        scope.forget();
                        continue;
                    }
                    None => {}
                }
                match func[inst].opcode {
                    Opcode::CheckBounds => {
                        if func[func[inst].args].len() > 2 {
                            stats.missed(COMPUTED_EXTENT);
                            scope.proved(func, inst);
                            continue;
                        }
                        let Some(asked) = about(func, inst) else {
                            // The constant reader could not name the address, so the rules that
                            // take one address never run. The range reader can, past the step the
                            // other one stopped at, and only when the base it lands on is the
                            // pointer the capability names.
                            let mid = midway(func, inst);
                            let size = match func[inst].extra {
                                Extra::Mem(info) => i128::from(func[info].size),
                                _ => 0,
                            };
                            let span =
                                mid.then(|| beyond(func, ranges.as_mut(), inst, size)).flatten();
                            let wide = span
                                .filter(|wide| {
                                    (self.sources.objects
                                        && declared(func, wide.base)
                                            .is_some_and(|local| reaches(&local, wide)))
                                        || (self.sources.objects
                                            && allocated_around(
                                                func,
                                                cfg,
                                                &mut checked,
                                                block,
                                                &[wide],
                                            ))
                                        || (self.sources.dominance && scope.bounds.reaches(wide))
                                })
                                .filter(|_| match aligned(func, cfg, &scope.aligns, inst) {
                                    Alignment::Answered => true,
                                    Alignment::Unknown => {
                                        stats.missed(UNKNOWN_ALIGNMENT);
                                        false
                                    }
                                    Alignment::Lost => {
                                        stats.missed(LOST_ALIGNMENT);
                                        false
                                    }
                                });
                            if wide.is_some() {
                                if fuel.take() {
                                    going.push((inst, REMOVED_MIDWAY));
                                } else {
                                    stats.missed(NO_FUEL);
                                    scope.proved(func, inst);
                                }
                                continue;
                            }
                            stats.missed(if mid {
                                why_midway(func, &scope.bounds, span, false)
                            } else {
                                UNKNOWN_SHAPE
                            });
                            scope.proved(func, inst);
                            continue;
                        };
                        // The four objects whose extent is known without anybody having checked
                        // it. A global was worked out over the module by `crate::extents` and an
                        // object every caller hands in by `crate::params`, both of which arrive as
                        // a flag; a local is read off its `alloca` here and an allocation off the
                        // call `crate::heap` marked. All four are asked of the same rule as every
                        // other fact. The reach of a walk the constant reader could not finish is
                        // asked last, because it is the only one that costs an analysis to answer.
                        let why = if self.sources.objects
                            && func[inst].flags.contains(Flags::STATIC)
                        {
                            Some(REMOVED_STATIC)
                        } else if self.sources.summaries && func[inst].flags.contains(Flags::HANDED)
                        {
                            Some(REMOVED_HANDED)
                        } else if self.sources.objects
                            && declared(func, asked.base)
                                .is_some_and(|local| covers(&local, &asked))
                        {
                            Some(REMOVED_LOCAL)
                        } else if self.sources.objects
                            && allocated(func, cfg, &mut checked, block, &[&asked])
                        {
                            Some(REMOVED_MADE)
                        } else if self.sources.dominance && scope.bounds.covers(&asked) {
                            Some(REMOVED)
                        } else {
                            // The same four sources in the same order, asked of the range of
                            // addresses the walk can reach rather than of the one address the
                            // constant reader could name. A flag has already been read above and
                            // reading it again would say the same thing, so what is left is the
                            // local, the allocation and what the walk carries.
                            reach(func, ranges.as_mut(), &asked, inst).and_then(|wide| {
                                if self.sources.objects
                                    && declared(func, wide.base)
                                        .is_some_and(|local| reaches(&local, &wide))
                                {
                                    Some(REMOVED_RANGE)
                                } else if self.sources.objects
                                    && allocated_around(func, cfg, &mut checked, block, &[&wide])
                                {
                                    Some(REMOVED_MADE)
                                } else if self.sources.dominance && scope.bounds.reaches(&wide) {
                                    Some(REMOVED_RANGE)
                                } else {
                                    None
                                }
                            })
                        };
                        // Asked once a rule has answered the bounds rather than in front of them
                        // all, because a check that was staying anyway costs the gate nothing and
                        // the number somebody reads has to be what it actually costs. A check kept
                        // here still runs, so it still establishes what it was about.
                        let why = why.filter(|_| match aligned(func, cfg, &scope.aligns, inst) {
                            Alignment::Answered => true,
                            Alignment::Unknown => {
                                stats.missed(UNKNOWN_ALIGNMENT);
                                false
                            }
                            Alignment::Lost => {
                                stats.missed(LOST_ALIGNMENT);
                                false
                            }
                        });
                        let Some(why) = why else {
                            if scope.bounds.covered_before(&asked) {
                                stats.missed(PAST_A_CALL);
                            }
                            // A check that stays is a check that runs, and a check that runs
                            // establishes what it was about. One that was removed establishes
                            // nothing new: whatever covered it covers everything it would have.
                            // Both halves of what it was about, since the alignment conjunct rides
                            // on this check and the gate above may well be the thing that kept it.
                            scope.bounds.held.push(asked);
                            scope.proved(func, inst);
                            continue;
                        };
                        if !fuel.take() {
                            stats.missed(NO_FUEL);
                            scope.bounds.held.push(asked);
                            scope.proved(func, inst);
                            continue;
                        }
                        // A check that goes normally establishes nothing new, because whatever
                        // answered it covers everything it would have. The range is the one
                        // exception: what answered it was a fact about a made up range around the
                        // address, and the next check on these bytes has to ask for that range
                        // again and may not get the same answer. So the narrow fact goes in, which
                        // is the thing that was actually proved.
                        if why == REMOVED_RANGE {
                            scope.bounds.held.push(asked);
                        }
                        going.push((inst, why));
                    }
                    Opcode::CheckLive => {
                        let Some(asked) = alive(func, inst) else {
                            // The bounds arm's paragraph, and the same two rules this arm already
                            // asks of a range, which is a local that holds everything the walk can
                            // reach and a lifetime fact that does.
                            let mid = midway(func, inst);
                            let span =
                                mid.then(|| beyond(func, ranges.as_mut(), inst, 1)).flatten();
                            let wide = span.filter(|wide| {
                                (self.sources.objects
                                    && !ends
                                    && declared(func, wide.base)
                                        .is_some_and(|local| reaches(&local, wide)))
                                    || (self.sources.dominance && scope.alive.reaches(wide))
                            });
                            if wide.is_some() {
                                if fuel.take() {
                                    going.push((inst, REMOVED_MIDWAY_LIVE));
                                } else {
                                    stats.missed(NO_FUEL_LIVE);
                                }
                                continue;
                            }
                            stats.missed(if mid {
                                why_midway(func, &scope.alive, span, true)
                            } else {
                                UNKNOWN_SHAPE_LIVE
                            });
                            continue;
                        };
                        // A global is alive as long as the program is, and a frame slot is alive
                        // until the function returns, so both objects whose extent is known
                        // without anybody having checked it answer this as well as a bounds
                        // check. `ends` is what makes the second one true: where a local stops
                        // being alive is written into the IR as `meta_end` and not read off the
                        // shape of the source, so a function with one in it is a function this
                        // does not claim anything about.
                        let why = if self.sources.objects
                            && func[inst].flags.contains(Flags::STATIC)
                        {
                            Some(REMOVED_LIVE_STATIC)
                        } else if self.sources.summaries && func[inst].flags.contains(Flags::HANDED)
                        {
                            Some(REMOVED_LIVE_HANDED)
                        } else if self.sources.objects
                            && !ends
                            && declared(func, asked.base)
                                .is_some_and(|local| covers(&local, &asked))
                        {
                            Some(REMOVED_LIVE_LOCAL)
                        } else if self.sources.dominance && scope.alive.covers(&asked) {
                            Some(REMOVED_LIVE)
                        } else {
                            // A lifetime fact and not a bounds one, because what is being asked
                            // is whether the storage is alive and a bounds check that passed says
                            // nothing about that. The widening argument is the bounds arm's: a
                            // range known alive that holds every address the walk can reach holds
                            // the one it actually uses.
                            reach(func, ranges.as_mut(), &asked, inst)
                                .filter(|wide| {
                                    (self.sources.objects
                                        && !ends
                                        && declared(func, wide.base)
                                            .is_some_and(|local| reaches(&local, wide)))
                                        || (self.sources.dominance && scope.alive.reaches(wide))
                                })
                                .map(|_| REMOVED_LIVE_RANGE)
                        };
                        let Some(why) = why else {
                            if scope.alive.covered_before(&asked) {
                                stats.missed(PAST_A_CALL_LIVE);
                            }
                            scope.alive.held.push(widened(func, &scope.bounds, asked));
                            continue;
                        };
                        if !fuel.take() {
                            stats.missed(NO_FUEL_LIVE);
                            scope.alive.held.push(widened(func, &scope.bounds, asked));
                            continue;
                        }
                        // The bounds arm's exception, for its reason. A range answered a made up
                        // range around this address, so what was proved is about the address.
                        if why == REMOVED_LIVE_RANGE {
                            scope.alive.held.push(widened(func, &scope.bounds, asked));
                        }
                        going.push((inst, why));
                    }
                    Opcode::CheckDeriv => {
                        let narrow = derives(func, inst);
                        let why = narrow.and_then(|(from, to)| {
                            if self.sources.objects && func[inst].flags.contains(Flags::STATIC) {
                                Some(REMOVED_DERIV_STATIC)
                            } else if self.sources.summaries
                                && func[inst].flags.contains(Flags::HANDED)
                            {
                                Some(REMOVED_DERIV_HANDED)
                            } else if self.sources.objects
                                && declared(func, from.base).is_some_and(|local| {
                                    covers(&local, &from) && covers(&local, &to)
                                })
                            {
                                Some(REMOVED_DERIV_LOCAL)
                            } else if self.sources.objects
                                && allocated(func, cfg, &mut checked, block, &[&from, &to])
                            {
                                Some(REMOVED_DERIV_MADE)
                            } else if self.sources.dominance && scope.bounds.holds_both(&from, &to)
                            {
                                Some(REMOVED_DERIV)
                            } else {
                                None
                            }
                        });
                        // Asked last, and asked off the check's own operands rather than off what
                        // `derives` worked out, because the case it is for is the one `derives`
                        // cannot read at all: past a step the constant reader gives up on the two
                        // ends are not one base and two constants. One thing has to hold both of
                        // the ranges, for the same reason one thing has to hold both of the
                        // addresses, which is that two things saying each end is inside something
                        // say nothing about it being the same something.
                        let why = why.or_else(|| {
                            spread(func, ranges.as_mut(), inst, inst).and_then(|(near, far)| {
                                if self.sources.objects
                                    && declared(func, near.base).is_some_and(|local| {
                                        reaches(&local, &near) && reaches(&local, &far)
                                    })
                                {
                                    Some(REMOVED_DERIV_RANGE)
                                } else if self.sources.objects
                                    && allocated_around(
                                        func,
                                        cfg,
                                        &mut checked,
                                        block,
                                        &[&near, &far],
                                    )
                                {
                                    Some(REMOVED_DERIV_MADE)
                                } else if self.sources.dominance
                                    && scope.bounds.reaches_both(&near, &far)
                                {
                                    Some(REMOVED_DERIV_RANGE)
                                } else {
                                    None
                                }
                            })
                        });
                        let Some(why) = why else {
                            match narrow {
                                Some((from, to)) => {
                                    if scope.bounds.held_both_before(&from, &to) {
                                        stats.missed(PAST_A_CALL_DERIV);
                                    }
                                }
                                None => {
                                    stats.missed(unreadable(func, ranges.as_mut(), inst));
                                }
                            }
                            continue;
                        };
                        if !fuel.take() {
                            stats.missed(NO_FUEL_DERIV);
                            continue;
                        }
                        going.push((inst, why));
                    }
                    Opcode::CheckInit => {
                        if func[func[inst].args].len() > 2 {
                            stats.missed(COMPUTED_EXTENT_PLANE);
                            continue;
                        }
                        let Some(asked) = about(func, inst) else {
                            stats.missed(UNKNOWN_SHAPE_INIT);
                            continue;
                        };
                        let why = if scope.written.covers(&asked) {
                            Some(REMOVED_INIT)
                        } else if scope.stored.covers(&asked) {
                            Some(REMOVED_STORED)
                        } else {
                            None
                        };
                        let Some(why) = why.filter(|_| self.sources.dominance) else {
                            let before = scope.written.covered_before(&asked)
                                || scope.stored.covered_before(&asked);
                            stats.missed(if before { PAST_A_CALL_INIT } else { NOTHING_WROTE_IT });
                            scope.written.held.push(asked);
                            continue;
                        };
                        if !fuel.take() {
                            stats.missed(NO_FUEL_INIT);
                            scope.written.held.push(asked);
                            continue;
                        }
                        going.push((inst, why));
                    }
                    Opcode::CheckType => {
                        if func[func[inst].args].len() > 2 {
                            stats.missed(COMPUTED_EXTENT_PLANE);
                            continue;
                        }
                        let Some((node, asked)) = holding(func, inst) else {
                            stats.missed(UNKNOWN_SHAPE_TYPE);
                            continue;
                        };
                        let held = scope.typed.entry(node).or_default();
                        if !(self.sources.dominance && held.covers(&asked)) {
                            stats.missed(if held.covered_before(&asked) {
                                PAST_A_CALL_TYPE
                            } else {
                                NOTHING_TYPED_IT
                            });
                            held.held.push(asked);
                            continue;
                        }
                        if !fuel.take() {
                            stats.missed(NO_FUEL_TYPE);
                            held.held.push(asked);
                            continue;
                        }
                        going.push((inst, REMOVED_TYPE));
                    }
                    // The two ways bytes that were written stop counting as written without a call
                    // being involved. A `meta_begin` is a lifetime starting, which is the storage
                    // becoming fresh again, and a `meta_init_copy` carries whatever the source said
                    // about its own bytes, which for an uninitialized source is that the
                    // destination is uninitialized too. Neither says anything about bounds or about
                    // lifetime, so neither goes through `opaque`.
                    Opcode::MetaBegin | Opcode::MetaInitCopy => {
                        scope.written.forget();
                        scope.stored.forget();
                        // Only the first of the two touches the type plane. A lifetime starting is
                        // storage nobody has stored through yet, which holds no type, and a copy of
                        // the init plane moves init entries and nothing else.
                        if func[inst].opcode == Opcode::MetaBegin {
                            scope.retyped(None);
                        }
                    }
                    // A store's write to the init plane, which says the bytes it covers have been
                    // written as plainly as a `check_init` that passed over them does, and says it
                    // without anything having to be asked. What kills a fact from a check kills
                    // one from here, for the same reasons, since both are a claim about the plane
                    // and nothing else. A width the reader cannot name is a write that may cover
                    // anything, and it adds nothing rather than taking anything away, because
                    // setting more bytes written never made a fact about written bytes false.
                    Opcode::MetaInit => {
                        if let Some(write) = crate::coalesce::read(func, inst, Opcode::MetaInit) {
                            scope.stored.held.push(Fact::range(write.base, write.at, write.size));
                        }
                    }
                    // The type plane's copy, which carries whatever the source said and this pass
                    // has no idea what that was. It says nothing about any other plane.
                    Opcode::MetaTypeCopy => {
                        scope.retyped(None);
                    }
                    // A store's judgement, which is the one plane write that names a type.
                    // `Scope::retyped` is the argument for keeping its own entry's facts.
                    Opcode::MetaType => {
                        let node = match func[inst].extra {
                            Extra::Node(node) => Some(node),
                            _ => None,
                        };
                        scope.retyped(node);
                    }
                    _ => continue,
                }
            }
            for child in dom.children(block) {
                let mut scope = scope.clone();
                scope.crossed(kills.between(graph, block, child));
                work.push((child, scope));
            }
        }

        for (inst, why) in going {
            func.remove_inst(inst);
            stats.optimized(why);
        }
        stats
    }
}

/// A range of bytes some check has already been passed on, or is being asked about.
///
/// The address is kept as the value it was computed from and the constant distance from it, rather
/// than as the pointer itself, because that is what makes two of these comparable: the whole of
/// what this pass knows about two addresses is that they are one value plus two constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fact {
    /// The value the address was computed from.
    pub(crate) base: Value,
    /// How far past it the access starts.
    pub(crate) offset: i128,
    /// How many bytes it covers.
    size: i128,
}

impl Fact {
    /// The whole of an object whose extent is known, starting at its own address.
    ///
    /// The two sources of one of these are an `alloca` of a fixed size and a global, and what they
    /// have in common is that the size is said by something other than a check that passed.
    pub(crate) fn whole(base: Value, size: i128) -> Self {
        Self { base, offset: 0, size }
    }

    /// A range of bytes named by where it starts and how far it runs.
    ///
    /// The general form of [`Fact::whole`], for a caller that has both ends of a range in hand
    /// rather than an object. `crate::dead_plane` is the one, and what it has is a plane write
    /// rather than an access, which is a different thing to be about and the same thing to ask.
    pub(crate) fn range(base: Value, offset: i128, size: i128) -> Self {
        Self { base, offset, size }
    }
}

/// A range of addresses an access can land in, and how many bytes it takes when it does.
///
/// What [`reach`] works out and the only thing it is used for. It is deliberately not a [`Fact`]:
/// a fact is something that was established and may be recorded, and this is a question and may
/// not. The address the program uses is `base` plus somewhere between `low` and `low` plus `width`
/// further along, and what a check proves when it runs is about that one address rather than about
/// the range this was made out of.
#[derive(Debug, Clone, Copy)]
struct Reach {
    /// The value the address was computed from.
    base: Value,
    /// The nearest the access can start to it.
    low: i128,
    /// How much further than that it can start.
    width: i128,
    /// How many bytes it covers.
    size: i128,
}

/// One kind of fact, and what has become of it.
#[derive(Debug, Clone, Default)]
struct Known {
    /// The ranges a check has been passed on and nothing has cast doubt on since.
    held: Vec<Fact>,
    /// The ones a call threw away, kept only so that the cost of throwing them away is a number
    /// somebody can read rather than a paragraph somebody has to believe.
    lost: Vec<Fact>,
    /// Where in [`Known::held`] the facts established since the last call begin.
    ///
    /// Everything in front of that index is a fact that was true when it was established and has
    /// had a call run over it since. For the bounds half those facts still answer a bounds check,
    /// which is what [`Known::crossed`] is about, and there are two questions they may not answer.
    /// An index rather than a flag on each fact because nothing ever takes one out of the middle:
    /// the vector is pushed and emptied and never anything else, so the ones from before the call
    /// are exactly the ones in front of a mark.
    since: usize,
}

impl Known {
    /// Whether something still standing answers this.
    fn covers(&self, asked: &Fact) -> bool {
        self.held.iter().any(|fact| covers(fact, asked))
    }

    /// Whether something still standing answers a range of addresses an access can land in.
    fn reaches(&self, asked: &Reach) -> bool {
        self.held.iter().any(|fact| reaches(fact, asked))
    }

    /// The facts no call has run over, which is the only kind two of the rules may read.
    fn fresh(&self) -> &[Fact] {
        let from = self.since.min(self.held.len());
        &self.held[from..]
    }

    /// The facts a call has run over, which is what the cost of not reading them is counted from.
    fn stale(&self) -> impl Iterator<Item = &Fact> {
        let upto = self.since.min(self.held.len());
        self.held[..upto].iter().chain(self.lost.iter())
    }

    /// Whether one thing still standing answers both of these ranges.
    ///
    /// One rather than one each, for the reason [`Known::holds_both`] gives, and the reason does
    /// not change when the ends are ranges instead of addresses.
    fn reaches_both(&self, from: &Reach, to: &Reach) -> bool {
        self.fresh().iter().any(|fact| reaches(fact, from) && reaches(fact, to))
    }

    /// Whether something would have answered it before a call came along.
    fn covered_before(&self, asked: &Fact) -> bool {
        self.stale().any(|fact| covers(fact, asked))
    }

    /// Whether one thing still standing answers both of these.
    ///
    /// One rather than one each, which is the whole point of asking it this way. Two facts saying
    /// two addresses are each inside some instance say nothing about whether it is the same
    /// instance, and that is the only thing a derivation check wants to know.
    fn holds_both(&self, from: &Fact, to: &Fact) -> bool {
        self.fresh().iter().any(|fact| covers(fact, from) && covers(fact, to))
    }

    /// Whether one would have answered both before a call came along.
    fn held_both_before(&self, from: &Fact, to: &Fact) -> bool {
        self.stale().any(|fact| covers(fact, from) && covers(fact, to))
    }

    /// Gives up everything, because something happened that this pass cannot see through.
    fn forget(&mut self) {
        self.lost.append(&mut self.held);
        self.since = 0;
    }

    /// Keeps everything and marks it as having had a call run over it.
    ///
    /// The bounds half only, and the module comment's section on what keeping it takes is the whole
    /// argument for why that is allowed. In one line: the lifetime check beside the access is still
    /// there, it compares the version the capability carries against the plane's, and a range that
    /// was inside one instance is inside that instance still or is about to be refused.
    fn crossed(&mut self) {
        self.since = self.held.len();
    }
}

/// What holds where the walk has got to.
///
/// The two kinds are apart because they are killed together and answered separately: a range being
/// inside one instance and that instance being alive are different claims, and reporting them as
/// one number would hide which of the two a check is still being paid for.
#[derive(Debug, Clone, Default)]
struct Scope {
    /// Ranges a `check_bounds` established are inside one storage instance.
    bounds: Known,
    /// Ranges a `check_live` established are in an instance that is alive.
    alive: Known,
    /// Ranges a `check_init` established have had every byte written.
    ///
    /// Kept apart from the other two for the reason those two are kept apart from each other. A
    /// range being inside one instance, that instance being alive, and the bytes in it having been
    /// written are three claims, and a check that passes establishes exactly one of them.
    written: Known,
    /// Ranges a `meta_init` wrote, the same claim as [`Scope::written`] reached another way.
    ///
    /// Apart only so that the report can say which of the two answered. A store and a check that
    /// passed both leave the plane saying those bytes are written, and one is given up exactly
    /// where the other is.
    stored: Known,
    /// Ranges a `check_type` established agree with a type, one set of ranges per plane entry.
    ///
    /// A fact here is not quite what the check is named after. What a `check_type` that passes
    /// establishes is that the plane over those bytes is compatible with the entry it asked with,
    /// which is the plane holding that entry or the plane holding the untyped one, and this pass
    /// cannot tell which. That is enough to answer a later check asking with the same entry and it
    /// is not enough to answer one asking with any other, so the entry is the key rather than a
    /// field, and a lookup that misses is the honest answer for every other type.
    typed: HashMap<Meta, Known>,
    /// What a `check_bounds` that ran proved about where the address it was about starts.
    ///
    /// Nothing here is ever given up, and that is the difference between this and the other two.
    /// They are facts about storage, and storage can be freed and handed back out, which is the
    /// whole of what a call does to them. This is a fact about the number a value holds, and
    /// nothing in a function changes the number an SSA value holds. So it survives a call, it
    /// survives inline assembly and it survives a `meta_end`, and dominance is the only thing that
    /// bounds it, which the walk already handles by giving each child its own copy.
    aligns: HashMap<Value, u64>,
}

impl Scope {
    /// Gives up every fact of either kind. The alignment facts are not one of the two, and
    /// [`Scope::aligns`] says why they are not given up here or anywhere else.
    fn forget(&mut self) {
        self.bounds.forget();
        self.alive.forget();
        self.written.forget();
        self.stored.forget();
        self.retyped(None);
    }

    /// Gives up the type facts that a plane write over an unknown range has made unsafe to keep.
    ///
    /// A `meta_type` naming entry `E` puts `E` over the bytes it covers and leaves every other byte
    /// where it was, so a range this pass recorded as agreeing with `E` agrees with `E` still,
    /// wherever the write landed. No such argument holds for any other entry, and where the write
    /// landed is the thing this pass does not know, so every other entry's facts go. A caller with
    /// no entry to spare, which is a copy or a lifetime starting or anything opaque, passes `None`
    /// and loses the lot.
    fn retyped(&mut self, kept: Option<Meta>) {
        for (&node, facts) in &mut self.typed {
            if Some(node) != kept {
                facts.forget();
            }
        }
    }

    /// Records what a `check_bounds` that is staying proves about where its address starts.
    ///
    /// The check runs the alignment conjunct, which is `addr & (align - 1) != 0` in the runtime's
    /// `bounds`, and refuses when it does not hold. So on any path past the check the address is a
    /// multiple of what the access assumed, and the first access through a pointer somebody handed
    /// in proves for nothing what every later access through the same value needs.
    ///
    /// Only for a check that stays. One that is removed does not run and proves nothing, and it
    /// needs nothing either, since [`aligned`] answered it before it was allowed to go.
    fn proved(&mut self, func: &Func, check: Inst) {
        let Extra::Mem(info) = func[check].extra else { return };
        let claim = u64::from(func[info].align);
        let Some(&pointer) = func[func[check].args].get(1) else { return };
        if claim > 1 {
            let held = self.aligns.entry(pointer).or_default();
            *held = (*held).max(claim);
        }
    }

    /// Gives up the lifetime facts and the initialization ones and the type ones, and keeps the
    /// bounds ones, marked as a call having run over them.
    ///
    /// The initialization facts and the type facts go with the lifetime ones rather than with the
    /// bounds. There is a version of the bounds argument that would keep them, since storage handed
    /// back out and handed over again comes with a capability whose version no longer matches and
    /// the lifetime check beside the access is what notices, but it rests on that check still being
    /// there, which is true only because this pass gives up the lifetime facts at the same point.
    /// Resting one rule on another rule's conservatism is worth a measurement before it is worth
    /// writing. tamnd/rucc#1617.
    fn called(&mut self) {
        self.bounds.crossed();
        self.alive.forget();
        self.wrote();
    }

    /// Does to the facts what the blocks between the walk's last block and its next one may have.
    fn crossed(&mut self, crossed: Crossed) {
        if crossed.everything {
            self.forget();
            return;
        }
        if crossed.called {
            self.called();
        }
        if crossed.wrote {
            self.wrote();
        }
        if crossed.unwritten {
            self.written.forget();
            self.stored.forget();
        }
        match crossed.retyped {
            Retyped::Untouched => {}
            Retyped::Only(node) => self.retyped(Some(node)),
            Retyped::Anyhow => self.retyped(None),
        }
    }

    /// Gives up the initialization facts and the type facts, which is what a call that frees
    /// nothing and may write memory does to them. The module comment has the argument.
    fn wrote(&mut self) {
        self.written.forget();
        self.stored.forget();
        self.retyped(None);
    }
}

/// What the blocks between a block's immediate dominator and the block itself may have done to the
/// facts the walk is carrying.
///
/// The walk hands a block what held at the end of its immediate dominator, and that is only what
/// holds at the start of the block when every path from the one to the other runs through nothing
/// that kills a fact. A block with one predecessor, which is its dominator, is that case. Any other
/// block is reached through blocks the walk visits somewhere else in the tree: the arms of a branch
/// before the join, or the body of a loop before its header is entered again. A `free` in one arm
/// ends the lifetime the check in front of the branch found alive, and the read after the join is
/// then a read of freed storage on that path, so what those blocks do has to be done to the facts
/// before the block sees them. Which instruction in a block does it and in what order does not
/// matter, because the facts are given up at the start of the later block whatever happened, so a
/// block's effect is kept as one summary of everything in it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Crossed {
    /// Something the pass cannot see through, which gives up everything.
    everything: bool,
    /// A call that might free.
    called: bool,
    /// A call that frees nothing and may write.
    wrote: bool,
    /// A `meta_begin` or a `meta_init_copy`, which can make written bytes unwritten.
    unwritten: bool,
    /// What happened to the type plane.
    retyped: Retyped,
}

/// What some blocks did to the type plane, as [`Scope::retyped`] needs to be told it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Retyped {
    /// Nothing wrote it.
    #[default]
    Untouched,
    /// Only `meta_type` naming this one entry wrote it, which leaves that entry's facts standing.
    Only(Meta),
    /// Anything else.
    Anyhow,
}

impl Retyped {
    /// Both of two things having happened.
    fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::Untouched, done) | (done, Self::Untouched) => done,
            (Self::Only(one), Self::Only(two)) if one == two => Self::Only(one),
            _ => Self::Anyhow,
        }
    }
}

impl Crossed {
    /// Both of two things having happened.
    fn and(self, other: Self) -> Self {
        Self {
            everything: self.everything || other.everything,
            called: self.called || other.called,
            wrote: self.wrote || other.wrote,
            unwritten: self.unwritten || other.unwritten,
            retyped: self.retyped.and(other.retyped),
        }
    }

    /// What one instruction does, which is what the walk does with it when it reaches it.
    fn of(func: &Func, wrote: &HashSet<Inst>, inst: Inst) -> Self {
        let mut crossed = Self::default();
        match opaque(func, wrote, inst) {
            Some(Opaque::Called) => crossed.called = true,
            Some(Opaque::Wrote) => crossed.wrote = true,
            Some(Opaque::Everything) => crossed.everything = true,
            None => match func[inst].opcode {
                Opcode::MetaBegin => {
                    crossed.unwritten = true;
                    crossed.retyped = Retyped::Anyhow;
                }
                Opcode::MetaInitCopy => crossed.unwritten = true,
                Opcode::MetaTypeCopy => crossed.retyped = Retyped::Anyhow,
                Opcode::MetaType => {
                    crossed.retyped = match func[inst].extra {
                        Extra::Node(node) => Retyped::Only(node),
                        _ => Retyped::Anyhow,
                    };
                }
                _ => {}
            },
        }
        crossed
    }
}

/// Each block's [`Crossed`], for the blocks that do anything to the facts at all.
struct Kills(HashMap<Block, Crossed>);

impl Kills {
    /// Read off every block once, before the walk, so the question at each block is a lookup.
    fn of(func: &Func, wrote: &HashSet<Inst>) -> Self {
        let mut kills = HashMap::new();
        for block in func.blocks() {
            let crossed = func
                .insts(block)
                .map(|inst| Crossed::of(func, wrote, inst))
                .fold(Crossed::default(), Crossed::and);
            if crossed != Crossed::default() {
                kills.insert(block, crossed);
            }
        }
        Self(kills)
    }

    /// What the blocks on some path from `above`, its immediate dominator, to `block` may have
    /// done, not counting `above` itself, whose instructions the walk has already been through.
    ///
    /// Those blocks are the ones that reach `block` without going through `above`, which is a walk
    /// backwards from its predecessors that stops at `above`. Every block that walk finds is one a
    /// path from `above` goes through, because a path from the entry to it that missed `above`
    /// would go on to `block` and `above` would not dominate `block`. `block` is among them when a
    /// loop comes back to it without going through `above`, and then what it does itself counts
    /// too, since on the second time round its start comes after its end.
    fn between(&self, graph: &Cfg, above: Block, block: Block) -> Crossed {
        if self.0.is_empty() || graph.predecessors(block) == [above] {
            return Crossed::default();
        }
        let mut crossed = Crossed::default();
        let mut seen: HashSet<Block> = HashSet::new();
        let mut work: Vec<Block> = graph.predecessors(block).to_vec();
        while let Some(at) = work.pop() {
            if at == above || !seen.insert(at) {
                continue;
            }
            if let Some(&kill) = self.0.get(&at) {
                crossed = crossed.and(kill);
                if crossed.everything {
                    break;
                }
            }
            work.extend_from_slice(graph.predecessors(at));
        }
        crossed
    }
}

/// What an instruction the pass cannot see through does to the facts the walk is carrying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Opaque {
    /// A call that might free. The lifetime facts go and the bounds facts stay, marked.
    Called,
    /// A call that frees nothing and may write. The plane facts go and the rest stay.
    Wrote,
    /// Everything else, which gives up both halves.
    Everything,
}

/// Whether this instruction could do something to memory that this pass cannot account for, and
/// what that means for what the walk is carrying.
///
/// A call is most of it, in every spelling, and inline assembly with it. A `tail_call` ends the
/// block and there is nothing after it to protect, and it is here anyway so that the reason a fact
/// survives is never that the walk did not think of something.
///
/// A call carrying [`Flags::NOFREE`] reaches nothing that ends a lifetime, so there is nothing for
/// it to have done to the bytes an earlier check was passed on. `crate::nofree` is what put the
/// flag there and what argues for it.
///
/// A `meta_end` and a `meta_transfer` end a lifetime by saying so, which is the plainest way for a
/// fact to stop being true, and neither is emitted today. Inline assembly is with them rather than
/// with the calls, because the argument for keeping the bounds half rests on the lifetime check at
/// the access reading a plane the runtime wrote, and a block of assembly is the one thing in the
/// IR that can write over a plane without the runtime having been asked.
fn opaque(func: &Func, wrote: &HashSet<Inst>, inst: Inst) -> Option<Opaque> {
    match func[inst].opcode {
        Opcode::Call | Opcode::CallIndirect | Opcode::TailCall => {
            if !func[inst].flags.contains(Flags::NOFREE) {
                return Some(Opaque::Called);
            }
            wrote.contains(&inst).then_some(Opaque::Wrote)
        }
        Opcode::InlineAsm | Opcode::MetaEnd | Opcode::MetaTransfer => Some(Opaque::Everything),
        _ => None,
    }
}

/// The calls marked [`Flags::NOFREE`] that may still write memory, which is every one of them
/// `crate::purity` has not said otherwise about. Worked out before the walk because the answer is
/// in the analyses and the walk needs them borrowed for other things.
fn writers(func: &Func, purity: &Facts) -> HashSet<Inst> {
    func.blocks()
        .flat_map(|block| func.insts(block))
        .filter(|&inst| {
            matches!(func[inst].opcode, Opcode::Call | Opcode::CallIndirect | Opcode::TailCall)
                && func[inst].flags.contains(Flags::NOFREE)
                && Callee::of(func, inst)
                    .is_none_or(|callee| purity.purity_of(callee).writes_memory())
        })
        .collect()
}

/// What a `check_bounds` is about, when it is one this pass can read.
pub(crate) fn about(func: &Func, check: Inst) -> Option<Fact> {
    let (base, offset, whole) = addressed(func, check)?;
    let Extra::Mem(info) = func[check].extra else { return None };
    hull(base, offset, i128::from(func[info].size), whole)
}

/// What a `check_type` is about, when it is one this pass can read: which plane entry it asks with
/// and which bytes it asks about.
///
/// The entry comes out of the access payload's `tbaa` field, where `rucc_safety::ask` puts it after
/// translating the aliasing node the front end named into the plane's vocabulary. So it is a plane
/// entry rather than a node in the aliasing tree, and two checks carrying the same one are asking
/// the same question.
fn holding(func: &Func, check: Inst) -> Option<(Meta, Fact)> {
    let Extra::Mem(info) = func[check].extra else { return None };
    let node = func[info].tbaa?;
    Some((node, about(func, check)?))
}

/// What a `check_live` is about, when it is one this pass can read.
///
/// One byte, because that is the whole of what the check says: the instance holding this address
/// is alive, and nothing about the address next door. The widening to a range that makes the fact
/// useful is `widened`, and it needs a bounds fact to do it.
pub(crate) fn alive(func: &Func, check: Inst) -> Option<Fact> {
    let (base, offset, whole) = addressed(func, check)?;
    hull(base, offset, 1, whole)
}

/// The address a check is about, as a base and a constant, and whether the capability names the
/// base rather than the address.
///
/// The capability has to be one this pass can tie to the address, which [`its_own`] is, so a check
/// that does not have it is not a check this pass has anything to say about.
fn addressed(func: &Func, check: Inst) -> Option<(Value, i128, bool)> {
    let args = &func[func[check].args];
    let &capability = args.first()?;
    let &pointer = args.get(1)?;
    let (base, offset) = normal(func, pointer);
    let named = named_by(func, capability)?;
    its_own(named, pointer, base).map(|whole| (base, offset, whole))
}

/// Whether a check's capability is about the address the check names or about the pointer that
/// address was worked out from, and which of the two it is.
///
/// Both are shapes `rucc-safety` emits. The first is what it used to emit everywhere, a `cap_of` in
/// front of each check naming the check's own pointer, and the second is what
/// `rucc_safety::origin` emits now, one capability taken where the object came from and shared by
/// every address walked off it. A check naming anything else is about some other instance and
/// nothing here is entitled to read it.
/// Whether a check this pass could not read names a capability the address really did come off.
///
/// Asked only where [`about`] has already answered nothing, so it is never on the path of a check
/// that goes, and it exists to tell one kind of miss from the rest. `rucc_safety::origin` shares one
/// capability down a whole derivation chain and [`normal`] stops walking at the first step it cannot
/// read, so a chain with a step like `i * 4` in it leaves the capability naming something further
/// back than the base and [`its_own`] refuses it. That is a miss something could be done about. A
/// capability naming a pointer the address was never walked off is a different instance and there is
/// nothing to do about one, so it stays in the row it was already in.
///
/// The walk here goes through a step of any kind, which is what makes it a different walk from
/// [`normal`], and it reads nothing but the chain, so it says the two are related and not by how
/// much.
fn midway(func: &Func, check: Inst) -> bool {
    let args = &func[func[check].args];
    let (Some(&capability), Some(&pointer)) = (args.first(), args.get(1)) else { return false };
    let Some(named) = named_by(func, capability) else { return false };
    let (base, _) = normal(func, pointer);
    let mut value = base;
    loop {
        if value == named {
            return true;
        }
        let Def::Result { inst, .. } = func[value].def else { return false };
        if func[inst].opcode != Opcode::PtrAdd {
            return false;
        }
        let Some(&from) = func[func[inst].args].first() else { return false };
        value = from;
    }
}

/// Every address a check [`its_own`] refused can land in, when its capability names the far end.
///
/// The piece [`about`] cannot supply. That one reads an address as a base and a constant, and a
/// chain with a step nobody can read has no such reading, so it answers nothing and the rules never
/// run. [`spanned`] has no such trouble, because a step nobody can read is exactly what it asks the
/// ranges about, and it walks the whole chain rather than stopping at the first one. So the walk is
/// carried on from where the constant reader gave up, and what comes back is a range of addresses
/// off a base further back.
///
/// The base it lands on has to be the value the capability names, and that is the whole of what
/// makes this sound rather than a widening. A fact answers a range by [`reaches`], which already
/// insists on the same base, so what a rule says yes to is that every address this walk can reach is
/// inside something already known about that base. The capability naming that base is what says the
/// instance the rules are talking about is the instance this check is about.
/// Which of the three midway rows a check that got this far belongs in.
///
/// Told apart by whether anything in the pass has an extent for the base, rather than by whether a
/// rule said yes, because a rule saying no covers both "I have never heard of this object" and "I
/// have heard of it and the walk leaves it" and those are completely different pieces of work. The
/// first is the great majority and it is not fixable by anything in this pass.
fn why_midway(func: &Func, known: &Known, wide: Option<Reach>, live: bool) -> &'static str {
    let Some(wide) = wide else {
        return if live { MIDWAY_CAPABILITY_LIVE } else { MIDWAY_CAPABILITY };
    };
    let heard =
        declared(func, wide.base).is_some() || known.held.iter().any(|fact| fact.base == wide.base);
    match (heard, live) {
        (false, false) => MIDWAY_NO_EXTENT,
        (false, true) => MIDWAY_NO_EXTENT_LIVE,
        (true, false) => MIDWAY_OVER,
        (true, true) => MIDWAY_OVER_LIVE,
    }
}

fn beyond(func: &Func, ranges: Option<&mut Ranges<'_>>, check: Inst, size: i128) -> Option<Reach> {
    let args = &func[func[check].args];
    let &capability = args.first()?;
    let &pointer = args.get(1)?;
    let named = named_by(func, capability)?;
    let (base, offset) = normal(func, pointer);
    let wide = spanned(func, ranges?, base, offset, size, check, Some(named))?;
    (wide.base == named).then_some(wide)
}

fn its_own(named: Value, pointer: Value, base: Value) -> Option<bool> {
    if named == pointer {
        return Some(false);
    }
    (named == base).then_some(true)
}

/// The bytes a check says belong to one instance.
///
/// Which is the access and nothing else when the capability was taken at the address, and the
/// access together with everything between it and the base when the capability was taken at the
/// base. The second is not a widening this pass made up. A capability names the instance its own
/// pointer is in, so the base is in that instance by the meaning of the operand, the access is in
/// it because that is what the check asks, and an instance is a run of bytes, so everything between
/// the two is in it as well.
///
/// That is what makes reading the second shape sound, and it has to be the fact rather than a note
/// on the side, because a fact is the thing both the asking and the recording go through. Asking
/// with it means whatever answers holds the base too, so the instance the answer is about is the
/// instance the capability names. Recording it after a check that stays is recording what the check
/// proves, and it is more than the narrow one, which is the whole reason a capability taken at the
/// base is worth having here.
fn hull(base: Value, offset: i128, size: i128, whole: bool) -> Option<Fact> {
    if !whole {
        return Some(Fact { base, offset, size });
    }
    let low = offset.min(0);
    let high = offset.checked_add(size)?.max(1);
    Some(Fact { base, offset: low, size: high.checked_sub(low)? })
}

/// The two ends of a `check_deriv`, each as the single byte at it.
///
/// A derivation check asks whether the pointer that came out of a `ptr_add` is still in the storage
/// instance the pointer that went in belongs to, so both ends have to be readable and both have to
/// come out of the same value, which is what makes the two offsets comparable at all. One byte each
/// because that is what is being asked about: not a range, but whether an address is in an instance.
///
/// The capability has to be about the pointer that went in, for the reason [`addressed`] gives. The
/// instance the check is about is the one that pointer belongs to, and a check naming some other
/// capability is about some other instance.
///
/// The width operand is not read. It matters to the runtime only for a pointer that walked off the
/// near end, where the check passes on the byte a stride further along instead of on the address
/// itself, and this pass never gets that far: it discharges nothing it has not put inside a range
/// outright.
pub(crate) fn derives(func: &Func, check: Inst) -> Option<(Fact, Fact)> {
    let args = &func[func[check].args];
    let &capability = args.first()?;
    let &from = args.get(1)?;
    let &to = args.get(2)?;
    let (base, start) = normal(func, from);
    let named = named_by(func, capability)?;
    let whole = its_own(named, from, base)?;
    let (walked, end) = normal(func, to);
    if base != walked {
        return None;
    }
    // One fact has to hold both ends, so widening the near one to reach the base is what carries
    // the base into whatever answers, which is what [`hull`] is for. The far end is left as it is,
    // since the one fact that holds the pair holds it.
    Some((hull(base, start, 1, whole)?, Fact { base, offset: end, size: 1 }))
}

/// The object a local is, when the address a check is about was computed from one.
///
/// This is the fact nobody had to check for, and section 7.2 puts it first of the four sources
/// because it is where most of the win is. An `alloca` of a fixed size is one storage instance of
/// that many bytes, said by the instruction that makes it rather than by a check that passed, so
/// the bytes from its address to that many further along are inside one instance for the same
/// reason a passing `check_bounds` says its own range is.
///
/// Only the fixed size form. The one that takes an operand is a variable length array, and how
/// many bytes it is is a value the program works out rather than a number in the payload, where
/// the field reads zero.
///
/// The fact holds everywhere in the function and no call takes it away, which is the other half of
/// what makes it worth having. A callee cannot free a frame slot: what it could free is whatever a
/// pointer stored in the slot points at, and that is a different instance and a different check.
/// So this is asked separately from the facts the walk carries rather than pushed into them, since
/// everything in there is thrown away at the first call this pass cannot see through.
fn declared(func: &Func, base: Value) -> Option<Fact> {
    let Def::Result { inst, .. } = func[base].def else { return None };
    if func[inst].opcode != Opcode::Alloca || !func[func[inst].args].is_empty() {
        return None;
    }
    let Extra::Mem(info) = func[inst].extra else { return None };
    Some(Fact::whole(base, i128::from(func[info].size)))
}

/// The alignment an allocator promises, in bytes.
///
/// C says storage an allocator hands back is aligned for any object with a fundamental alignment,
/// which is sixteen bytes on the targets this compiles for. Eight is claimed rather than sixteen
/// because the claim has to hold wherever this pass runs and the pass is given a function rather
/// than a target. What it costs is an access that assumes more than eight bytes, which is a
/// `long double` or a vector, keeping a check it could have lost.
const ALLOCATED: u64 = 8;

/// How far into an expression [`divides`] reads before it gives up.
///
/// A subscript is a multiply and a constant and the answer is two steps in. The bound is here
/// because the walk is over an expression the program wrote and nothing about an expression stops
/// it from being as deep as the source file is long.
const DEEP: u32 = 4;

/// Whether the address a check is about starts where the access assumes it does.
///
/// The alignment conjunct of judgement J1 rides on `check_bounds`, which document 06 section 6.3
/// settled, so a check that goes takes the test of it with it and something here has to have
/// answered it first. An access that assumes nothing about where it starts has nothing to answer,
/// and that is what an alignment of one is and what a member of a packed record gets.
///
/// What answers it is the object the address was computed from and the steps taken from it, which
/// is the same ground the bounds question walks. An `alloca` says what it is aligned to and an
/// allocator promises [`ALLOCATED`], and each step from there leaves whatever the step itself
/// divides by. So `p[i]` on an `int *` out of `malloc` is answered by the four in the subscript's
/// own multiply, and `(int *)(p + 1)` is not answered at all, which is row S7 and the whole reason
/// this is here.
///
/// A global is not read here at all. It arrives as [`Flags::ALIGNED`] from `crate::extents`, which
/// is given the module this is not, and the flag is the whole of what this asks about one.
///
/// A pointer whose origin this cannot read is answered by a check that already ran on it, which is
/// [`Scope::proved`], or by `!aligned(a)` written on the value, which is the side table
/// `crate::params` fills from the call sites. Between them those are the only things that answer a
/// block parameter, a pointer loaded out of memory or one handed in. What is left after that is
/// zero, which answers nothing and keeps the check, and [`UNKNOWN_ALIGNMENT`] counts them.
///
/// The two answers given before [`settles`] is asked are not arithmetic and so are not a rule's.
/// An access of one byte assumes nothing about where it starts, so there is nothing to prove about
/// it, and the flag is a fact `crate::extents` established over the whole module and wrote down.
///
/// The two ways of saying no are told apart because they are worth different things to a reader.
/// [`Alignment::Unknown`] is a value nothing here says anything about and a better fact could take.
/// [`Alignment::Lost`] is an address this followed all the way back to an object it knows the
/// alignment of, where the steps taken from it landed somewhere the access may not start, and no
/// fact answers one of those because a check that stays is what the conjunct is for.
fn aligned(func: &Func, cfg: Option<&Cfg>, aligns: &HashMap<Value, u64>, check: Inst) -> Alignment {
    let Extra::Mem(info) = func[check].extra else { return Alignment::Unknown };
    let claim = u64::from(func[info].align);
    if claim <= 1 || func[check].flags.contains(Flags::ALIGNED) {
        return Alignment::Answered;
    }
    let Some(&pointer) = func[func[check].args].get(1) else { return Alignment::Unknown };
    let known = settled(func, cfg, aligns, pointer);
    if settles(known, claim) {
        Alignment::Answered
    } else if known == 0 {
        Alignment::Unknown
    } else {
        Alignment::Lost
    }
}

/// What [`aligned`] found out about where an access starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Alignment {
    /// The address starts where the access assumes, so the check may go as far as this conjunct is
    /// concerned.
    Answered,
    /// Nothing here says where the address starts.
    Unknown,
    /// This knows where the address started and knows the steps took it off, which is row S7.
    Lost,
}

/// Whether an address known to be a multiple of one number meets an access's claim.
///
/// The companion to [`covers`] for the alignment conjunct, and it decides nothing either. The walk
/// in [`settled`] worked out a number the address divides by, and whether that answers the access
/// is the rule file's to say. The address is opaque in the question, because nothing here knows
/// what it is and the answer is about every address the walk's number holds of.
///
/// It is worth saying what the rule catches that the comparison it replaced did not. `known` being
/// the larger number is not `claim` dividing it, and the two agree only because both are powers of
/// two. Every number that gets here is one, for the reason the head in `safety.model` writes out,
/// and now that reason is written somewhere a solver reads rather than only somewhere a person
/// does.
fn settles(known: u64, claim: u64) -> bool {
    let mut question = Question::default();
    let at = question.opaque();
    let at = question.app("value.i64", &[at]);
    let known = question.number(i128::from(known));
    let known = question.app("iconst.i64", &[known]);
    let claim = question.number(i128::from(claim));
    let claim = question.app("iconst.i64", &[claim]);
    let term = question.app("aligned.i64", &[at, known, claim]);
    match safety::TABLE.find(&question, term) {
        Some(found) => yes(&safety::TABLE, found.rule),
        None => false,
    }
}

/// What a pointer is known to be aligned to, in bytes, or zero when nothing here says.
///
/// Every number involved is a power of two, so the greatest common divisor of two of them is the
/// smaller, which is why the steps are gathered with a `min` and why they start at the largest
/// number there is instead of at zero. Zero is the answer and not a step, since an alignment of
/// zero is not something an access can assume and a claim is never met by one.
///
/// `crate::params` calls this on an argument at a call site, with no graph and with `aligns`
/// carrying what the round before worked out about the caller's own parameters. Sharing the walk
/// is the point of doing it that way: what a fact says about a parameter is then exactly what the
/// callee would have worked out for itself if the value had not crossed a boundary.
pub(crate) fn settled(
    func: &Func,
    cfg: Option<&Cfg>,
    aligns: &HashMap<Value, u64>,
    pointer: Value,
) -> u64 {
    let mut budget = JOINS;
    joined(func, cfg, aligns, pointer, &mut Vec::new(), &mut budget)
}

/// How many block parameters [`settled`] will walk through before answering nothing.
///
/// The cut in [`joined`] keeps the walk from going round for ever, and this keeps it from going
/// wide for ever. A chain of joins each of which has two predecessors is a walk that doubles at
/// every step, and a function with forty of those in a row is not a function worth an answer.
const JOINS: u32 = 256;

/// [`settled`] with the two things a walk through a join needs, the values it is already inside of
/// and what is left of its budget.
///
/// A block parameter holds whatever its predecessors hand in, so it is aligned to at least the
/// least of what those are aligned to. That is arithmetic over values this function already has
/// and it needs nothing written on the function, which is why it is here rather than in the side
/// table tamnd/rucc#1385 is otherwise about. The side table is read at the top of the loop, beside
/// the answers the checks in this function gave.
///
/// The care it needs is a parameter that reaches itself round a loop, and the cut is that a value
/// the walk is already inside of contributes only the steps taken to get back to it. That lands on
/// the fixpoint rather than above it, which is worth the sentence because getting it wrong here
/// removes a check that should stay. Every step along a path is folded in as a divisor by the
/// `min` below, going round the loop a second time applies those same divisors again, and a
/// minimum does not move when you take it twice. So the answer after one lap is the answer after
/// any number of them, and there is nothing a second lap could lower that the first did not.
fn joined(
    func: &Func,
    cfg: Option<&Cfg>,
    aligns: &HashMap<Value, u64>,
    pointer: Value,
    inside: &mut Vec<Value>,
    budget: &mut u32,
) -> u64 {
    let mut steps = u64::MAX;
    let mut value = pointer;
    loop {
        // Asked in front of the shape, because the point of both is the values the shape gives up
        // on: a pointer handed in, a pointer read out of a field, a block parameter. A check that
        // ran on one of those is as good an answer as an `alloca`, and `!aligned(a)` is the same
        // answer about the same value worked out from outside the function, which is what
        // `crate::params` writes and what section 6.2.4 of
        // `spec/safe-memory/06-instrumentation.md` has the fact for.
        //
        // Whichever of the two says more is the one to take. A fact is a promise and never a
        // denial, so two promises about one value are both true and the larger is no less true
        // than the smaller.
        let proved = aligns.get(&value).copied();
        let written = func.facts(value).align.map(u64::from);
        if let Some(known) = proved.max(written) {
            return steps.min(known);
        }
        let inst = match func[value].def {
            Def::Result { inst, .. } => inst,
            // A function's parameter with no fact on it. Nothing inside the function says anything
            // about one, so this is where the row goes that the side table above is for, and it is
            // still the larger half of it.
            Def::Param { block, index } => {
                let Some(cfg) = cfg else { return 0 };
                if func.entry() == Some(block) {
                    return 0;
                }
                // The cut, and the only place a walk answers with the steps alone. Everywhere
                // else running out of things to look at is nothing known, which is zero.
                if inside.contains(&value) {
                    return steps;
                }
                if *budget == 0 {
                    return 0;
                }
                *budget -= 1;
                inside.push(value);
                let least = handed(func, cfg, aligns, block, index, inside, budget);
                inside.pop();
                return steps.min(least);
            }
        };
        match func[inst].opcode {
            Opcode::Alloca => {
                let Extra::Mem(info) = func[inst].extra else { return 0 };
                return steps.min(u64::from(func[info].align));
            }
            Opcode::Call if func[inst].flags.contains(Flags::HEAP) => {
                return steps.min(ALLOCATED);
            }
            Opcode::PtrAdd => {
                let args = &func[func[inst].args];
                let (Some(&from), Some(&by)) = (args.first(), args.get(1)) else { return 0 };
                steps = steps.min(divides(func, by, DEEP));
                value = from;
            }
            _ => return 0,
        }
    }
}

/// The least alignment any predecessor hands to one parameter of a block.
///
/// Zero for a block nothing reaches and for an edge whose arguments do not run that far, since
/// either is a function this does not understand and claiming an alignment for one would be
/// claiming it out of nothing. A predecessor whose terminator is missing is the same case.
fn handed(
    func: &Func,
    cfg: &Cfg,
    aligns: &HashMap<Value, u64>,
    block: Block,
    index: u32,
    inside: &mut Vec<Value>,
    budget: &mut u32,
) -> u64 {
    let preds = cfg.predecessors(block);
    if preds.is_empty() {
        return 0;
    }
    let mut least = u64::MAX;
    for &pred in preds {
        let Some(term) = func.terminator(pred) else { return 0 };
        let Some(&came) = copy::edge_args(func, term, block).get(index as usize) else {
            return 0;
        };
        least = least.min(joined(func, Some(cfg), aligns, came, inside, budget));
        if least == 0 {
            break;
        }
    }
    least
}

/// The largest power of two that divides a step, or one when nothing here says.
///
/// One is the answer for anything unreadable and it is the right one: every number divides by one,
/// so a step nobody can read leaves a pointer aligned to a byte and no more. Zero divides by
/// everything, which is a walk that took no step and has to leave what it started with alone.
fn divides(func: &Func, step: Value, depth: u32) -> u64 {
    if let Some(number) = constant(func, step) {
        let Ok(size) = u64::try_from(number.unsigned_abs()) else { return 1 };
        return if size == 0 { u64::MAX } else { 1 << size.trailing_zeros() };
    }
    let Def::Result { inst, .. } = func[step].def else { return 1 };
    let args = &func[func[inst].args];
    let (Some(&left), Some(&right)) = (args.first(), args.get(1)) else { return 1 };
    if depth == 0 {
        return 1;
    }
    match func[inst].opcode {
        // A subscript, which is an index nobody knows anything about times the element size.
        Opcode::Mul => {
            divides(func, left, depth - 1).saturating_mul(divides(func, right, depth - 1))
        }
        Opcode::Shl => match constant(func, right) {
            Some(by) if (0..64).contains(&by) => {
                divides(func, left, depth - 1).checked_shl(by as u32).unwrap_or(u64::MAX)
            }
            _ => 1,
        },
        // Two numbers added divide by whatever they both divide by, which is a field offset added
        // to a subscript and is how a member of an array of records comes out.
        Opcode::Add | Opcode::Sub => {
            divides(func, left, depth - 1).min(divides(func, right, depth - 1))
        }
        _ => 1,
    }
}

/// The object an allocator made, when the address a check is about was computed from one and this
/// function has already found out it is not null.
///
/// The same shape as [`declared`] one storey up, with a marked call saying the size instead of an
/// `alloca` and one more thing to establish. `crate::heap` has the argument for both halves: what a
/// call to `malloc` says is an extent and never a lifetime, and it only says it where the program
/// has looked, because a null pointer is inside no object and a check on one is a check that is
/// meant to fail.
///
/// Nothing is claimed when the graph was not built, which is a function this found no allocation in
/// and so a function where the answer would have been no anyway.
fn allocation(
    func: &Func,
    cfg: Option<&Cfg>,
    checked: &mut HashMap<Value, HashSet<Block>>,
    block: Block,
    base: Value,
) -> Option<Fact> {
    let whole = heap::made(func, base)?;
    let cfg = cfg?;
    checked
        .entry(whole.base)
        .or_insert_with(|| heap::tested(func, cfg, whole.base))
        .contains(&block)
        .then_some(whole)
}

/// Whether all of those bytes are inside one object an allocator made.
///
/// Every part has to be inside, and inside the same object, which is what asking [`covers`] with one
/// fact and several does.
fn allocated(
    func: &Func,
    cfg: Option<&Cfg>,
    checked: &mut HashMap<Value, HashSet<Block>>,
    block: Block,
    parts: &[&Fact],
) -> bool {
    let Some(first) = parts.first() else { return false };
    let Some(whole) = allocation(func, cfg, checked, block, first.base) else { return false };
    parts.iter().all(|part| covers(&whole, part))
}

/// Whether every address a walk can reach is inside one object an allocator made.
///
/// [`allocated`] for the question [`reach`] and [`spread`] ask. The object comes from the same place
/// and is believed for the same reason, and what is asked of it is [`reaches`] rather than
/// [`covers`], so a walk by a step the ranges put numbers on can be answered by a call that says how
/// many bytes it made.
///
/// The wide path used to ask a local and the facts the walk carries and nothing else, so a program
/// that walked into its own `malloc` by an index kept its checks however plainly the size was
/// written. That is the first half of tamnd/rucc#880.
fn allocated_around(
    func: &Func,
    cfg: Option<&Cfg>,
    checked: &mut HashMap<Value, HashSet<Block>>,
    block: Block,
    spans: &[&Reach],
) -> bool {
    let Some(first) = spans.first() else { return false };
    let Some(whole) = allocation(func, cfg, checked, block, first.base) else { return false };
    spans.iter().all(|span| reaches(&whole, span))
}

/// A lifetime fact grown from one address to the checked range it sits in.
///
/// The argument is in the module comment: a `check_bounds` that passed put its whole range inside
/// one instance, so the instance this lifetime check found alive is the instance that range is in.
/// With no range around the address the fact stays as it came, which is correct and answers only a
/// repeat of the very same check.
///
/// A local is asked about first, because the object it is is the widest range there can be for an
/// address computed from it and a wider fact answers more later checks. What that gives is a
/// lifetime check anywhere in a local discharging every later one in the same local, up to the
/// first call, which is the shape a function that reads several fields of a local struct has.
///
/// Only the bounds facts no call has run over, which is the third of the three things the module
/// comment's section on keeping the bounds half says have to hold. A range that was inside one
/// instance before a `free` and an allocation of something smaller at the same address is not
/// inside one instance after it, so widening a lifetime check that passed on the new instance by
/// that range would claim the whole of the old one is alive. The fact is still good enough to
/// answer a bounds check, because the lifetime check beside that access refuses the case, and it is
/// not good enough to be the reason a lifetime check goes away.
fn widened(func: &Func, bounds: &Known, asked: Fact) -> Fact {
    if let Some(local) = declared(func, asked.base).filter(|local| covers(local, &asked)) {
        return local;
    }
    bounds.fresh().iter().find(|fact| covers(fact, &asked)).copied().unwrap_or(asked)
}

/// The value an address was computed from, and how far past it the address is.
///
/// A `ptr_add` over a constant is walked through, and anything else is where the answer stops. The
/// arithmetic here is exact because it is done in `i128` over offsets that came out of the IR as
/// sixty four bit constants, and whether it is small enough to mean anything at sixty four bits is
/// the rule's question rather than this function's.
pub(crate) fn normal(func: &Func, value: Value) -> (Value, i128) {
    let mut base = value;
    let mut offset: i128 = 0;
    while let Some((from, step)) = walked(func, base) {
        let Some(sum) = offset.checked_add(step) else { break };
        base = from;
        offset = sum;
    }
    (base, offset)
}

/// Every address a walk can reach, when a step it takes is a value rather than a constant.
///
/// This is the third of the four sources section 7.2 lists, and it is the one that needs an
/// analysis. [`normal`] stops at the first `ptr_add` whose step it cannot read, and what it hands
/// back is a fact about a base nobody knows the size of. Document 10's ranges do know something
/// about the step: an index the program has already tested, or one a loop counts, is bounded even
/// though it is not constant. So the walk carries on past the step, adding the low end of its
/// range to the offset and the width of the range to the size.
///
/// What comes out is a range of addresses the access can land in, and it is a [`Reach`] rather than
/// a [`Fact`] on purpose. Whether an object holding all of that range holds the one address the
/// access actually uses is [`reaches`], which asks a rule with the distance left opaque, so one
/// answer covers every value the step could take.
///
/// It is only ever asked with. What this returns must never be recorded as established, and the
/// one place it could be is the push in the `check_bounds` arm, which happens only where this
/// returned nothing or answered nothing. The reason is that the widened range is not what a check
/// proves. A check that runs and passes proves the address the program used was inside the object,
/// and says nothing at all about the rest of the range this function made up around it.
fn reach(func: &Func, ranges: Option<&mut Ranges<'_>>, asked: &Fact, at: Inst) -> Option<Reach> {
    let wide = spanned(func, ranges?, asked.base, asked.offset, asked.size, at, None)?;
    // Nothing was walked past, so this is the fact that came in and asking it again is work
    // somebody already did.
    (wide.base != asked.base).then_some(wide)
}

/// Which of the reasons a derivation check this pass could not read is kept for.
///
/// The census and nothing else. Whether the check goes has already been decided by the time this
/// runs, and what it answers is the question somebody reading `-fopt-info-missed` is actually
/// asking, which is what would have to be built for this pile to move.
///
/// It walks the same ground [`spread`] walks rather than being folded into it, because the two want
/// different things. [`spread`] wants an answer or nothing, and stopping at the first step it cannot
/// read is the fastest way to nothing. This wants to get as far as it can and name where it stopped,
/// so it runs only on checks that are staying and it is allowed to be the slower of the two.
///
/// The five that begin `nothing here says how big` are one refusal counted five ways. What is missing
/// in every one of them is how many bytes belong to the object, and where the pointer came from is
/// what says which piece of work would supply it: `__counted_by` and the type plane for a pointer out
/// of memory, section 7.5's summaries for one that was handed over, `crate::extents` reaching further
/// for a global, and the allocation summaries for one a call returned.
fn unreadable(func: &Func, ranges: Option<&mut Ranges<'_>>, check: Inst) -> &'static str {
    let args = &func[func[check].args];
    let (Some(&capability), Some(&from), Some(&to)) = (args.first(), args.get(1), args.get(2))
    else {
        return NO_EXTENT_OTHER;
    };
    if named_by(func, capability) != Some(from) {
        return NOT_ITS_CAPABILITY_DERIV;
    }
    // No ranges is a function with no walk in it that steps by a value, so every step here was a
    // constant, so the reader that gives up on two bases gave up on two bases.
    let Some(ranges) = ranges else { return TWO_BASES_DERIV };
    let (base, offset) = normal(func, from);
    let Some(near) = spanned(func, ranges, base, offset, 1, check, None) else {
        return NO_EXTENT_OTHER;
    };
    let (base, offset) = normal(func, to);
    let Some(far) = spanned(func, ranges, base, offset, 1, check, None) else {
        return NO_EXTENT_OTHER;
    };
    if near.base != far.base {
        return TWO_BASES_DERIV;
    }
    if declared(func, near.base).is_some() {
        return OVER_THE_LOCAL_DERIV;
    }
    match func[near.base].def {
        Def::Param { .. } => NO_EXTENT_HANDED,
        Def::Result { inst, .. } => match func[inst].opcode {
            Opcode::Load => NO_EXTENT_LOADED,
            Opcode::GlobalAddr => NO_EXTENT_GLOBAL,
            Opcode::Call | Opcode::CallIndirect => NO_EXTENT_RETURNED,
            _ => NO_EXTENT_OTHER,
        },
    }
}

/// The two ends of a derivation check, each as the range of addresses it can be at.
///
/// A derivation check asks whether the pointer that came out of a walk is still in the storage
/// instance the pointer that went in belongs to. [`derives`] answers that only when both ends
/// normalize to one base over constants, and past a step the constant reader gives up on they do
/// not, which is why this reads the check's operands again rather than taking what that worked
/// out. Each end becomes a range, and the two still have to be off one base or there is nothing
/// comparable to ask about.
///
/// One byte each, for the reason [`derives`] gives. Nothing here claims anything about how many
/// bytes are readable at either address.
///
/// The capability has to be the `cap_of` of the pointer that went in, for the reason [`addressed`]
/// gives. Only that one, where [`derives`] reads a capability taken at the base the address was
/// worked out from as well: a range this reached by walking past a step comes back off a base of
/// its own, which is not the base the capability names, so there is nothing to widen towards.
fn spread(
    func: &Func,
    ranges: Option<&mut Ranges<'_>>,
    check: Inst,
    at: Inst,
) -> Option<(Reach, Reach)> {
    let ranges = ranges?;
    let args = &func[func[check].args];
    let &capability = args.first()?;
    let &from = args.get(1)?;
    let &to = args.get(2)?;
    if named_by(func, capability) != Some(from) {
        return None;
    }
    let (base, offset) = normal(func, from);
    let near = spanned(func, ranges, base, offset, 1, at, None)?;
    let (base, offset) = normal(func, to);
    let far = spanned(func, ranges, base, offset, 1, at, None)?;
    (near.base == far.base).then_some((near, far))
}

/// Every address a walk off `base` can reach, and how many bytes it takes when it gets there.
///
/// The loop is [`normal`]'s with one more thing to try. A `ptr_add` over a constant is walked
/// through the same way, and a `ptr_add` over a value is walked through when document 10's ranges
/// put numbers on that value: the low end of the range goes on the distance and the width of it on
/// the slack. Anything else is where the walk stops.
///
/// A caller with a base in mind passes it as `stop` and the walk ends there rather than carrying on
/// past it, which matters because the pointer a capability was taken at is very often a walk off
/// something further back.
///
/// Nothing is returned when a step is a value the ranges say nothing useful about, rather than the
/// walk stopping there and handing back what it had. What it had would be a range off a `ptr_add`
/// nobody knows the size of, which answers nothing, so stopping would be a longer way of saying no.
fn spanned(
    func: &Func,
    ranges: &mut Ranges<'_>,
    base: Value,
    offset: i128,
    size: i128,
    at: Inst,
    stop: Option<Value>,
) -> Option<Reach> {
    let mut base = base;
    let mut low = offset;
    let mut width: i128 = 0;
    loop {
        // Where a caller has a base in mind, the walk is over when it gets there. Without this it
        // carries on past, because a pointer somebody took a capability at is often a walk off
        // something else, and a range off a base further back is a range about a base the caller
        // was not asking about.
        if stop == Some(base) {
            break;
        }
        // A constant step again, because past a step that needed a range there can be more of
        // them, and the frontend leaves a field offset as a constant under an array index.
        if let Some((from, step)) = walked(func, base) {
            low = low.checked_add(step)?;
            base = from;
            continue;
        }
        let Some(from) = operand_of(func, base, Opcode::PtrAdd, 0) else { break };
        let by = operand_of(func, base, Opcode::PtrAdd, 1)?;
        let (least, most) = ranges.at_inst(by, at).signed_bounds()?;
        low = low.checked_add(least)?;
        width = width.checked_add(most.checked_sub(least)?)?;
        base = from;
    }
    Some(Reach { base, low, width, size })
}

/// Whether any walk in this function steps by a value rather than a constant.
///
/// The question the ranges are built for. A function without one of these would pay for a copy of
/// the control flow graph and never ask anything of it.
/// Whether anything in this function says a lifetime is over.
///
/// Nothing emits `meta_end` today, so this is false everywhere and the frame slot rule in
/// [`Discharge::run`] is on for every function. It is written anyway, and written over the whole
/// function rather than along the walk, because the day something does emit one the cheap reading
/// is the wrong one: a lifetime that ended in one arm of a branch has ended for a check after the
/// join, and a walk down the dominator tree would not have seen it. Turning the rule off for the
/// function is the reading that stays right when that day comes, and the finer one is a job for
/// whoever makes `meta_end` appear.
fn ends_a_lifetime(func: &Func) -> bool {
    func.blocks().any(|block| func.insts(block).any(|inst| func[inst].opcode == Opcode::MetaEnd))
}

fn walks_by_a_value(func: &Func) -> bool {
    func.blocks().any(|block| {
        func.insts(block).any(|inst| {
            func[inst].opcode == Opcode::PtrAdd
                && func[func[inst].args].get(1).is_some_and(|&by| constant(func, by).is_none())
        })
    })
}

/// Whether the control flow joins a pointer anywhere, which is what the alignment walk needs it for.
///
/// A block parameter of pointer type outside the entry is a pointer that came in one way on one
/// path and another way on another, and [`joined`] answers what it is aligned to by asking the
/// predecessors. Asked rather than always building the graph because the comment above says a
/// function that wants none of it should pay for none of it, and a function without a join has
/// nothing here to ask about.
fn joins_a_pointer(func: &Func, entry: Block) -> bool {
    func.blocks().any(|block| {
        block != entry && func[block].params.iter().any(|&param| func[param].ty == Type::PTR)
    })
}

/// The pointer one `ptr_add` over a constant was computed from, and by how much.
fn walked(func: &Func, value: Value) -> Option<(Value, i128)> {
    let from = operand_of(func, value, Opcode::PtrAdd, 0)?;
    let by = operand_of(func, value, Opcode::PtrAdd, 1)?;
    Some((from, constant(func, by)?))
}

/// Operand `index` of the instruction that produced `value`, when that instruction is `opcode`.
pub(crate) fn operand_of(func: &Func, value: Value, opcode: Opcode, index: usize) -> Option<Value> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    if func[inst].opcode != opcode {
        return None;
    }
    func[func[inst].args].get(index).copied()
}

/// The pointer a capability is about, whichever producer made it.
///
/// [`Opcode::capability_names`] is the fact and this is the lookup over a value. Asked instead of
/// `operand_of(func, capability, Opcode::CapOf, 0)`, which was the same question while `cap_of` was
/// the only producer `rucc-safety` emitted and became a narrower one when tamnd/rucc#1241 started
/// emitting the cheap ones. A rule here cares which pointer a capability describes and not how the
/// capability was arrived at, so asking for the opcode by name would have meant a check through a
/// pointer read out of memory quietly stopped being dischargeable on the day that read got cheaper.
pub(crate) fn named_by(func: &Func, capability: Value) -> Option<Value> {
    let Def::Result { inst, .. } = func[capability].def else { return None };
    let at = func[inst].opcode.capability_names()?;
    func[func[inst].args].get(at).copied()
}

/// The value of an integer constant, read with its own sign.
pub(crate) fn constant(func: &Func, value: Value) -> Option<i128> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    if func[inst].opcode != Opcode::IConst {
        return None;
    }
    let Extra::Imm(imm) = func[inst].extra else { return None };
    let ty = func[value].ty;
    ty.is_int().then(|| func[imm].signed(ty))
}

/// Whether an established fact answers the check being asked about.
///
/// This function decides nothing. It puts the two together into the term the rule file is written
/// about and asks the table, which is the whole of section 7.7's split: the paragraph above worked
/// out that the two addresses are one value a constant apart, and whether that is enough is
/// somebody's proof rather than this file's opinion.
pub(crate) fn covers(fact: &Fact, asked: &Fact) -> bool {
    if fact.base != asked.base {
        return false;
    }
    let Some(delta) = asked.offset.checked_sub(fact.offset) else { return false };
    let mut question = Question::default();
    let at = question.opaque();
    let at = question.app("value.i64", &[at]);
    let span = question.number(fact.size);
    let span = question.app("iconst.i64", &[span]);
    let far = question.number(delta);
    let far = question.app("iconst.i64", &[far]);
    let reach = question.number(asked.size);
    let reach = question.app("iconst.i64", &[reach]);
    let term = question.app("covered.i64", &[at, span, far, reach]);
    match safety::TABLE.find(&question, term) {
        Some(found) => yes(&safety::TABLE, found.rule),
        None => false,
    }
}

/// Whether an object holds every address a walk can land on.
///
/// The companion to [`covers`] for the question [`reach`] asks, and it decides nothing either. It
/// puts the object and the range of addresses into the term the rule file is written about and
/// asks the table. The distance the program actually walks is opaque in the question, which is
/// what makes one answer cover every value it could take.
fn reaches(fact: &Fact, asked: &Reach) -> bool {
    if fact.base != asked.base {
        return false;
    }
    let Some(delta) = asked.low.checked_sub(fact.offset) else { return false };
    let mut question = Question::default();
    let at = question.opaque();
    let at = question.app("value.i64", &[at]);
    let span = question.number(fact.size);
    let span = question.app("iconst.i64", &[span]);
    let delta = question.number(delta);
    let delta = question.app("iconst.i64", &[delta]);
    let width = question.number(asked.width);
    let width = question.app("iconst.i64", &[width]);
    let size = question.number(asked.size);
    let size = question.app("iconst.i64", &[size]);
    let step = question.opaque();
    let step = question.app("value.i64", &[step]);
    let term = question.app("reached.i64", &[at, span, delta, width, size, step]);
    match safety::TABLE.find(&question, term) {
        Some(found) => yes(&safety::TABLE, found.rule),
        None => false,
    }
}

/// Whether the rule that fired answers yes.
///
/// A discharge rule replaces the question with a constant, and one is yes. Every rule in the file
/// answers that today, and reading it off the rule rather than assuming it is what keeps this
/// honest on the day one of them answers something else.
pub(crate) fn yes(table: &Table, rule: usize) -> bool {
    matches!(table.rules[rule].replacement, [Piece::App { .. }, Piece::Int(1)])
}

/// A term built to be asked about, and nothing else.
///
/// The rules are matched against this rather than against the function, because what is being asked
/// about is not in the function: it is what the walk worked out about two of its instructions. So
/// the subject is a small arena of exactly the term being asked, built fresh for each question and
/// thrown away with the answer.
#[derive(Debug, Default)]
pub(crate) struct Question {
    held: Vec<Held>,
}

/// One node of that term.
#[derive(Debug)]
enum Held {
    /// A number the pattern can read and a guard can be about.
    Int(i128),
    /// A head and its arguments.
    App(&'static str, Vec<usize>),
    /// Something with no structure, which is how an address the rule only names is written.
    Opaque,
}

impl Question {
    /// Adds a constant and gives back where it went.
    ///
    /// Named for what it adds rather than for what it holds, because the arena also answers
    /// [`Subject::int`] and one name for the two would read as though building a term and asking
    /// about one were the same act.
    pub(crate) fn number(&mut self, value: i128) -> usize {
        self.held.push(Held::Int(value));
        self.held.len() - 1
    }

    /// Adds an application of `head` to what is already in the arena.
    pub(crate) fn app(&mut self, head: &'static str, args: &[usize]) -> usize {
        self.held.push(Held::App(head, args.to_vec()));
        self.held.len() - 1
    }

    /// Adds something the rule can bind and cannot look inside.
    pub(crate) fn opaque(&mut self) -> usize {
        self.held.push(Held::Opaque);
        self.held.len() - 1
    }
}

impl Subject for Question {
    type Node = usize;

    fn head(&self, node: usize) -> Option<(&str, usize)> {
        match &self.held[node] {
            Held::App(head, args) => Some((head, args.len())),
            Held::Int(_) | Held::Opaque => None,
        }
    }

    fn arg(&self, node: usize, index: usize) -> usize {
        match &self.held[node] {
            Held::App(_, args) => args[index],
            // The walk only asks for an argument `head` said was there, so this is unreachable
            // rather than a case with an answer.
            Held::Int(_) | Held::Opaque => unreachable!("only an application has arguments"),
        }
    }

    fn int(&self, node: usize) -> Option<i128> {
        match self.held[node] {
            Held::Int(value) => Some(value),
            Held::App(..) | Held::Opaque => None,
        }
    }

    fn same(&self, a: usize, b: usize) -> bool {
        // Every node of a question is written once, so two places holding one thing are one place.
        a == b
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        AsmInfo, Block, BlockCallList, Builder, Extra, Facts, Flags, Func, Inst, InstData, IntPred,
        MemInfo, MemOrder, Meta, Opcode, Restrict, Signature, Type, Value,
    };

    use std::sync::Arc;

    use rucc_ir::Module;
    use rucc_target::{TargetInfo, Triple};

    use super::{DISCHARGE, Fact};
    use crate::stats::Kind;
    use crate::{Fuel, Pass, pass};

    /// A function taking a pointer, with one block, ready to have accesses put in it.
    fn blank() -> (Interner, Func, Block, Value) {
        let mut names = Interner::new();
        let name = names.intern("f");
        let mut func = Func::new(name, Signature::new().with_params(&[Type::PTR]));
        let block = func.create_block();
        let pointer = func.append_param(block, Type::PTR);
        (names, func, block, pointer)
    }

    /// Puts `cap_of` and a `check_bounds` over `size` bytes at `pointer` into a block.
    ///
    /// The same shape `rucc-safety` emits, written out here rather than reached for, because
    /// `rucc-opt` is rank 9 alongside `rucc-safety` and cannot depend on it.
    fn check(build: &mut Builder<'_>, pointer: Value, size: u64) {
        checking_at(build, pointer, pointer, size);
    }

    /// The same, with the capability taken at `from` rather than at the address being checked.
    ///
    /// What `rucc_safety::origin` writes, once a capability belongs to a pointer rather than to an
    /// access: a field read off a struct is checked through the capability the struct's pointer
    /// got, and there is one of those for the whole function rather than one per field.
    fn checking_at(build: &mut Builder<'_>, from: Value, pointer: Value, size: u64) {
        let args = build.func().push_values(&[from]);
        let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let info = MemInfo {
            size,
            align: 1,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let args = build.func().push_values(&[capability, pointer]);
        let extra = Extra::Mem(build.func().add_mem(info));
        build.inst(InstData { args, extra, ..InstData::new(Opcode::CheckBounds) }, &[]);
    }

    /// Puts `cap_of` and a `check_live` at `pointer` into a block.
    ///
    /// `rucc-safety` emits this straight after the bounds check for the same access and shares the
    /// one `cap_of` between the two. Sharing it is not what the pass reads, so the tests build a
    /// second one, which is the harder shape for it to accept.
    fn live(build: &mut Builder<'_>, pointer: Value) {
        let args = build.func().push_values(&[pointer]);
        let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let args = build.func().push_values(&[capability, pointer]);
        build.inst(InstData { args, ..InstData::new(Opcode::CheckLive) }, &[]);
    }

    /// Both checks in front of one access, in the order `rucc-safety` writes them.
    fn access(build: &mut Builder<'_>, pointer: Value, size: u64) {
        check(build, pointer, size);
        live(build, pointer);
    }

    /// Puts `cap_of` and a `check_init` over `size` bytes at `pointer` into a block.
    ///
    /// The shape `rucc_safety::began` writes in front of a read. It carries a `MemInfo` for the
    /// same reason the bounds check does, since how many bytes the access takes is the whole of
    /// what the check is about.
    fn began(build: &mut Builder<'_>, pointer: Value, size: u64) {
        let args = build.func().push_values(&[pointer]);
        let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let info = MemInfo {
            size,
            align: 1,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let args = build.func().push_values(&[capability, pointer]);
        let extra = Extra::Mem(build.func().add_mem(info));
        build.inst(InstData { args, extra, ..InstData::new(Opcode::CheckInit) }, &[]);
    }

    /// How many init checks are left in a function.
    fn inits(func: &Func) -> usize {
        func.blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<_>>())
            .filter(|&inst| func[inst].opcode == Opcode::CheckInit)
            .count()
    }

    /// Puts `cap_of` and a `check_type` over `size` bytes at `pointer` into a block, asking with
    /// plane entry `node`.
    ///
    /// The shape `rucc_safety::ask` writes in front of a read. The entry is a bare index because
    /// that is all this pass ever does with one: it compares two of them and it never looks the
    /// node up, so a number nothing in the module table answers is the same question to it.
    fn asked(build: &mut Builder<'_>, pointer: Value, size: u64, node: Meta) {
        let args = build.func().push_values(&[pointer]);
        let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let info = MemInfo {
            size,
            align: 1,
            order: MemOrder::NotAtomic,
            tbaa: Some(node),
            owns: 0,
            restrict: Restrict::NONE,
        };
        let args = build.func().push_values(&[capability, pointer]);
        let extra = Extra::Mem(build.func().add_mem(info));
        build.inst(InstData { args, extra, ..InstData::new(Opcode::CheckType) }, &[]);
    }

    /// Puts a `meta_type` over `size` bytes at `pointer` into a block, naming entry `node`.
    fn judged(build: &mut Builder<'_>, pointer: Value, size: i128, node: Meta) {
        let length = build.iconst(Type::int(64), size);
        let args = build.func().push_values(&[pointer, length]);
        let extra = Extra::Node(node);
        build.inst(InstData { args, extra, ..InstData::new(Opcode::MetaType) }, &[]);
    }

    /// How many type checks are left in a function.
    fn types(func: &Func) -> usize {
        func.blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<_>>())
            .filter(|&inst| func[inst].opcode == Opcode::CheckType)
            .count()
    }

    /// A pointer `bytes` past another one.
    fn past(build: &mut Builder<'_>, pointer: Value, bytes: i128) -> Value {
        let offset = build.iconst(Type::int(64), bytes);
        let args = build.func().push_values(&[pointer, offset]);
        build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR)
    }

    /// A pointer a number of bytes past another one, where the number is not one anybody can read.
    ///
    /// The shape an indexed access leaves behind: `p[i]` is a step by `i * 4` and `normal` stops
    /// walking at it, so the base it reaches is the stepped pointer itself rather than `p`.
    fn stepped(build: &mut Builder<'_>, pointer: Value, step: Value) -> Value {
        let args = build.func().push_values(&[pointer, step]);
        build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR)
    }

    /// A `check_live` with its capability taken at `from` rather than at the address it checks.
    fn living_at(build: &mut Builder<'_>, from: Value, pointer: Value) {
        let args = build.func().push_values(&[from]);
        let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let args = build.func().push_values(&[capability, pointer]);
        build.inst(InstData { args, ..InstData::new(Opcode::CheckLive) }, &[]);
    }

    /// Puts the flag `crate::extents` writes onto every check in a function.
    ///
    /// The pass reads what the IR says, so what a test has to build is an IR that says it. Working
    /// out which checks deserve it is `crate::extents`, is about a module rather than a function,
    /// and has its own tests.
    fn marked(func: &mut Func) {
        flagged(func, Flags::STATIC);
    }

    /// Puts that flag on every check in the function, the way an annotator before the pipeline
    /// would have.
    fn flagged(func: &mut Func, flag: Flags) {
        let insts: Vec<Inst> =
            func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
        for inst in insts {
            let check = matches!(
                func[inst].opcode,
                Opcode::CheckBounds | Opcode::CheckLive | Opcode::CheckDeriv
            );
            if check {
                func[inst].flags |= flag;
            }
        }
    }

    /// How many checks are left in a function.
    fn checks(func: &Func) -> usize {
        func.blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<_>>())
            .filter(|&inst| func[inst].opcode == Opcode::CheckBounds)
            .count()
    }

    /// How many lifetime checks are left in a function.
    fn lives(func: &Func) -> usize {
        func.blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<_>>())
            .filter(|&inst| func[inst].opcode == Opcode::CheckLive)
            .count()
    }

    fn run(func: &mut Func) -> crate::Stats {
        DISCHARGE.run(func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
    }

    /// The same, with one of the measurement's variants rather than the pass the pipeline runs.
    fn run_with(pass: &super::Discharge, func: &mut Func) -> crate::Stats {
        pass.run(func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
    }

    #[test]
    fn a_run_that_may_only_ask_an_object_leaves_what_dominance_would_have_taken() {
        // Two checks of the same bytes on a pointer that came from outside. Nothing here says how
        // big the object is, so the only thing that could answer the second one is the first one
        // having run, and a run that may not ask that has to keep both.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 4);
        check(&mut build, pointer, 4);
        build.ret(&[]);
        let stats = run_with(&super::OBJECTS, &mut func);
        assert_eq!(checks(&func), 2);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 0);
    }

    #[test]
    fn a_run_that_may_only_ask_dominance_takes_the_second_check_of_the_same_bytes() {
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 4);
        check(&mut build, pointer, 4);
        build.ret(&[]);
        let stats = run_with(&super::DOMINANCE, &mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
    }

    #[test]
    fn a_run_that_may_only_ask_dominance_leaves_a_check_inside_a_local() {
        // The other way round. One check, nothing in front of it, and the bytes are inside an
        // `alloca` whose size is written on it. Only the object can answer that one.
        let (_, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        check(&mut build, slot, 4);
        build.ret(&[]);
        let stats = run_with(&super::DOMINANCE, &mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LOCAL), 0);
        assert_eq!(
            run_with(&super::OBJECTS, &mut func).count(Kind::Optimized, super::REMOVED_LOCAL),
            1
        );
    }

    #[test]
    fn the_measurement_variants_answer_to_names_of_their_own() {
        // A run that cannot be reached by a flag is a run nobody can measure with.
        let names: Vec<&str> = [
            &DISCHARGE,
            &super::OBJECTS,
            &super::DOMINANCE,
            &super::SUMMARIES,
            &super::NARROW,
            &super::EVERY,
        ]
        .iter()
        .map(|pass| pass.name())
        .collect();
        assert_eq!(
            names,
            [
                "discharge",
                "discharge-objects",
                "discharge-dominance",
                "discharge-summaries",
                "discharge-narrow",
                "discharge-every"
            ]
        );
        for name in names {
            assert!(pass::find(name).is_some(), "`{name}` is not in the pass list");
        }
    }

    #[test]
    fn a_second_check_of_the_same_bytes_goes() {
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 4);
        check(&mut build, pointer, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
    }

    #[test]
    fn a_second_init_check_inside_the_bytes_the_first_covered_goes() {
        // The same question the bounds arm's first test asks, about the other plane. Sixteen bytes
        // were read and passed on, and four of them being read again asks nothing new: bytes that
        // have been written stay written.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        began(&mut build, pointer, 16);
        let inside = past(&mut build, pointer, 8);
        began(&mut build, inside, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(inits(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_INIT), 1);
    }

    #[test]
    fn an_init_check_past_what_the_first_covered_stays() {
        // Four bytes were read and the four after them were not, and nothing about the first four
        // says anything about the second four.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        began(&mut build, pointer, 4);
        let after = past(&mut build, pointer, 4);
        began(&mut build, after, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(inits(&func), 2);
        assert_eq!(stats.count(Kind::Missed, super::NOTHING_WROTE_IT), 2);
    }

    #[test]
    fn an_init_check_a_call_stands_between_stays_and_is_counted() {
        // The bounds facts cross a call and these do not, which the `called` comment argues for.
        // The row is here so that what the conservatism costs is a number rather than a paragraph.
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        began(&mut build, pointer, 8);
        let callee = names.intern("might_free");
        let signature = build.func().add_signature(Signature::new());
        build.call(callee, signature, &[]);
        began(&mut build, pointer, 8);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(inits(&func), 2);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL_INIT), 1);
    }

    #[test]
    fn an_init_check_a_lifetime_starting_stands_between_stays() {
        // A `meta_begin` is storage becoming fresh, and fresh storage holds nothing anybody wrote,
        // so a read that was passed on in front of one says nothing behind it.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        began(&mut build, pointer, 8);
        let size = build.iconst(Type::int(64), 8);
        let args = build.func().push_values(&[pointer, size]);
        build.inst(InstData { args, ..InstData::new(Opcode::MetaBegin) }, &[]);
        began(&mut build, pointer, 8);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(inits(&func), 2);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_INIT), 0);
    }

    #[test]
    fn an_init_check_a_copy_stands_between_stays() {
        // A `meta_init_copy` gives the destination whatever the source said about itself, and an
        // uninitialized source says the destination is uninitialized too. It is the one plane
        // write that can take initialization away.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        began(&mut build, pointer, 8);
        let from = past(&mut build, pointer, 64);
        let size = build.iconst(Type::int(64), 8);
        let args = build.func().push_values(&[pointer, from, size]);
        build.inst(InstData { args, ..InstData::new(Opcode::MetaInitCopy) }, &[]);
        began(&mut build, pointer, 8);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(inits(&func), 2);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_INIT), 0);
    }

    /// Puts the `meta_init` a store leaves behind over `size` bytes at `pointer` into a block.
    fn stored(build: &mut Builder<'_>, pointer: Value, size: i128) {
        let bytes = build.iconst(Type::int(64), size);
        let args = build.func().push_values(&[pointer, bytes]);
        build.inst(InstData { args, ..InstData::new(Opcode::MetaInit) }, &[]);
    }

    #[test]
    fn an_init_check_over_bytes_a_store_wrote_goes() {
        // Sixteen bytes were stored and four of them are read. The plane says they are written
        // because the store is what said so, and a check asking it again can only hear yes.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        stored(&mut build, pointer, 16);
        let inside = past(&mut build, pointer, 8);
        began(&mut build, inside, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(inits(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_STORED), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_INIT), 0);
    }

    #[test]
    fn an_init_check_past_what_a_store_wrote_stays() {
        // Four bytes stored and eight read. The other four are the ones the check is there for.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        stored(&mut build, pointer, 4);
        began(&mut build, pointer, 8);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(inits(&func), 1);
        assert_eq!(stats.count(Kind::Missed, super::NOTHING_WROTE_IT), 1);
    }

    #[test]
    fn an_init_check_a_store_answered_before_a_call_stays_and_is_counted() {
        // A call may free the storage and hand it back out fresh, which is what it does to a fact
        // from a check, and a fact from a store is the same claim about the same plane.
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        stored(&mut build, pointer, 8);
        let callee = names.intern("might_free");
        let signature = build.func().add_signature(Signature::new());
        build.call(callee, signature, &[]);
        began(&mut build, pointer, 8);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(inits(&func), 1);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL_INIT), 1);
    }

    #[test]
    fn an_init_check_a_store_answered_before_a_lifetime_starting_stays() {
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        stored(&mut build, pointer, 8);
        let size = build.iconst(Type::int(64), 8);
        let args = build.func().push_values(&[pointer, size]);
        build.inst(InstData { args, ..InstData::new(Opcode::MetaBegin) }, &[]);
        began(&mut build, pointer, 8);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(inits(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_STORED), 0);
    }

    #[test]
    fn a_store_of_a_width_nobody_can_read_answers_nothing() {
        // The width is a parameter, so the store may have written one byte or a thousand, and the
        // check is left to find out which.
        let (_, mut func, block, pointer) = blank();
        let width = func.append_param(block, Type::int(64));
        let mut build = Builder::new(&mut func, block);
        let args = build.func().push_values(&[pointer, width]);
        build.inst(InstData { args, ..InstData::new(Opcode::MetaInit) }, &[]);
        began(&mut build, pointer, 8);
        build.ret(&[]);
        run(&mut func);
        assert_eq!(inits(&func), 1);
    }

    #[test]
    fn a_second_type_check_inside_the_bytes_the_first_covered_goes() {
        // Sixteen bytes were read at one type and four of them are read again at the same type.
        // The plane was not written in between, so the second question has the first one's answer.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        asked(&mut build, pointer, 16, Meta::new(3));
        let inside = past(&mut build, pointer, 8);
        asked(&mut build, inside, 4, Meta::new(3));
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(types(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_TYPE), 1);
    }

    #[test]
    fn a_second_type_check_at_another_type_stays() {
        // The same bytes and a different entry, which is a different question. Bytes that agree
        // with one type are exactly the bytes a read at another type is refused for.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        asked(&mut build, pointer, 8, Meta::new(3));
        asked(&mut build, pointer, 8, Meta::new(4));
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(types(&func), 2);
        assert_eq!(stats.count(Kind::Missed, super::NOTHING_TYPED_IT), 2);
    }

    #[test]
    fn a_type_check_a_store_through_the_same_type_stands_between_goes() {
        // The judgement a store writes puts its own entry over the bytes it covered and leaves
        // every other byte where it was, so a range that agreed with that entry agrees with it
        // still, wherever the store landed. `Scope::retyped` is the argument.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        asked(&mut build, pointer, 8, Meta::new(3));
        judged(&mut build, pointer, 8, Meta::new(3));
        asked(&mut build, pointer, 8, Meta::new(3));
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(types(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_TYPE), 1);
    }

    #[test]
    fn a_type_check_a_store_through_another_type_stands_between_stays() {
        // The union member store and the untyped store section 7.4 names, which arrive here as the
        // same instruction with a different entry on it. Where it landed is what this pass does not
        // know, so every range it might have covered stops agreeing with anything.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        asked(&mut build, pointer, 8, Meta::new(3));
        let far = past(&mut build, pointer, 64);
        judged(&mut build, far, 8, Meta::new(4));
        asked(&mut build, pointer, 8, Meta::new(3));
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(types(&func), 2);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_TYPE), 0);
    }

    #[test]
    fn a_type_check_a_copy_stands_between_stays() {
        // The `memcpy` case. A `meta_type_copy` gives the destination whatever the source said, and
        // what the source said is not something this pass has any way to find out.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        asked(&mut build, pointer, 8, Meta::new(3));
        let from = past(&mut build, pointer, 64);
        let size = build.iconst(Type::int(64), 8);
        let args = build.func().push_values(&[pointer, from, size]);
        build.inst(InstData { args, ..InstData::new(Opcode::MetaTypeCopy) }, &[]);
        asked(&mut build, pointer, 8, Meta::new(3));
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(types(&func), 2);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_TYPE), 0);
    }

    #[test]
    fn a_type_check_a_call_stands_between_stays_and_is_counted() {
        // Type facts go at a call for the reason the init ones do, and the row is here so that what
        // that costs is a number.
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        asked(&mut build, pointer, 8, Meta::new(3));
        let callee = names.intern("might_free");
        let signature = build.func().add_signature(Signature::new());
        build.call(callee, signature, &[]);
        asked(&mut build, pointer, 8, Meta::new(3));
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(types(&func), 2);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL_TYPE), 1);
    }

    /// The same check over an access that assumes something about where it starts.
    ///
    /// [`check`] assumes nothing, which is the right default for the tests above it: what they are
    /// about is which bytes a check covers, and an access that assumes nothing has no alignment to
    /// answer and so reaches every rule. These are the ones about the alignment itself.
    fn assuming(build: &mut Builder<'_>, pointer: Value, size: u64, align: u32) {
        let args = build.func().push_values(&[pointer]);
        let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let info = MemInfo {
            size,
            align,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let args = build.func().push_values(&[capability, pointer]);
        let extra = Extra::Mem(build.func().add_mem(info));
        build.inst(InstData { args, extra, ..InstData::new(Opcode::CheckBounds) }, &[]);
    }

    #[test]
    fn a_check_that_ran_answers_the_alignment_of_the_next_one_through_the_same_pointer() {
        // A pointer from outside, so nothing about where it came from says what it is aligned to,
        // and two checks of the same bytes. The first stays, because nothing covers its bytes, and
        // in staying it runs and refuses if the address is not a multiple of four. So on the way
        // to the second the address is a multiple of four whatever anybody knew before, the bytes
        // are covered by the first, and the second goes. This is the shape most of an ordinary
        // library is: a function reads a field of something it was handed and then reads it again.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        assuming(&mut build, pointer, 4, 4);
        assuming(&mut build, pointer, 4, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
        assert_eq!(stats.count(Kind::Missed, super::UNKNOWN_ALIGNMENT), 0);
    }

    #[test]
    fn a_check_that_ran_answers_an_alignment_no_larger_than_the_one_it_tested() {
        // The first check assumes two bytes and the second assumes four, and two does not answer
        // four, so the second stays. Everything a check proves is what the access it stands beside
        // was allowed to assume and not a byte more.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        assuming(&mut build, pointer, 4, 2);
        assuming(&mut build, pointer, 4, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 2);
        assert_eq!(stats.count(Kind::Missed, super::LOST_ALIGNMENT), 1);
    }

    #[test]
    fn a_call_does_not_take_the_alignment_a_check_proved() {
        // What a call can do is free the storage and hand it back out smaller, which is why the
        // bounds facts are marked when one runs over them. It cannot change the number in a value,
        // and an alignment fact is about the number, so it crosses a call untouched. The bounds
        // half carries across too, marked, which is what leaves this with one check.
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        assuming(&mut build, pointer, 4, 4);
        let callee = names.intern("might_free");
        let signature = build.func().add_signature(Signature::new());
        build.call(callee, signature, &[]);
        assuming(&mut build, pointer, 4, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Missed, super::UNKNOWN_ALIGNMENT), 0);
    }

    #[test]
    fn an_alignment_a_check_proved_reaches_only_the_blocks_that_check_dominates() {
        // The alignment is proved in one arm of a branch and read in the join, so on the other
        // path nothing has tested the address at all. A fact that leaked here would take a check
        // the misaligned read needs, and dominance is the only thing holding an alignment fact.
        // The check in the entry assumes a byte, which is an access that assumes nothing about
        // where it starts, so it covers the bytes for the one in the join without saying anything
        // about its alignment and the gate is what is left deciding.
        let (_, mut func, block, pointer) = blank();
        let arm = func.create_block();
        let join = func.create_block();
        let mut build = Builder::new(&mut func, block);
        assuming(&mut build, pointer, 16, 1);
        let condition = build.iconst(Type::int(32), 1);
        build.br_if(condition, arm, &[], join, &[]);
        let mut build = Builder::new(&mut func, arm);
        assuming(&mut build, pointer, 32, 4);
        build.jump(join, &[]);
        let mut build = Builder::new(&mut func, join);
        assuming(&mut build, pointer, 4, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 3, "the one in the arm reaches further, so all three stay");
        assert_eq!(stats.count(Kind::Missed, super::UNKNOWN_ALIGNMENT), 1);
    }

    #[test]
    fn an_alignment_written_on_a_pointer_handed_in_answers_a_check_on_it() {
        // The side table half of tamnd/rucc#1385. The first check assumes a byte, so it covers the
        // bytes the second reads and says nothing about where either of them starts, and the
        // second one is left with the alignment conjunct and nothing inside the function to answer
        // it with. The fact is the answer, and it is the only one there is for a pointer handed in.
        let (_, mut func, block, pointer) = blank();
        func.set_facts(pointer, Facts { align: Some(4), ..Facts::NONE });
        let mut build = Builder::new(&mut func, block);
        assuming(&mut build, pointer, 4, 1);
        assuming(&mut build, pointer, 4, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
        assert_eq!(stats.count(Kind::Missed, super::UNKNOWN_ALIGNMENT), 0);
    }

    #[test]
    fn an_alignment_written_on_a_pointer_answers_no_more_than_it_says() {
        // The same function with the fact saying two and the access assuming four. Two does not
        // answer four, so the check stays, and it stays as an address this knows about rather than
        // as one nothing has heard of, which is the difference between the two rows.
        let (_, mut func, block, pointer) = blank();
        func.set_facts(pointer, Facts { align: Some(2), ..Facts::NONE });
        let mut build = Builder::new(&mut func, block);
        assuming(&mut build, pointer, 4, 1);
        assuming(&mut build, pointer, 4, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 2);
        assert_eq!(stats.count(Kind::Missed, super::UNKNOWN_ALIGNMENT), 0);
        assert_eq!(stats.count(Kind::Missed, super::LOST_ALIGNMENT), 1);
    }

    #[test]
    fn an_alignment_every_arm_hands_in_reaches_the_join() {
        // Both predecessors hand the join a slot, both slots are aligned to eight, so the join's
        // parameter is aligned to eight whichever way control came. Nothing is written on the
        // function and nothing is assumed from a type: the answer is the least of what the
        // predecessors actually pass, which is arithmetic over values already here.
        let (_, mut func, block, _) = blank();
        let join = func.create_block();
        let arm = func.create_block();
        let carried = func.append_param(join, Type::PTR);
        let mut build = Builder::new(&mut func, block);
        let one = local(&mut build, 64);
        let condition = build.iconst(Type::int(32), 1);
        build.br_if(condition, arm, &[], join, &[one]);
        let mut build = Builder::new(&mut func, arm);
        let two = local(&mut build, 64);
        build.jump(join, &[two]);
        let mut build = Builder::new(&mut func, join);
        assuming(&mut build, carried, 16, 1);
        assuming(&mut build, carried, 4, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(stats.count(Kind::Missed, super::UNKNOWN_ALIGNMENT), 0);
        assert_eq!(checks(&func), 1, "the one that covers the bytes stays, the aligned one goes");
    }

    #[test]
    fn an_alignment_one_arm_does_not_hand_in_does_not_reach_the_join() {
        // The same function with one arm handing in the pointer the function was given. Nothing
        // here says anything about that one, so the least over the predecessors is nothing, and a
        // walk that took the other arm's answer would be reading a fact off the arm the program
        // did not take.
        let (_, mut func, block, pointer) = blank();
        let join = func.create_block();
        let arm = func.create_block();
        let carried = func.append_param(join, Type::PTR);
        let mut build = Builder::new(&mut func, block);
        let one = local(&mut build, 64);
        let condition = build.iconst(Type::int(32), 1);
        build.br_if(condition, arm, &[], join, &[one]);
        let mut build = Builder::new(&mut func, arm);
        build.jump(join, &[pointer]);
        let mut build = Builder::new(&mut func, join);
        assuming(&mut build, carried, 16, 1);
        assuming(&mut build, carried, 4, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(stats.count(Kind::Missed, super::UNKNOWN_ALIGNMENT), 1);
        assert_eq!(checks(&func), 2);
    }

    #[test]
    fn a_pointer_that_walks_a_loop_keeps_only_what_the_step_leaves() {
        // The cut, which is the part of the join walk worth a test of its own. The header's
        // parameter comes in from the entry as a slot aligned to eight and comes round the back
        // edge as itself stepped by eight. The walk meets the parameter inside itself and takes
        // only the steps it took to get back there, which is the fixpoint rather than a guess:
        // going round again steps by eight again and eight is already the least. So four is
        // answered, and sixteen is refused by an answer rather than by nothing, which is the
        // difference between the two rows.
        let (_, mut func, block, _) = blank();
        let header = func.create_block();
        let exit = func.create_block();
        let carried = func.append_param(header, Type::PTR);
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 64);
        build.jump(header, &[slot]);
        let mut build = Builder::new(&mut func, header);
        assuming(&mut build, carried, 16, 1);
        assuming(&mut build, carried, 4, 4);
        assuming(&mut build, carried, 4, 16);
        let next = past(&mut build, carried, 8);
        let condition = build.iconst(Type::int(32), 1);
        build.br_if(condition, header, &[next], exit, &[]);
        let mut build = Builder::new(&mut func, exit);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(stats.count(Kind::Missed, super::UNKNOWN_ALIGNMENT), 0);
        assert_eq!(stats.count(Kind::Missed, super::LOST_ALIGNMENT), 1);
    }

    #[test]
    fn the_two_reasons_an_alignment_is_not_answered_are_counted_apart() {
        // One function with both in it. The pointer from outside is answered by nothing at all,
        // which is the row a fact from somewhere else could take, and the slot read a byte in is
        // answered by something that says no, which is the row no fact takes. The first check on
        // each is the one that covers the bytes for the second, since the gate is only asked once
        // a rule has answered those.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        assuming(&mut build, pointer, 16, 1);
        assuming(&mut build, pointer, 4, 4);
        let slot = local(&mut build, 16);
        let odd = past(&mut build, slot, 1);
        assuming(&mut build, odd, 4, 1);
        assuming(&mut build, odd, 4, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(stats.count(Kind::Missed, super::UNKNOWN_ALIGNMENT), 1);
        assert_eq!(stats.count(Kind::Missed, super::LOST_ALIGNMENT), 1);
    }

    #[test]
    fn a_step_off_an_alignment_a_check_proved_is_walked_the_way_a_local_is() {
        // The pointer is proved four byte aligned by a check that ran, and then the two accesses
        // are at four bytes in, which keeps it, and at one byte in, which does not. Nothing about
        // the walk changes because the thing it ends at is a check rather than an `alloca`, which
        // is the point of putting the answer where `settled` already looks.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        assuming(&mut build, pointer, 64, 4);
        let even = past(&mut build, pointer, 4);
        assuming(&mut build, even, 4, 4);
        let odd = past(&mut build, pointer, 1);
        assuming(&mut build, odd, 4, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 2, "the one four bytes in goes and the one a byte in stays");
        assert_eq!(stats.count(Kind::Missed, super::LOST_ALIGNMENT), 1);
    }

    #[test]
    fn a_check_inside_a_local_at_an_offset_the_local_is_aligned_through_goes() {
        // An eight byte aligned slot read four bytes in, which is a member of a record and the
        // commonest access there is. The offset leaves four of the eight, the access assumes four,
        // and the check goes the way it did before any of this.
        let (_, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        let field = past(&mut build, slot, 4);
        assuming(&mut build, field, 4, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LOCAL), 1);
        assert_eq!(stats.count(Kind::Missed, super::UNKNOWN_ALIGNMENT), 0);
    }

    #[test]
    fn a_check_a_cast_moved_off_the_alignment_stays_however_well_its_bytes_are_covered() {
        // Row S7 written in IR. The bytes are inside the slot and the slot is aligned, but the
        // access starts one byte in and assumes four, and one byte in is where the alignment is
        // lost. This is the check the misaligned read needs and the one the accounting run found
        // going missing.
        let (_, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        let odd = past(&mut build, slot, 1);
        assuming(&mut build, odd, 4, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LOCAL), 0);
        assert_eq!(stats.count(Kind::Missed, super::LOST_ALIGNMENT), 1);
    }

    #[test]
    fn a_subscript_that_steps_by_the_width_it_reads_settles_its_own_alignment() {
        // `p[i]` on an `int *` an allocator made. Nobody knows what the index is, and nobody has
        // to: the step is the index times four, four divides it whatever the index turns out to
        // be, and the allocation it starts from is aligned to more than that.
        let (_, mut func, inside, _, pointer, index) = allocation(64);
        let mut build = Builder::new(&mut func, inside);
        let four = build.iconst(Type::int(64), 4);
        let step = build.binary(Opcode::Mul, index, four, Flags::NONE);
        let args = build.func().push_values(&[pointer, step]);
        let at = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        assuming(&mut build, at, 4, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(stats.count(Kind::Missed, super::UNKNOWN_ALIGNMENT), 0);
    }

    #[test]
    fn what_answers_an_alignment_claim_is_the_rule_and_not_a_comparison() {
        // The four cases the rule is asked about, and the fifth is the reason it is a rule. A
        // number larger than the claim and not a multiple of it answers nothing, and the guard is
        // written so that the question never gets asked with one, because `super::settled` only
        // ever gives back a power of two. The last one is the same point from the other end: the
        // largest number there is is larger than every claim and divides nothing, and what the
        // walk means by it is that it took no step rather than that it found an alignment.
        assert!(super::settles(8, 8));
        assert!(super::settles(16, 8));
        assert!(!super::settles(4, 8));
        assert!(!super::settles(0, 8));
        assert!(!super::settles(u64::MAX, 8));
    }

    #[test]
    fn a_step_by_something_nobody_can_read_settles_nothing() {
        // A step the ranges do bound, so the bytes are answered and the check was on its way out,
        // and a step nothing says the low bits of, so where the access starts is not answered. A
        // mask of seven is nought to seven and three is one of those.
        let (_, mut func, block, _, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        let step = low_bits(&mut build, index, 7);
        let args = build.func().push_values(&[slot, step]);
        let at = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        assuming(&mut build, at, 4, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Missed, super::LOST_ALIGNMENT), 1);
    }

    #[test]
    fn a_check_over_a_length_the_program_worked_out_is_not_this_pass_to_read() {
        // Section 7.4's hoisted check covers as many bytes as its loop runs times, which is a value
        // and not a number. Every range this pass compares is a pair of numbers, so it says so and
        // leaves the check alone rather than reading the payload, whose size is one element.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 4);
        let args = build.func().push_values(&[pointer]);
        let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let bytes = build.iconst(Type::int(64), 4);
        let info = MemInfo {
            size: 4,
            align: 1,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let extra = Extra::Mem(build.func().add_mem(info));
        let args = build.func().push_values(&[capability, pointer, bytes]);
        build.inst(InstData { args, extra, ..InstData::new(Opcode::CheckBounds) }, &[]);
        build.ret(&[]);

        let stats = run(&mut func);
        assert_eq!(checks(&func), 2, "the second one stays");
        assert_eq!(stats.count(Kind::Missed, super::COMPUTED_EXTENT), 1);
    }

    #[test]
    fn a_check_of_bytes_inside_a_checked_range_goes() {
        // Four bytes at offset four, inside sixteen bytes at offset zero. This is the shape the
        // whole pass is for: a struct whose fields are read one after another through one pointer.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        let field = past(&mut build, pointer, 4);
        check(&mut build, field, 4);
        build.ret(&[]);
        run(&mut func);
        assert_eq!(checks(&func), 1);
    }

    #[test]
    fn a_check_of_bytes_past_the_end_of_a_checked_range_stays() {
        // Four bytes at offset fourteen is two bytes past the end of the sixteen that were
        // checked, and those two bytes are what the check is for.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        let over = past(&mut build, pointer, 14);
        check(&mut build, over, 4);
        build.ret(&[]);
        assert!(!run(&mut func).changed());
        assert_eq!(checks(&func), 2);
    }

    #[test]
    fn a_check_of_bytes_before_a_checked_range_stays() {
        // The guard's `delta` is not negative, and this is why. A read four bytes below what was
        // checked is a read of somebody else's memory, and it is the bug the check exists for.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        let under = past(&mut build, pointer, -4);
        check(&mut build, under, 4);
        build.ret(&[]);
        assert!(!run(&mut func).changed());
        assert_eq!(checks(&func), 2);
    }

    #[test]
    fn a_check_whose_capability_was_taken_where_the_pointer_came_from_covers_the_bytes_between() {
        // The shape a capability that belongs to a pointer produces. The check is on a field eight
        // bytes in and the capability was taken at the struct's pointer, so what it says is that
        // those four bytes and that pointer are in one instance. An instance is a run of bytes, so
        // everything from the pointer up to the end of the field is in it, and that is the fact.
        // The second check is inside it and goes.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        let field = past(&mut build, pointer, 8);
        checking_at(&mut build, pointer, field, 4);
        checking_at(&mut build, pointer, pointer, 4);
        build.ret(&[]);
        run(&mut func);
        assert_eq!(checks(&func), 1);
    }

    #[test]
    fn a_check_whose_capability_was_taken_where_the_pointer_came_from_says_nothing_past_the_end() {
        // And the run stops where the access does. Four bytes at twelve are past the twelve the
        // check above established, and nothing here says the instance reaches that far.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        let field = past(&mut build, pointer, 8);
        checking_at(&mut build, pointer, field, 4);
        let over = past(&mut build, pointer, 12);
        checking_at(&mut build, pointer, over, 4);
        build.ret(&[]);
        assert!(!run(&mut func).changed());
        assert_eq!(checks(&func), 2);
    }

    #[test]
    fn a_check_whose_capability_is_about_neither_end_of_the_walk_stays() {
        // Two rules and no third. A capability is about the address being checked or about the
        // pointer that address came off, and one about anything else is asking after an instance
        // this pass has nothing to say about. The capability here is readable and names a
        // pointer this address was never walked off, which is a different instance and nothing to
        // be done about, so it stays in the row it was in.
        let mut names = Interner::new();
        let name = names.intern("two");
        let mut func = Func::new(name, Signature::new().with_params(&[Type::PTR, Type::PTR]));
        let block = func.create_block();
        let pointer = func.append_param(block, Type::PTR);
        let other = func.append_param(block, Type::PTR);
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        let field = past(&mut build, pointer, 4);
        checking_at(&mut build, other, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 2);
        assert_eq!(stats.count(Kind::Missed, super::UNKNOWN_SHAPE), 1);
    }

    #[test]
    fn the_two_reasons_a_shape_is_not_read_are_counted_apart() {
        // One row used to hold both of these and the row said the first thing about both. The
        // first check walks off the pointer by a step nobody here can read, and its capability
        // names that pointer, so what stopped it is that the capability is about something
        // further back than the base a walk this pass can follow reaches, and since the pointer is
        // a parameter nothing here has an extent for, it lands in the row that says so. The second
        // is checked through a capability taken at an unrelated pointer, which is a different
        // instance and is not the same problem at all.
        let mut names = Interner::new();
        let name = names.intern("two");
        let params = [Type::PTR, Type::PTR, Type::int(64)];
        let mut func = Func::new(name, Signature::new().with_params(&params));
        let block = func.create_block();
        let pointer = func.append_param(block, Type::PTR);
        let other = func.append_param(block, Type::PTR);
        let step = func.append_param(block, Type::int(64));
        let mut build = Builder::new(&mut func, block);
        let far = stepped(&mut build, pointer, step);
        checking_at(&mut build, pointer, far, 4);
        checking_at(&mut build, other, pointer, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 2);
        assert_eq!(stats.count(Kind::Missed, super::MIDWAY_NO_EXTENT), 1);
        assert_eq!(stats.count(Kind::Missed, super::UNKNOWN_SHAPE), 1);
    }

    #[test]
    fn a_lifetime_check_whose_capability_is_further_back_than_its_base_is_counted_apart_too() {
        // The same split on the other half, because the two rows are close to the same size on
        // the amalgamation and a relaxation would have to serve both.
        let mut names = Interner::new();
        let name = names.intern("two");
        let params = [Type::PTR, Type::PTR, Type::int(64)];
        let mut func = Func::new(name, Signature::new().with_params(&params));
        let block = func.create_block();
        let pointer = func.append_param(block, Type::PTR);
        let other = func.append_param(block, Type::PTR);
        let step = func.append_param(block, Type::int(64));
        let mut build = Builder::new(&mut func, block);
        let far = stepped(&mut build, pointer, step);
        living_at(&mut build, pointer, far);
        living_at(&mut build, other, pointer);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(lives(&func), 2);
        assert_eq!(stats.count(Kind::Missed, super::MIDWAY_NO_EXTENT_LIVE), 1);
        assert_eq!(stats.count(Kind::Missed, super::UNKNOWN_SHAPE_LIVE), 1);
    }

    #[test]
    fn a_check_through_a_pointer_nothing_relates_to_the_first_stays() {
        let mut names = Interner::new();
        let name = names.intern("two");
        let mut func = Func::new(name, Signature::new().with_params(&[Type::PTR, Type::PTR]));
        let block = func.create_block();
        let one = func.append_param(block, Type::PTR);
        let other = func.append_param(block, Type::PTR);
        let mut build = Builder::new(&mut func, block);
        check(&mut build, one, 16);
        check(&mut build, other, 4);
        build.ret(&[]);
        assert!(!run(&mut func).changed());
        assert_eq!(checks(&func), 2);
    }

    #[test]
    fn a_bounds_check_a_call_stands_between_goes_and_its_lifetime_check_stays() {
        // The eighth box of tamnd/rucc#1241 and the module comment's section on why it is allowed.
        // The range the first check established is still one range on the far side of the call, or
        // the lifetime check at the second access is about to refuse, and that check is still here
        // to do it because the lifetime facts are still dropped.
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 16);
        let callee = names.intern("might_free");
        let signature = build.func().add_signature(Signature::new());
        build.call(callee, signature, &[]);
        access(&mut build, pointer, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1, "the bounds check crossed the call");
        assert_eq!(lives(&func), 2, "and the lifetime check did not");
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL), 0);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL_LIVE), 1);
    }

    #[test]
    fn a_bounds_check_inline_assembly_stands_between_stays_and_is_counted() {
        // The other half of the split. A call hands the planes to the runtime and a block of
        // assembly does not, so this one drops both kinds and the row that says what that costs is
        // still reachable.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        build.inst(InstData::new(Opcode::InlineAsm), &[]);
        check(&mut build, pointer, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(checks(&func), 2);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL), 1);
    }

    #[test]
    fn a_lifetime_check_is_not_widened_by_a_range_a_call_ran_over() {
        // The third of the three things the module comment says have to hold. The sixteen bytes
        // were one instance before the call and the call may have freed them and made something
        // smaller in their place, so the lifetime check at the pointer says the new instance is
        // alive and says nothing at all about the byte twelve further on. Widening by the older
        // range would discharge the second lifetime check, and the access it guards is the one
        // that would then land in storage the new instance does not own.
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        let callee = names.intern("might_free");
        let signature = build.func().add_signature(Signature::new());
        build.call(callee, signature, &[]);
        live(&mut build, pointer);
        let field = past(&mut build, pointer, 12);
        live(&mut build, field);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(lives(&func), 2, "the second one is not answered by the first");
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE), 0);
    }

    #[test]
    fn a_check_a_call_that_cannot_free_stands_between_goes() {
        // The other side of the paragraph above. The summary said this call reaches nothing that
        // ends a lifetime, so the range the first check established is still one range.
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        let callee = names.intern("counts_them");
        let signature = build.func().add_signature(Signature::new());
        let call = build.call(callee, signature, &[]);
        check(&mut build, pointer, 4);
        build.ret(&[]);
        func[call].flags |= Flags::NOFREE;
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL), 0);
    }

    /// A call to `name` flagged as reaching nothing that frees, which is what `crate::nofree` writes
    /// onto a `memcpy` or a function of the module's own that stores and never frees.
    fn freeing_nothing(build: &mut Builder<'_>, names: &mut Interner, name: &str) {
        let callee = names.intern(name);
        let signature = build.func().add_signature(Signature::new());
        let call = build.call(callee, signature, &[]);
        build.func()[call].flags |= Flags::NOFREE;
    }

    /// The analyses the pipeline hands a pass, with `crate::purity` told the module declares `name`
    /// and nothing else, so a name in its library table gets that table's answer.
    fn declaring(names: &mut Interner, name: &str) -> crate::Analyses {
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let mut module = Module::new(names.intern("t.c"), &target);
        module.add_func(Func::new(names.intern(name), Signature::new()));
        let facts = crate::purity::Facts::of_module(&module, names);
        crate::machine::fixtures::analyses().calling(Arc::new(facts))
    }

    #[test]
    fn a_plane_check_a_call_that_frees_nothing_but_writes_stands_between_stays() {
        // Freeing nothing is not writing nothing. The callee may store a `float` over the bytes
        // the first check found holding an `int`, or copy bytes nothing wrote over them, and the
        // second pair of checks is what would say so. The lifetime of the storage is untouched,
        // so the bounds check still goes.
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 8);
        asked(&mut build, pointer, 8, Meta::new(3));
        began(&mut build, pointer, 8);
        freeing_nothing(&mut build, &mut names, "retypes_it");
        check(&mut build, pointer, 8);
        asked(&mut build, pointer, 8, Meta::new(3));
        began(&mut build, pointer, 8);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(types(&func), 2);
        assert_eq!(inits(&func), 2);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL_TYPE), 1);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL_INIT), 1);
    }

    #[test]
    fn a_plane_check_a_call_that_writes_nothing_stands_between_goes() {
        // `strlen` frees nothing and writes nothing, which `crate::purity`'s library table says,
        // so neither plane can have changed and the second pair has nothing left to find.
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        asked(&mut build, pointer, 8, Meta::new(3));
        began(&mut build, pointer, 8);
        freeing_nothing(&mut build, &mut names, "strlen");
        asked(&mut build, pointer, 8, Meta::new(3));
        began(&mut build, pointer, 8);
        build.ret(&[]);
        let mut an = declaring(&mut names, "strlen");
        let stats = DISCHARGE.run(&mut func, &mut an, &mut Fuel::unlimited());
        assert_eq!(types(&func), 1);
        assert_eq!(inits(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_TYPE), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_INIT), 1);
    }

    #[test]
    fn a_lifetime_check_after_a_join_a_call_in_one_arm_stands_before_stays() {
        // The check in front of the branch dominates the one after the join, and on the path
        // through the arm a call that may free runs between them. The walk goes from the entry to
        // the join straight down the dominator tree and never through the arm, so what the arm did
        // has to reach the join some other way, or `free (p)` in one arm of an `if` is a read of
        // freed storage nothing stops.
        let (mut names, mut func, block, pointer) = blank();
        let arm = func.create_block();
        let join = func.create_block();
        let mut build = Builder::new(&mut func, block);
        live(&mut build, pointer);
        let condition = build.iconst(Type::int(32), 1);
        build.br_if(condition, arm, &[], join, &[]);
        let mut build = Builder::new(&mut func, arm);
        let callee = names.intern("might_free");
        let signature = build.func().add_signature(Signature::new());
        build.call(callee, signature, &[]);
        build.jump(join, &[]);
        let mut build = Builder::new(&mut func, join);
        live(&mut build, pointer);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(lives(&func), 2);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE), 0);
    }

    #[test]
    fn a_plane_check_after_a_join_a_write_in_one_arm_stands_before_stays() {
        // The same shape for the two plane facts, with a call that frees nothing and may write in
        // the arm. The bounds check after the join still goes, since nothing in the arm can end
        // the storage the first one was about.
        let (mut names, mut func, block, pointer) = blank();
        let arm = func.create_block();
        let join = func.create_block();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 8);
        asked(&mut build, pointer, 8, Meta::new(3));
        began(&mut build, pointer, 8);
        let condition = build.iconst(Type::int(32), 1);
        build.br_if(condition, arm, &[], join, &[]);
        let mut build = Builder::new(&mut func, arm);
        freeing_nothing(&mut build, &mut names, "retypes_it");
        build.jump(join, &[]);
        let mut build = Builder::new(&mut func, join);
        check(&mut build, pointer, 8);
        asked(&mut build, pointer, 8, Meta::new(3));
        began(&mut build, pointer, 8);
        build.ret(&[]);
        run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(types(&func), 2);
        assert_eq!(inits(&func), 2);
    }

    #[test]
    fn a_lifetime_check_at_the_top_of_a_loop_a_call_further_down_stays() {
        // The loop header's immediate dominator is the block in front of the loop, and the second
        // time round the header is reached from the bottom of the loop, after the call. The call
        // is in the header itself here, after the check, which is the case where a block's own
        // instructions come between its end and its next start.
        let (mut names, mut func, block, pointer) = blank();
        let header = func.create_block();
        let exit = func.create_block();
        let mut build = Builder::new(&mut func, block);
        live(&mut build, pointer);
        build.jump(header, &[]);
        let mut build = Builder::new(&mut func, header);
        live(&mut build, pointer);
        let callee = names.intern("might_free");
        let signature = build.func().add_signature(Signature::new());
        build.call(callee, signature, &[]);
        let condition = build.iconst(Type::int(32), 1);
        build.br_if(condition, header, &[], exit, &[]);
        let mut build = Builder::new(&mut func, exit);
        build.ret(&[]);
        run(&mut func);
        assert_eq!(lives(&func), 2);
    }

    #[test]
    fn a_check_after_a_join_whose_arms_do_nothing_still_goes() {
        // The other side: a branch whose arms touch nothing leaves the facts where they were, and
        // the check after the join is answered by the one in front of the branch as before.
        let (_, mut func, block, pointer) = blank();
        let arm = func.create_block();
        let join = func.create_block();
        let mut build = Builder::new(&mut func, block);
        live(&mut build, pointer);
        let condition = build.iconst(Type::int(32), 1);
        build.br_if(condition, arm, &[], join, &[]);
        let mut build = Builder::new(&mut func, arm);
        build.jump(join, &[]);
        let mut build = Builder::new(&mut func, join);
        live(&mut build, pointer);
        build.ret(&[]);
        run(&mut func);
        assert_eq!(lives(&func), 1);
    }

    #[test]
    fn inline_assembly_throws_the_facts_away_whatever_it_is_flagged() {
        // There is no flag that would make this safe. The template is text the compiler does not
        // read, so nothing worked anything out about what it reaches.
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        build.inline_asm(
            AsmInfo {
                template: names.intern("nop"),
                constraints: names.intern(""),
                clobbers: names.intern(""),
                targets: BlockCallList::EMPTY,
            },
            &[],
            &[],
            Flags::NONE,
        );
        check(&mut build, pointer, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL), 1);
    }

    #[test]
    fn a_check_that_only_one_path_covers_stays() {
        // The dominator tree is what makes this right. The check in the arm covers the one in the
        // join on one path and not on the other, and a check that goes has to be one that ran.
        let (_, mut func, block, pointer) = blank();
        let arm = func.create_block();
        let join = func.create_block();
        let mut build = Builder::new(&mut func, block);
        let condition = build.iconst(Type::int(32), 1);
        build.br_if(condition, arm, &[], join, &[]);
        let mut build = Builder::new(&mut func, arm);
        check(&mut build, pointer, 16);
        build.jump(join, &[]);
        let mut build = Builder::new(&mut func, join);
        check(&mut build, pointer, 4);
        build.ret(&[]);
        assert!(!run(&mut func).changed());
        assert_eq!(checks(&func), 2);
    }

    #[test]
    fn a_check_a_dominating_block_covers_goes() {
        let (_, mut func, block, pointer) = blank();
        let after = func.create_block();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        build.jump(after, &[]);
        let mut build = Builder::new(&mut func, after);
        let field = past(&mut build, pointer, 8);
        check(&mut build, field, 8);
        build.ret(&[]);
        run(&mut func);
        assert_eq!(checks(&func), 1);
    }

    #[test]
    fn fuel_stops_the_removing_and_not_the_looking() {
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 4);
        check(&mut build, pointer, 4);
        check(&mut build, pointer, 4);
        build.ret(&[]);
        let mut fuel = Fuel::of(1);
        let stats = DISCHARGE.run(&mut func, &mut crate::machine::fixtures::analyses(), &mut fuel);
        assert_eq!(checks(&func), 2);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL), 1);
    }

    #[test]
    fn a_second_lifetime_check_of_the_same_address_goes() {
        // The narrow fact on its own, with no range around it to widen into.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        live(&mut build, pointer);
        live(&mut build, pointer);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(lives(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE), 1);
    }

    #[test]
    fn a_lifetime_check_inside_a_checked_range_goes() {
        // The shape the pass is for, with both halves of it. Sixteen bytes are checked and found
        // alive, then a field four bytes in is read, and neither check in front of it survives.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 16);
        let field = past(&mut build, pointer, 4);
        access(&mut build, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(lives(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE), 1);
    }

    #[test]
    fn a_lifetime_check_outside_every_checked_range_stays() {
        // Four bytes at offset twenty are past the sixteen that were checked, so nothing says the
        // address is in the instance that was found alive, and it might be in no instance at all.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 16);
        let over = past(&mut build, pointer, 20);
        live(&mut build, over);
        build.ret(&[]);
        assert!(!run(&mut func).changed());
        assert_eq!(lives(&func), 2);
    }

    #[test]
    fn a_lifetime_check_with_no_range_around_it_does_not_widen() {
        // Without the bounds check the first lifetime check speaks only for its own address, so
        // the one four bytes along is a different question and stays.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        live(&mut build, pointer);
        let field = past(&mut build, pointer, 4);
        live(&mut build, field);
        build.ret(&[]);
        assert!(!run(&mut func).changed());
        assert_eq!(lives(&func), 2);
    }

    #[test]
    fn a_lifetime_check_a_call_stands_between_stays_and_is_counted() {
        // Section 8.8's number. This is the one the summaries were written for.
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 16);
        let callee = names.intern("might_free");
        let signature = build.func().add_signature(Signature::new());
        build.call(callee, signature, &[]);
        let field = past(&mut build, pointer, 4);
        live(&mut build, field);
        build.ret(&[]);
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(lives(&func), 2);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL_LIVE), 1);
    }

    #[test]
    fn a_lifetime_check_a_call_that_cannot_free_stands_between_goes() {
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 16);
        let callee = names.intern("counts_them");
        let signature = build.func().add_signature(Signature::new());
        let call = build.call(callee, signature, &[]);
        let field = past(&mut build, pointer, 4);
        live(&mut build, field);
        build.ret(&[]);
        func[call].flags |= Flags::NOFREE;
        let stats = run(&mut func);
        assert_eq!(lives(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE), 1);
    }

    #[test]
    fn ending_a_lifetime_throws_the_facts_away() {
        // Nothing emits `meta_end` yet, so this is the test that says what will happen when
        // something does, rather than a test of anything the compiler does today.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 16);
        let size = build.iconst(Type::int(64), 16);
        let args = build.func().push_values(&[pointer, size]);
        build.inst(InstData { args, ..InstData::new(Opcode::MetaEnd) }, &[]);
        access(&mut build, pointer, 16);
        build.ret(&[]);
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(checks(&func), 2);
        assert_eq!(lives(&func), 2);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL), 1);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL_LIVE), 1);
    }

    #[test]
    fn fuel_runs_out_over_both_kinds_of_check() {
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 16);
        access(&mut build, pointer, 4);
        build.ret(&[]);
        let mut fuel = Fuel::of(1);
        let stats = DISCHARGE.run(&mut func, &mut crate::machine::fixtures::analyses(), &mut fuel);
        assert_eq!(checks(&func), 1);
        assert_eq!(lives(&func), 2);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL_LIVE), 1);
    }

    #[test]
    fn a_distance_too_large_to_be_a_real_access_is_not_discharged() {
        // The guard's bound. The two readings of the arithmetic agree while the numbers stay
        // small, so a rule proved at sixty four bits is not asked about anything else. Nothing
        // here is wrong, it simply is not proved, and a check that is not proved to be unnecessary
        // stays.
        let huge = i128::from(u64::MAX) * 4;
        let fact = Fact { base: Value::new(0), offset: 0, size: huge };
        let asked = Fact { base: Value::new(0), offset: huge / 2, size: 4 };
        assert!(!super::covers(&fact, &asked));
    }

    #[test]
    fn a_range_of_addresses_wider_than_the_rule_allows_is_not_discharged() {
        // The guard on `reached.i64` bounds each of the three numbers at four gigabytes, for the
        // reason the rule file gives: past there the compiler's `i128` reading of the guard and the
        // solver's sixty four bit reading part company, and a rule proved under one and run under
        // the other is a rule proved about arithmetic that is not happening. A step whose range is
        // that wide is the usual case rather than a corner, since an index nothing has bounded says
        // nothing about where the access lands.
        let base = Value::new(0);
        let whole = Fact::whole(base, i128::from(u64::MAX) * 4);
        let asked = super::Reach { base, low: 0, width: i128::from(u64::MAX), size: 4 };
        assert!(!super::reaches(&whole, &asked));
    }

    #[test]
    fn a_range_of_addresses_that_ends_where_the_object_does_is_discharged() {
        // Sixteen bytes, a step somewhere in nought to eleven, four bytes read. The last address
        // the walk can reach is the last one in the object, which is inside it.
        let base = Value::new(0);
        let whole = Fact::whole(base, 16);
        let asked = super::Reach { base, low: 0, width: 12, size: 4 };
        assert!(super::reaches(&whole, &asked));
        let over = super::Reach { base, low: 0, width: 13, size: 4 };
        assert!(!super::reaches(&whole, &over), "one byte further runs off the end");
    }

    #[test]
    fn a_walk_by_a_bounded_step_off_a_local_takes_its_derivation_check_with_it() {
        // The shape `derives` cannot read at all: the pointer that went in is the slot and the one
        // that came out is a value past it, so the two are not one base and two constants. Both
        // ends widen to the slot, the slot holds both ranges, and one thing holding both is what a
        // derivation check asks about.
        let (_, mut func, block, _, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, slot, step);
        deriv(&mut build, slot, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV_RANGE), 1);
    }

    #[test]
    fn a_walk_that_can_leave_the_local_keeps_its_derivation_check() {
        // Nought to fifteen off a slot of eight. Every step is bounded and the answer is still no,
        // because the question is whether the slot holds every address the walk can reach.
        let (_, mut func, block, _, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 8);
        let step = low_bits(&mut build, index, 15);
        let at = walk(&mut build, slot, step);
        deriv(&mut build, slot, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV_RANGE), 0);
        assert_eq!(stats.count(Kind::Missed, super::OVER_THE_LOCAL_DERIV), 1);
    }

    #[test]
    fn a_lifetime_check_a_bounded_walk_lands_inside_a_checked_range_goes() {
        // An access over thirty two bytes establishes the range, and the lifetime check beside it
        // makes that range one a check found alive. The lifetime check on the walk then goes,
        // because every address the walk can reach is in the range that was found alive.
        //
        // Written off a parameter rather than a slot because a slot answers the narrow question on
        // its own. What has to answer this one is a range a check was passed on.
        let (_, mut func, block, pointer, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 32);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, pointer, step);
        live(&mut build, at);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(lives(&func), 1, "the one in front of the access stays");
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE_RANGE), 1);
    }

    #[test]
    fn a_lifetime_check_a_bounded_walk_can_leave_the_checked_range_keeps_it() {
        // The same over eight bytes, under a walk that can go fifteen past the start. A range of
        // eight bytes does not hold an address fifteen along from where it begins.
        let (_, mut func, block, pointer, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 8);
        let step = low_bits(&mut build, index, 15);
        let at = walk(&mut build, pointer, step);
        live(&mut build, at);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(lives(&func), 2);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE_RANGE), 0);
    }

    /// A stack slot of `size` bytes, in the entry block where the verifier wants one.
    fn local(build: &mut Builder<'_>, size: u64) -> Value {
        let info = MemInfo {
            size,
            align: 8,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let extra = Extra::Mem(build.func().add_mem(info));
        build.value(InstData { extra, ..InstData::new(Opcode::Alloca) }, Type::PTR)
    }

    /// A function taking a pointer and an index, with one block.
    fn indexed() -> (Interner, Func, Block, Value, Value) {
        let mut names = Interner::new();
        let name = names.intern("f");
        let mut func = Func::new(name, Signature::new().with_params(&[Type::PTR, Type::int(64)]));
        let block = func.create_block();
        let pointer = func.append_param(block, Type::PTR);
        let index = func.append_param(block, Type::int(64));
        (names, func, block, pointer, index)
    }

    /// A pointer a value past another one.
    fn walk(build: &mut Builder<'_>, pointer: Value, by: Value) -> Value {
        let args = build.func().push_values(&[pointer, by]);
        build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR)
    }

    /// The low bits of a value, which is a step the ranges can put a number on.
    fn low_bits(build: &mut Builder<'_>, value: Value, mask: i128) -> Value {
        let bits = build.iconst(Type::int(64), mask);
        build.binary(Opcode::And, value, bits, Flags::NONE)
    }

    #[test]
    fn a_walk_by_a_step_the_ranges_bound_inside_a_local_goes() {
        // Section 7.2's third source. The step is not a constant, so the walk stops at the
        // `ptr_add` and the fact that comes out is about a base nobody knows the size of. What
        // the ranges say is that the step is somewhere in nought to seven, so the four bytes the
        // access wants are somewhere in nought to eleven, and all of that is inside the sixteen
        // the slot is.
        let (_, mut func, block, _, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, slot, step);
        check(&mut build, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_RANGE), 1);
    }

    #[test]
    fn a_check_whose_capability_is_further_back_than_its_base_is_answered_by_the_ranges() {
        // The row #1390 is about. The capability was taken at the pointer, the address is that
        // pointer stepped by something nobody can read, and the constant reader stops at the step
        // so it has no base and no constant to ask a rule with. Carrying the walk on past the
        // step with the ranges lands on the pointer the capability names, and the sixty four
        // bytes an earlier check proved hold every address the step can reach.
        let (_, mut func, block, pointer, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 64);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, pointer, step);
        checking_at(&mut build, pointer, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_MIDWAY), 1);
    }

    #[test]
    fn a_step_that_can_reach_past_what_was_checked_keeps_its_check() {
        // The same function with the mask widened. The step is somewhere in nought to a hundred
        // and twenty seven, the four bytes can start as far out as that, and the check that ran
        // proved sixty four. Nothing here says the rest of it belongs to the same instance.
        let (_, mut func, block, pointer, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 64);
        let step = low_bits(&mut build, index, 127);
        let at = walk(&mut build, pointer, step);
        checking_at(&mut build, pointer, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 2);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_MIDWAY), 0);
        // The row that says a fact about this pointer was there and the walk leaves it, rather
        // than the row for a pointer nothing has an extent for. This is the honest refusal.
        assert_eq!(stats.count(Kind::Missed, super::MIDWAY_OVER), 1);
    }

    #[test]
    fn a_lifetime_check_further_back_than_its_base_is_answered_the_same_way() {
        // The other half. The first access puts a lifetime fact in, widened by its own bounds
        // check to the sixty four bytes that check proved are one instance, and every address the
        // step can reach is inside it.
        let (_, mut func, block, pointer, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 64);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, pointer, step);
        living_at(&mut build, pointer, at);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(lives(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_MIDWAY_LIVE), 1);
    }

    #[test]
    fn the_two_reasons_a_midway_lifetime_check_is_left_alone_are_counted_apart() {
        // The same pair as on the bounds half, so that all four midway rows are pinned. The first
        // function has a lifetime fact about the pointer and a step that walks off the end of it,
        // and the second has no fact about the pointer at all. The pass refuses both and the
        // rows have to say which refusal it was, because one of them is a range worth tightening
        // and the other is an object nothing here will ever have an extent for.
        let (_, mut func, block, pointer, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 64);
        let step = low_bits(&mut build, index, 127);
        let at = walk(&mut build, pointer, step);
        living_at(&mut build, pointer, at);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_MIDWAY_LIVE), 0);
        assert_eq!(stats.count(Kind::Missed, super::MIDWAY_OVER_LIVE), 1);

        let (_, mut func, block, pointer, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, pointer, step);
        living_at(&mut build, pointer, at);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(stats.count(Kind::Missed, super::MIDWAY_NO_EXTENT_LIVE), 1);
        assert_eq!(stats.count(Kind::Missed, super::MIDWAY_OVER_LIVE), 0);
    }

    #[test]
    fn a_walk_by_a_step_the_ranges_cannot_bound_is_left_alone() {
        // The same function with the mask taken off. A parameter can be anything, so the range of
        // addresses the walk reaches is the whole of memory and no slot covers it.
        let (_, mut func, block, _, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        let at = walk(&mut build, slot, index);
        check(&mut build, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_RANGE), 0);
    }

    #[test]
    fn a_walk_a_bounded_step_can_take_off_the_end_of_a_local_is_left_alone() {
        // Nought to seven again, four bytes again, and a slot of eight this time. The step being
        // bounded is not the question. The question is whether every address it can reach is
        // inside the slot, and seven plus four is not.
        let (_, mut func, block, _, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 8);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, slot, step);
        check(&mut build, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_RANGE), 0);
    }

    #[test]
    fn a_constant_step_past_a_bounded_one_is_walked_too() {
        // A field of an element of an array of structs, which is the shape this is for. The array
        // index needs a range and the field offset does not, and the walk has to get through both.
        let (_, mut func, block, _, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 32);
        let step = low_bits(&mut build, index, 15);
        let element = walk(&mut build, slot, step);
        let field = past(&mut build, element, 8);
        check(&mut build, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_RANGE), 1);
    }

    #[test]
    fn what_a_range_discharge_records_is_the_bytes_and_not_the_range() {
        // The second check is the same bytes as the first, and the first went because a made up
        // range around it was inside the slot. What the first one proved is that those bytes are
        // in the slot, so the second one goes on that rather than on the ranges being asked all
        // over again.
        let (_, mut func, block, _, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, slot, step);
        check(&mut build, at, 4);
        check(&mut build, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_RANGE), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
    }

    /// A stack slot whose size the program works out, which is what a variable length array is.
    fn growable(build: &mut Builder<'_>, size: Value) -> Value {
        let info = MemInfo {
            size: 0,
            align: 8,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let extra = Extra::Mem(build.func().add_mem(info));
        let args = build.func().push_values(&[size]);
        build.value(InstData { args, extra, ..InstData::new(Opcode::Alloca) }, Type::PTR)
    }

    #[test]
    fn a_check_of_bytes_inside_a_local_goes_with_nothing_in_front_of_it() {
        // Section 7.2's first source. No check established this and none had to: an `alloca` of
        // sixteen bytes is sixteen bytes of one storage instance because that is what it makes.
        let (_, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        let field = past(&mut build, slot, 8);
        check(&mut build, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LOCAL), 1);
    }

    #[test]
    fn a_check_past_the_end_of_a_local_stays() {
        // The slot is sixteen bytes and the access runs to twenty. Nothing about it being a local
        // says anything about the four bytes after it, which belong to whatever the frame puts
        // there next.
        let (_, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        let field = past(&mut build, slot, 16);
        check(&mut build, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LOCAL), 0);
    }

    #[test]
    fn a_check_of_bytes_inside_a_local_goes_across_a_call() {
        // The other half of what makes the fact worth having. A callee cannot free a frame slot,
        // so unlike everything the walk carries this one is not thrown away at a call.
        let (mut names, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        let callee = names.intern("might_free");
        let signature = build.func().add_signature(Signature::new());
        build.call(callee, signature, &[]);
        check(&mut build, slot, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LOCAL), 1);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL), 0);
    }

    #[test]
    fn a_check_inside_a_variable_length_array_stays() {
        // How many bytes it is is a value the program works out, and the payload's size field
        // reads zero. A pass that read it anyway would discharge every check in the array.
        let (_, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let bytes = build.iconst(Type::int(64), 64);
        let slot = growable(&mut build, bytes);
        check(&mut build, slot, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LOCAL), 0);
    }

    #[test]
    fn a_lifetime_check_in_a_local_goes_with_nothing_in_front_of_it() {
        // The frame slot rule, and the point is that neither of these has a check in front of it.
        // A slot is alive until the function returns, so a lifetime check anywhere inside one is
        // asking a question the `alloca` already answered.
        let (_, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        live(&mut build, slot);
        let field = past(&mut build, slot, 12);
        live(&mut build, field);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(lives(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE_LOCAL), 2);
    }

    #[test]
    fn a_lifetime_check_inside_a_local_goes_across_a_call() {
        // The last of the five ways a lifetime check is taken out, pinned here because the
        // argument in the module comment about keeping bounds facts across a call is an argument
        // about all five. Three of them survive a call and none of the three is about storage a
        // callee could free, which is what makes them harmless to a bounds fact that crossed. This
        // is the frame slot one, and the other two already have a test each.
        let (mut names, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        let callee = names.intern("might_free");
        let signature = build.func().add_signature(Signature::new());
        build.call(callee, signature, &[]);
        live(&mut build, slot);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(lives(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE_LOCAL), 1);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL_LIVE), 0);
    }

    #[test]
    fn a_lifetime_check_past_the_end_of_a_local_stays() {
        // The slot answers for its own bytes and no further, so an address outside it is a
        // different instance and a question nothing has answered.
        let (_, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        live(&mut build, slot);
        let field = past(&mut build, slot, 24);
        live(&mut build, field);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(lives(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE_LOCAL), 1);
    }

    #[test]
    fn something_ending_a_lifetime_turns_the_frame_slot_rule_off() {
        // The gate, and with it the widening the frame slot rule usually hides. With a `meta_end`
        // anywhere in the function the slot answers nothing, so the first check stays and pays,
        // and what takes the second one out is the first one widened to the whole slot.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        live(&mut build, slot);
        let field = past(&mut build, slot, 12);
        live(&mut build, field);
        let size = build.iconst(Type::int(64), 16);
        let args = build.func().push_values(&[pointer, size]);
        build.inst(InstData { args, ..InstData::new(Opcode::MetaEnd) }, &[]);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(lives(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE_LOCAL), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE), 1);
    }

    /// Puts `cap_of` and a `check_deriv` for a walk from `from` to `to` into a block.
    ///
    /// The stride is the width of one element, which is what `rucc-safety` passes and what the
    /// runtime uses for a pointer that walked off the near end. This pass does not read it.
    fn deriv(build: &mut Builder<'_>, from: Value, to: Value, stride: i128) {
        deriving_at(build, from, from, to, stride);
    }

    /// The same, with the capability taken at `held` rather than at the address the walk starts on.
    fn deriving_at(build: &mut Builder<'_>, held: Value, from: Value, to: Value, stride: i128) {
        let args = build.func().push_values(&[held]);
        let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let width = build.iconst(Type::int(64), stride);
        let args = build.func().push_values(&[capability, from, to, width]);
        build.inst(InstData { args, ..InstData::new(Opcode::CheckDeriv) }, &[]);
    }

    /// How many derivation checks are left in a function.
    fn derivs(func: &Func) -> usize {
        func.blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<_>>())
            .filter(|&inst| func[inst].opcode == Opcode::CheckDeriv)
            .count()
    }

    #[test]
    fn a_walk_inside_a_checked_range_goes() {
        // Sixteen bytes were checked, and the walk goes from the start of them to eight in. Both
        // ends are in one range, so the second address is in the instance the first belongs to.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        let field = past(&mut build, pointer, 8);
        deriv(&mut build, pointer, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV), 1);
    }

    #[test]
    fn a_walk_that_leaves_the_checked_range_stays() {
        // Four bytes were checked and the walk goes eight past them. Nothing here says the two
        // addresses are in one instance, which is the whole of what the check is about.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 4);
        let field = past(&mut build, pointer, 8);
        deriv(&mut build, pointer, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV), 0);
    }

    #[test]
    fn a_walk_whose_capability_was_taken_where_the_pointer_came_from_is_read_too() {
        // The same two rules on the near end of a walk. Sixteen bytes were checked, the walk runs
        // from eight in to twelve in, and the capability is the one the pointer those two came off
        // got. The near end has to reach back to that pointer for the answer to be about the
        // instance the capability names, which is what the fact it asks does.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        let field = past(&mut build, pointer, 8);
        let next = past(&mut build, pointer, 12);
        deriving_at(&mut build, pointer, field, next, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV), 1);
    }

    #[test]
    fn two_ranges_holding_one_end_each_do_not_answer_a_walk() {
        // The case the one fact rule is written for. Both addresses have been checked, so both are
        // inside some instance, and nothing says it is the same one. The walk stays.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 4);
        let field = past(&mut build, pointer, 64);
        check(&mut build, field, 4);
        deriv(&mut build, pointer, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV), 0);
    }

    #[test]
    fn a_walk_inside_a_local_goes_with_nothing_in_front_of_it() {
        // The shape almost every derivation check in real code has: a field of a local struct.
        // `rucc-safety` emits the walk before the bounds check on what it produced, so a fact from
        // an earlier check is usually the wrong size for it and the local is what answers.
        let (_, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        let field = past(&mut build, slot, 8);
        deriv(&mut build, slot, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV_LOCAL), 1);
    }

    #[test]
    fn a_walk_off_the_end_of_a_local_stays() {
        // Where the slot stops is where the fact stops. One past the end is the case the runtime
        // has slack for and this pass does not use any of it.
        let (_, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        let field = past(&mut build, slot, 16);
        deriv(&mut build, slot, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV_LOCAL), 0);
    }

    #[test]
    fn a_walk_a_call_stands_between_stays_and_is_counted() {
        // The same price the other two kinds pay, reported the same way, so the cost of not
        // trusting a call is a number per function rather than a paragraph.
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        let callee = names.intern("might_free");
        let signature = build.func().add_signature(Signature::new());
        build.call(callee, signature, &[]);
        let field = past(&mut build, pointer, 8);
        deriv(&mut build, pointer, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 1);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL_DERIV), 1);
    }

    #[test]
    fn a_check_the_module_says_is_inside_a_global_goes_with_nothing_in_front_of_it() {
        // The other half of section 7.2's first source. The size of a global lives on the module
        // and this pass is given one function, so the answer arrives as a flag `crate::extents`
        // wrote before the pipeline started, and all three kinds carry it.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        let field = past(&mut build, pointer, 8);
        deriv(&mut build, pointer, field, 1);
        access(&mut build, field, 4);
        build.ret(&[]);
        marked(&mut func);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(lives(&func), 0);
        assert_eq!(derivs(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_STATIC), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE_STATIC), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV_STATIC), 1);
    }

    #[test]
    fn a_check_the_module_says_every_caller_hands_in_goes_with_nothing_in_front_of_it() {
        // Section 7.5's summaries, arriving the same way a global's extent does and for the same
        // reason: which object a caller passes is a fact about a different function. What the flag
        // says is an extent and a lifetime, because the objects `crate::params` believes are a
        // caller's frame slot and a global and both are alive for as long as the call runs.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        let field = past(&mut build, pointer, 8);
        deriv(&mut build, pointer, field, 1);
        access(&mut build, field, 4);
        build.ret(&[]);
        flagged(&mut func, Flags::HANDED);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(lives(&func), 0);
        assert_eq!(derivs(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_HANDED), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE_HANDED), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV_HANDED), 1);
    }

    /// A function that takes an index, allocates `size` bytes and tests the answer against null.
    ///
    /// Gives back the block where the test has passed, the block where it has not, the pointer and
    /// the index. The flag is put on by hand, because which calls deserve it is a question about a
    /// module and `crate::heap` is what answers it.
    ///
    /// The index is there for the tests about a walk by a value. A parameter on its own is any
    /// number at all, so a test that wants a bounded one puts [`low_bits`] over it the same way the
    /// local tests do.
    fn allocation(size: i128) -> (Interner, Func, Block, Block, Value, Value) {
        let mut names = Interner::new();
        let name = names.intern("f");
        let mut func = Func::new(name, Signature::new().with_params(&[Type::int(64)]));
        let entry = func.create_block();
        let inside = func.create_block();
        let outside = func.create_block();
        let index = func.append_param(entry, Type::int(64));
        let mut build = Builder::new(&mut func, entry);
        let signature = build.func().add_signature(
            Signature::new().with_params(&[Type::int(64)]).with_returns(&[Type::PTR]),
        );
        let bytes = build.iconst(Type::int(64), size);
        let call = build.call(names.intern("malloc"), signature, &[bytes]);
        let at = build.func();
        at[call].flags |= Flags::HEAP;
        let pointer = at[call].results().next().expect("a call that gives back a pointer");
        let zero = build.iconst(Type::int(64), 0);
        let null = build.unary(Opcode::IntToPtr, zero, Type::PTR);
        let condition = build.icmp(IntPred::Ne, pointer, null);
        build.br_if(condition, inside, &[], outside, &[]);
        let mut build = Builder::new(&mut func, outside);
        build.ret(&[]);
        (names, func, inside, outside, pointer, index)
    }

    #[test]
    fn a_check_inside_an_allocation_the_program_tested_goes() {
        // The third of the objects whose extent nobody had to check for. `malloc(16)` says how
        // many bytes it made in the call, and the branch on null is what makes it true here.
        let (_, mut func, inside, _, pointer, _) = allocation(16);
        let mut build = Builder::new(&mut func, inside);
        let field = past(&mut build, pointer, 8);
        deriv(&mut build, pointer, field, 1);
        access(&mut build, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(derivs(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_MADE), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV_MADE), 1);
        // The lifetime check is the one an allocation says nothing about, because a `free` in this
        // same function can end it, and it is what reports a use after free.
        assert_eq!(lives(&func), 1);
    }

    #[test]
    fn a_check_on_an_allocation_nobody_tested_stays() {
        // Down the other arm the pointer is null, a null pointer is inside no object at all, and
        // the check is one that is supposed to fail.
        let (_, mut func, _, outside, pointer, _) = allocation(16);
        let mut build = Builder::new(&mut func, outside);
        access(&mut build, pointer, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_MADE), 0);
    }

    #[test]
    fn a_check_past_the_end_of_an_allocation_stays() {
        // Four bytes at offset fourteen is two bytes past the sixteen that were asked for, and
        // those two bytes are what the check is for.
        let (_, mut func, inside, _, pointer, _) = allocation(16);
        let mut build = Builder::new(&mut func, inside);
        let field = past(&mut build, pointer, 14);
        access(&mut build, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_MADE), 0);
    }

    #[test]
    fn a_walk_that_leaves_an_allocation_stays() {
        // One end inside and the other past the end is a walk out of the object, which is what a
        // derivation check is there to catch, so both ends have to be inside before it goes.
        let (_, mut func, inside, _, pointer, _) = allocation(16);
        let mut build = Builder::new(&mut func, inside);
        let field = past(&mut build, pointer, 32);
        deriv(&mut build, pointer, field, 1);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV_MADE), 0);
    }

    #[test]
    fn a_check_inside_an_allocation_goes_across_a_call() {
        // The other reason a fact read off the instruction is worth having. How many bytes an
        // allocator made is not something a callee can change, so unlike a fact from a check that
        // ran this one is still there on the far side of a call.
        let (mut names, mut func, inside, _, pointer, _) = allocation(16);
        let mut build = Builder::new(&mut func, inside);
        access(&mut build, pointer, 4);
        let signature = build.func().add_signature(Signature::new());
        build.call(names.intern("g"), signature, &[]);
        access(&mut build, pointer, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_MADE), 2);
        // Both lifetime checks stay, and the second one is the one a `free` inside `g` would make
        // report.
        assert_eq!(lives(&func), 2);
    }

    /// A function that allocates `size` bytes and never looks at what it got back.
    ///
    /// The shape `bench/safety/a-strided-column-sum.c` has. The size on its own must not answer a
    /// check here, because reading through what `malloc` gave back without testing it is the bug
    /// this compiler is for.
    fn untested(size: i128) -> (Interner, Func, Block, Value, Value) {
        let mut names = Interner::new();
        let name = names.intern("f");
        let mut func = Func::new(name, Signature::new().with_params(&[Type::int(64)]));
        let block = func.create_block();
        let index = func.append_param(block, Type::int(64));
        let mut build = Builder::new(&mut func, block);
        let signature = build.func().add_signature(
            Signature::new().with_params(&[Type::int(64)]).with_returns(&[Type::PTR]),
        );
        let bytes = build.iconst(Type::int(64), size);
        let call = build.call(names.intern("malloc"), signature, &[bytes]);
        let at = build.func();
        at[call].flags |= Flags::HEAP;
        let pointer = at[call].results().next().expect("a call that gives back a pointer");
        (names, func, block, pointer, index)
    }

    #[test]
    fn a_walk_by_a_step_the_ranges_bound_inside_an_allocation_goes() {
        // The first half of tamnd/rucc#880. The step is not a constant, so the walk stops at the
        // `ptr_add` and what answers the check has to be asked of the range of addresses it can
        // reach. That range is nought to seven plus the four bytes the access wants, all of it
        // inside the sixteen the call says it made, and the branch on null is what makes the
        // sixteen true here.
        let (_, mut func, inside, _, pointer, index) = allocation(16);
        let mut build = Builder::new(&mut func, inside);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, pointer, step);
        check(&mut build, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_MADE), 1);
    }

    #[test]
    fn a_walk_by_a_step_that_can_leave_an_allocation_stays() {
        // The same function with the mask widened. Nought to thirty one plus four bytes runs off
        // the end of sixteen, and the bytes past the end are what the check is for.
        let (_, mut func, inside, _, pointer, index) = allocation(16);
        let mut build = Builder::new(&mut func, inside);
        let step = low_bits(&mut build, index, 31);
        let at = walk(&mut build, pointer, step);
        check(&mut build, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_MADE), 0);
    }

    #[test]
    fn a_derivation_by_a_step_the_ranges_bound_inside_an_allocation_goes() {
        // The same for the derivation check, which is the one the column sum is left with. Both
        // ends have to be inside and inside the same object: the near end is the pointer itself and
        // the far end is anywhere in nought to seven past it.
        let (_, mut func, inside, _, pointer, index) = allocation(16);
        let mut build = Builder::new(&mut func, inside);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, pointer, step);
        deriv(&mut build, pointer, at, 1);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV_MADE), 1);
    }

    #[test]
    fn a_walk_into_an_allocation_nobody_tested_stays() {
        // The other half of the rule, which this does not weaken. A program that walks into what
        // `malloc` gave back without ever looking at it is a program that reads through null when
        // the allocation fails, and the checks are what report it.
        let (_, mut func, block, pointer, index) = untested(16);
        let mut build = Builder::new(&mut func, block);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, pointer, step);
        check(&mut build, at, 4);
        deriv(&mut build, pointer, at, 1);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(derivs(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_MADE), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV_MADE), 0);
    }

    #[test]
    fn a_check_every_caller_hands_in_goes_across_a_call() {
        // The reason the flag is worth having at all. A frame slot of the caller is not something
        // the callee's own callees can free, so the fact does not die at a call the way a fact
        // from a check that ran does.
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 4);
        let signature = build.func().add_signature(Signature::new());
        build.call(names.intern("g"), signature, &[]);
        access(&mut build, pointer, 4);
        build.ret(&[]);
        flagged(&mut func, Flags::HANDED);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(lives(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_HANDED), 2);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE_HANDED), 2);
    }

    #[test]
    fn a_check_inside_a_global_goes_across_a_call() {
        // A callee can free what a global points at and cannot free the global, which lives as
        // long as the program does. So this is the one fact besides a local that a call leaves
        // standing, and it is read off the instruction rather than out of the scope for that
        // reason.
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        let callee = names.intern("might_free");
        let signature = build.func().add_signature(Signature::new());
        build.call(callee, signature, &[]);
        access(&mut build, pointer, 4);
        build.ret(&[]);
        marked(&mut func);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(lives(&func), 0);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL), 0);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL_LIVE), 0);
    }

    #[test]
    fn a_check_the_module_marked_costs_fuel_like_any_other() {
        // A discharge is a discharge whatever established the fact, so `-fpass-fuel` has to stop
        // this one too or a bisection would step over it.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 4);
        build.ret(&[]);
        marked(&mut func);
        let stats =
            DISCHARGE.run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::of(1));
        assert_eq!(checks(&func) + lives(&func), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL_LIVE), 1);
    }

    #[test]
    fn a_walk_whose_two_ends_are_off_two_pointers_with_no_ranges_says_the_same() {
        // Nothing in this function steps by a value, so the ranges are never built and the answer
        // has to come out of the constant reader alone. That reader stopped for one reason, and it
        // is the same reason.
        let (_, mut func, block, pointer) = blank();
        let other = func.append_param(block, Type::PTR);
        let mut build = Builder::new(&mut func, block);
        let at = past(&mut build, other, 8);
        deriv(&mut build, pointer, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 1);
        assert_eq!(stats.count(Kind::Missed, super::TWO_BASES_DERIV), 1);
    }

    #[test]
    fn a_walk_whose_two_ends_are_off_two_pointers_says_so() {
        // Nothing comparable to ask about. Both ends are readable and each is somewhere inside
        // something, and two facts of that shape say nothing at all about it being one something,
        // which is the only thing a derivation check wants to know.
        let (_, mut func, block, pointer, index) = indexed();
        let other = func.append_param(block, Type::PTR);
        let mut build = Builder::new(&mut func, block);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, other, step);
        deriv(&mut build, pointer, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 1);
        assert_eq!(stats.count(Kind::Missed, super::TWO_BASES_DERIV), 1);
    }

    #[test]
    fn a_walk_off_a_pointer_this_function_was_handed_says_so() {
        // The largest pile after a loaded pointer, 1321 checks on SQLite. Everything about the
        // shape is readable: one base, a step the ranges bound, both ends off that base. What is
        // missing is how many bytes belong to the object, and a pointer that arrived as a
        // parameter is one nothing in the function can say that about. Section 7.5's summaries are
        // what would.
        let (_, mut func, block, pointer, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, pointer, step);
        deriv(&mut build, pointer, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_EXTENT_HANDED), 1);
    }

    #[test]
    fn a_walk_off_a_pointer_this_function_loaded_says_so() {
        // The largest pile of the lot, 2155 checks on SQLite, and the shape is `p->field[i]`. The
        // extent of what a pointer in memory points at is not written down anywhere the compiler
        // can see today, which is what `__counted_by` and the type plane are for.
        let (_, mut func, block, pointer, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        let info = MemInfo {
            size: 8,
            align: 8,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let args = build.func().push_values(&[pointer]);
        let extra = Extra::Mem(build.func().add_mem(info));
        let held = build.value(InstData { args, extra, ..InstData::new(Opcode::Load) }, Type::PTR);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, held, step);
        deriv(&mut build, held, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_EXTENT_LOADED), 1);
    }

    #[test]
    fn a_walk_off_a_global_says_so() {
        // 485 checks on SQLite, and the one pile of the four where somebody does know the answer.
        // A global's extent is on the module, `crate::extents` reads it and writes the fact onto
        // every check it can settle before the pipeline starts, and it cannot settle this one
        // because it runs before anything has put a number on the index. See tamnd/rucc#878.
        let (mut names, mut func, block, _, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        let extra = Extra::Symbol(names.intern("g"));
        let base = build.value(InstData { extra, ..InstData::new(Opcode::GlobalAddr) }, Type::PTR);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, base, step);
        deriv(&mut build, base, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_EXTENT_GLOBAL), 1);
    }

    #[test]
    fn a_walk_off_a_pointer_the_check_does_not_name_stays() {
        // The capability has to be the `cap_of` of the pointer that went in. One naming something
        // else is asking about a different instance and is not this pass's to answer.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        let field = past(&mut build, pointer, 8);
        let args = build.func().push_values(&[field]);
        let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let width = build.iconst(Type::int(64), 4);
        let args = build.func().push_values(&[capability, pointer, field, width]);
        build.inst(InstData { args, ..InstData::new(Opcode::CheckDeriv) }, &[]);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 1);
        assert_eq!(stats.count(Kind::Missed, super::NOT_ITS_CAPABILITY_DERIV), 1);
    }
}
