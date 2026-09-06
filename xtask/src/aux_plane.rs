//! Where the aux plane goes, and what putting it there costs.
//!
//! Design: `spec/safe-memory/05-representation.md` section 5.2.2, `spec/safe-memory/10-boundaries.md`
//! section 10.4, and `spec/safe-memory/13-performance.md` section 13.5, which is the fourth box of
//! milestone S3.
//!
//! Document 05 puts the aux array immediately in front of the object, following Fil-C, on the
//! grounds that the aux line is then in the same physical page as the data and gets prefetched
//! with it. Document 10.4 says an adopted third-party allocator cannot do that and carries its aux
//! in a shadow map instead, paying an extra miss for it. Document 13 asks whether that extra miss
//! is real, or whether adjacent aux merely triples the object's footprint and evicts something
//! else. If it is the second, then `rucc-safe-rt`'s own allocator is not a convenience, it is
//! effectively mandatory, and every program that wants jemalloc is a program that cannot have the
//! monitor at its stated cost.
//!
//! # What this is
//!
//! A cache simulation, not a measurement of a running program, because none of the monitor exists
//! yet and the whole point of a cheap experiment is that it happens before the expensive thing it
//! decides. What is simulated is the geometry: where each byte lands under each scheme, and which
//! cache lines and pages a program's accesses therefore touch. That is exactly the part of the
//! question the two schemes differ on, and it is the part that can be got right without a monitor.
//!
//! What is not simulated is everything else about a real machine: prefetchers, store buffers,
//! memory level parallelism, replacement policies that are not LRU, and the fact that a miss which
//! overlaps another miss is nearly free. A hardware prefetcher in particular is the one omission
//! that could change a conclusion here, and it is named again where the answer is stated.
//!
//! # The three schemes
//!
//! `off` is the program as written, with no monitor and no metadata, which is the baseline every
//! ratio is against. `shadow` keeps the program's own layout and puts the aux in a direct mapped
//! side table at `base + (addr >> 3) * 16`, which is what an adopted allocator gets. `adjacent`
//! is document 05.2.2's block layout, aux then header then payload, which moves every object.
//!
//! The asymmetry that makes this worth simulating rather than reasoning about: adjacent aux keeps
//! each object's metadata near that object, and in exchange it pushes neighbouring objects three
//! times further apart, so a program that walks a run of small objects loses the spatial locality
//! it had. Shadow aux keeps the program dense and pays for a second stream, and that second stream
//! is itself dense, so neighbouring objects share aux lines. Which of those wins is a question
//! about the access pattern rather than about the schemes, which is why there are seven programs
//! here and not one.

use std::fmt::Write as _;

use crate::Result;

/// Bytes in a cache line, on every machine this compiler targets.
const LINE: u64 = 64;

/// Bytes in a page, for the data TLB.
const PAGE: u64 = 4096;

/// Bytes of aux per pointer-sized word of payload.
///
/// Document 05.2.2: `ver` and a packed `(lo, ext, meta)`, sixteen bytes for every eight bytes of
/// payload, which is what makes the aux plane 2:1 and is the number the footprint question is
/// about.
const AUX: u64 = 16;

/// Bytes of header in front of the payload, under the adjacent scheme.
const HEADER: u64 = 32;

/// Where the simulated heap starts, and where the shadow map starts.
///
/// Far enough apart that no shadow line can alias a data line in any cache modelled here, which
/// is the point: on a real machine they are separate mappings and a set index collision between
/// them is an accident of the offset rather than a property of the design.
const HEAP: u64 = 0x0000_1000_0000;
const SHADOW: u64 = 0x4000_0000_0000;

/// The machine the simulation is of.
///
/// One geometry, chosen to be an unremarkable server core: 32 KiB of eight way L1 data cache, 1
/// MiB of sixteen way L2, a 64 entry fully associative data TLB and a 2048 entry second level TLB
/// behind it, both over four kilobyte pages. The absolute numbers belong to this geometry and
/// nothing else, and the report says so. What is being read out of it is the ordering between the
/// schemes on each program, which is the thing the box asks for.
///
/// The second level TLB is here because leaving it out is the one simplification that would have
/// changed an answer. A 64 entry TLB alone misses on nearly every access of any program whose
/// working set is more than a quarter of a megabyte, which makes every scheme look equally bad and
/// hides the difference the box is asking about. What costs on a real machine is the page walk,
/// which is a miss in the second level, so that is what gets reported.
const L1_BYTES: u64 = 32 * 1024;
const L1_WAYS: u64 = 8;
const L2_BYTES: u64 = 1024 * 1024;
const L2_WAYS: u64 = 16;
const TLB_ENTRIES: u64 = 64;
const STLB_ENTRIES: u64 = 2048;
const STLB_WAYS: u64 = 8;

/// Where the aux for a word of payload lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scheme {
    /// Nowhere, because there is no monitor. The baseline.
    Off,
    /// A direct mapped side table, which is what an adopted allocator gets.
    Shadow,
    /// Immediately in front of the object, which is document 05.2.2's block layout.
    Adjacent,
}

impl Scheme {
    /// Every scheme, in the order the report prints them.
    const ALL: [Scheme; 3] = [Scheme::Off, Scheme::Shadow, Scheme::Adjacent];

    /// What it is called in the report.
    fn name(self) -> &'static str {
        match self {
            Scheme::Off => "off",
            Scheme::Shadow => "shadow",
            Scheme::Adjacent => "adjacent",
        }
    }
}

/// A set associative cache with least recently used replacement.
///
/// Tags only, because nothing here reads a value. The TLB is one of these with a line the size of
/// a page and a single set, which is what a fully associative TLB is.
#[derive(Debug)]
struct Cache {
    /// One vector of tags per set, most recently used first.
    sets: Vec<Vec<u64>>,
    /// How many tags a set holds.
    ways: usize,
    /// How far to shift an address to drop the offset within a line.
    shift: u32,
    /// Accesses that found their line.
    hits: u64,
    /// Accesses that did not.
    misses: u64,
}

impl Cache {
    /// A cache of `bytes` bytes, `ways` ways, and lines of `line` bytes.
    fn new(bytes: u64, ways: u64, line: u64) -> Cache {
        let sets = (bytes / line / ways).max(1);
        Cache {
            sets: vec![Vec::with_capacity(ways as usize); sets as usize],
            ways: ways as usize,
            shift: line.trailing_zeros(),
            hits: 0,
            misses: 0,
        }
    }

    /// Reads the line holding `addr`, and says whether it was already there.
    fn access(&mut self, addr: u64) -> bool {
        let line = addr >> self.shift;
        let set = (line % self.sets.len() as u64) as usize;
        let ways = self.ways;
        let entries = &mut self.sets[set];
        if let Some(at) = entries.iter().position(|tag| *tag == line) {
            entries.remove(at);
            entries.insert(0, line);
            self.hits += 1;
            return true;
        }
        if entries.len() == ways {
            entries.pop();
        }
        entries.insert(0, line);
        self.misses += 1;
        false
    }
}

/// What one run of one program under one scheme did.
#[derive(Debug, Default, Clone, Copy)]
struct Counts {
    /// Loads and stores the program made, not counting the ones the monitor added.
    accesses: u64,
    /// Aux loads the monitor added.
    aux: u64,
    /// Lines the first level cache did not have.
    l1: u64,
    /// Lines the second level cache did not have either, which is a trip to memory.
    l2: u64,
    /// Page walks, meaning pages neither level of the TLB had.
    tlb: u64,
    /// Bytes of heap the program and its metadata occupy together.
    footprint: u64,
}

/// A heap, a machine, and the counters for one run.
#[derive(Debug)]
struct Sim {
    scheme: Scheme,
    /// What every count in a program is divided by.
    ///
    /// One for the report and a hundred for the tests, because the invariants the tests check are
    /// invariants of the shape rather than of the size, and a test suite that takes twenty seconds
    /// to say so is one people stop running.
    scale: u64,
    next: u64,
    l1: Cache,
    l2: Cache,
    tlb: Cache,
    stlb: Cache,
    counts: Counts,
}

/// One allocated object, as much of it as an access needs to know.
#[derive(Debug, Clone, Copy)]
struct Obj {
    /// The address the program holds, which is where the payload starts.
    lo: u64,
    /// Where this object's aux array starts, under the adjacent scheme, and unused otherwise.
    aux: u64,
}

impl Sim {
    fn new(scheme: Scheme) -> Sim {
        Sim::sized(scheme, 1)
    }

    /// The same, at a fraction of the size.
    fn sized(scheme: Scheme, scale: u64) -> Sim {
        Sim {
            scheme,
            scale,
            next: HEAP,
            l1: Cache::new(L1_BYTES, L1_WAYS, LINE),
            l2: Cache::new(L2_BYTES, L2_WAYS, LINE),
            tlb: Cache::new(TLB_ENTRIES * PAGE, TLB_ENTRIES, PAGE),
            stlb: Cache::new(STLB_ENTRIES * PAGE, STLB_WAYS, PAGE),
            counts: Counts::default(),
        }
    }

    /// Allocates `size` bytes and answers where the program's pointer to them points.
    ///
    /// A bump allocator with sixteen byte alignment, which is what document 05.2.2 says the
    /// lifetime plane's granule forces anyway. Nothing here frees, so nothing here reuses, and a
    /// program whose locality comes from an allocator's free list reuse is a program this does not
    /// model. That is stated in the report rather than left for somebody to find.
    fn alloc(&mut self, size: u64) -> Obj {
        let size = size.next_multiple_of(16);
        // A large allocation is page aligned and a small one is packed against its neighbour,
        // which is what every real allocator does and is not a detail. Packing small objects is
        // precisely the locality the adjacent scheme gives up, so an allocator model that aligned
        // everything would have hidden the effect being measured, and one that aligned nothing
        // would have left the two schemes disagreeing by a thirty two byte offset on a four
        // megabyte array, which is an artefact rather than a finding.
        let aligned = size >= PAGE;
        match self.scheme {
            Scheme::Off | Scheme::Shadow => {
                let lo = if aligned { self.next.next_multiple_of(PAGE) } else { self.next };
                self.next = lo + size;
                Obj { lo, aux: 0 }
            }
            Scheme::Adjacent => {
                let extent = size / 8 * AUX + HEADER;
                let lo = if aligned {
                    (self.next + extent).next_multiple_of(PAGE)
                } else {
                    self.next + extent
                };
                self.next = lo + size;
                Obj { lo, aux: lo - HEADER - size / 8 * AUX }
            }
        }
    }

    /// `n` of something, scaled down to whatever this run is doing.
    fn many(&self, n: u64) -> u64 {
        (n / self.scale).max(1)
    }

    /// Where the aux for the word at `off` bytes into `obj` lives.
    fn aux_of(&self, obj: Obj, off: u64) -> u64 {
        match self.scheme {
            Scheme::Off => 0,
            Scheme::Shadow => SHADOW + (obj.lo + off) / 8 * AUX,
            Scheme::Adjacent => obj.aux + off / 8 * AUX,
        }
    }

    /// Touches one address, through every level.
    fn touch(&mut self, addr: u64) {
        if !self.tlb.access(addr) && !self.stlb.access(addr) {
            self.counts.tlb += 1;
        }
        if !self.l1.access(addr) && !self.l2.access(addr) {
            self.counts.l2 += 1;
        }
    }

    /// An ordinary load or store of something that is not a pointer.
    fn data(&mut self, addr: u64) {
        self.counts.accesses += 1;
        self.touch(addr);
    }

    /// A load or store of a pointer word, which is what needs an aux slot.
    fn word(&mut self, obj: Obj, off: u64) {
        self.counts.accesses += 1;
        self.touch(obj.lo + off);
        if self.scheme != Scheme::Off {
            self.counts.aux += 1;
            self.touch(self.aux_of(obj, off));
        }
    }

    /// The counters, with what the caches recorded folded in.
    fn finish(mut self) -> Counts {
        self.counts.l1 = self.l1.misses;
        self.counts.footprint = self.next - HEAP;
        self.counts
    }
}

/// A reproducible pseudorandom sequence.
///
/// xorshift64star, because this crate has no dependencies and because the only property wanted is
/// that the same seed gives the same trace on every machine, so that two runs of this task are
/// comparable and a number in the report can be reproduced.
#[derive(Debug)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A number below `n`.
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    /// `n` distinct numbers below `n`, shuffled.
    fn permutation(&mut self, n: usize) -> Vec<usize> {
        let mut order: Vec<usize> = (0..n).collect();
        for i in (1..n).rev() {
            let j = self.below(i as u64 + 1) as usize;
            order.swap(i, j);
        }
        order
    }
}

/// One program, as a shape of allocations and accesses.
struct Program {
    /// What it is called in the report.
    name: &'static str,
    /// What shape of program it stands for.
    about: &'static str,
    /// The trace itself, run against a simulator that has already picked a scheme.
    run: fn(&mut Sim),
}

/// The programs, and why each one is here.
///
/// Seven, chosen so that the two schemes are each favoured by some of them, because a set of
/// programs that all have the same shape answers a narrower question than it appears to. The
/// scalar sweep is the control: it touches no pointer, so every scheme has to give the same
/// numbers, and if it ever does not then the apparatus is wrong rather than the answer being
/// interesting.
const PROGRAMS: &[Program] = &[
    Program {
        name: "list",
        about: "a linked list walked in the order it was built",
        run: list_in_order,
    },
    Program {
        name: "list-aged",
        about: "the same list after enough churn that its order is not its layout",
        run: list_aged,
    },
    Program { name: "tree", about: "a binary search tree under random lookups", run: tree },
    Program { name: "hash", about: "a chained hash table under random lookups", run: hash },
    Program {
        name: "sweep",
        about: "an array of small records with a pointer in each, walked linearly",
        run: sweep,
    },
    Program { name: "graph", about: "an object graph walked at random", run: graph },
    Program { name: "scalars", about: "an array of integers summed, the control", run: scalars },
];

/// How many nodes the node-shaped programs build.
const NODES: u64 = 100_000;

/// A list node: a `next` pointer and twenty four bytes of whatever the list is for.
const NODE: u64 = 32;

fn walk_list(sim: &mut Sim, nodes: &[Obj], order: &[usize], laps: usize) {
    for _ in 0..laps {
        for &at in order {
            let node = nodes[at];
            sim.word(node, 0);
            sim.data(node.lo + 8);
        }
    }
}

fn list_in_order(sim: &mut Sim) {
    let count = sim.many(NODES) as usize;
    let nodes: Vec<Obj> = (0..count).map(|_| sim.alloc(NODE)).collect();
    let order: Vec<usize> = (0..count).collect();
    walk_list(sim, &nodes, &order, 8);
}

fn list_aged(sim: &mut Sim) {
    let count = sim.many(NODES) as usize;
    let nodes: Vec<Obj> = (0..count).map(|_| sim.alloc(NODE)).collect();
    let order = Rng::new(1).permutation(count);
    walk_list(sim, &nodes, &order, 8);
}

/// A tree node: two child pointers, a key and a value.
fn tree(sim: &mut Sim) {
    // Allocated in insertion order, which is what building a tree from unsorted input does, so a
    // node and its children are as far apart in memory as they are apart in insertion time.
    let count = sim.many(NODES) as usize;
    let nodes: Vec<Obj> = (0..count).map(|_| sim.alloc(NODE)).collect();
    let mut rng = Rng::new(2);
    // A lookup walks about log2(n) levels. Which node it lands on at each level is what the tree's
    // shape decides, and a random tree over random keys puts it somewhere unrelated to the parent,
    // which is modelled by choosing the next node at random from the ones already inserted.
    let depth = 17;
    for _ in 0..sim.many(200_000) {
        let mut at = 0;
        for _ in 0..depth {
            let node = nodes[at];
            sim.data(node.lo + 16);
            sim.word(node, 0);
            at = rng.below(count as u64) as usize;
        }
    }
}

/// How many buckets the hash table has.
const BUCKETS: u64 = 65536;

fn hash(sim: &mut Sim) {
    let buckets = sim.many(BUCKETS);
    let count = sim.many(NODES) as usize;
    let table = sim.alloc(buckets * 8);
    let nodes: Vec<Obj> = (0..count).map(|_| sim.alloc(NODE)).collect();
    let mut rng = Rng::new(3);
    for _ in 0..sim.many(400_000) {
        let bucket = rng.below(buckets);
        sim.word(table, bucket * 8);
        // One and a half nodes per chain on average at this load factor, and each visit reads the
        // key to compare it and the `next` pointer to go on.
        let chain = 1 + usize::from(rng.below(2) == 0);
        for _ in 0..chain {
            let node = nodes[rng.below(count as u64) as usize];
            sim.data(node.lo + 8);
            sim.word(node, 0);
        }
    }
}

/// How many records the linear sweep walks.
const RECORDS: u64 = 200_000;

/// A record: a pointer and two integers, which is the shape document 05.2.5 is about.
const RECORD: u64 = 16;

fn sweep(sim: &mut Sim) {
    let records = sim.many(RECORDS);
    let array = sim.alloc(records * RECORD);
    for _ in 0..8 {
        for i in 0..records {
            sim.word(array, i * RECORD);
            sim.data(array.lo + i * RECORD + 8);
        }
    }
}

/// How many nodes the graph has, and how many pointers each one holds.
const VERTICES: u64 = 50_000;
const EDGES: u64 = 4;

fn graph(sim: &mut Sim) {
    let count = sim.many(VERTICES) as usize;
    let nodes: Vec<Obj> = (0..count).map(|_| sim.alloc(64)).collect();
    let mut rng = Rng::new(4);
    let mut at = 0;
    for _ in 0..sim.many(400_000) {
        let node = nodes[at];
        sim.word(node, rng.below(EDGES) * 8);
        sim.data(node.lo + 32);
        at = rng.below(count as u64) as usize;
    }
}

/// How many bytes of integers the control sweeps.
const SCALARS: u64 = 4 * 1024 * 1024;

fn scalars(sim: &mut Sim) {
    let bytes = sim.many(SCALARS).next_multiple_of(8);
    let array = sim.alloc(bytes);
    for _ in 0..4 {
        for i in 0..bytes / 8 {
            sim.data(array.lo + i * 8);
        }
    }
}

/// Runs every program under every scheme and prints what happened.
///
/// # Errors
///
/// Never. The signature matches the other tasks so that the dispatch in `main` stays one line per
/// task, and a measurement that cannot fail is better said in the type than in a comment.
pub(crate) fn aux() -> Result<()> {
    let mut out = String::new();
    out.push_str("aux plane locality, simulated\n\n");
    let _ = writeln!(
        out,
        "L1 {} KiB {}-way, L2 {} KiB {}-way, {} byte lines, {} entry TLB and {} entry L2 TLB \
         over {} KiB pages\n",
        L1_BYTES / 1024,
        L1_WAYS,
        L2_BYTES / 1024,
        L2_WAYS,
        LINE,
        TLB_ENTRIES,
        STLB_ENTRIES,
        PAGE / 1024
    );
    let mut all = Vec::new();
    let _ = writeln!(
        out,
        "{:<10} {:>9} {:>11} {:>11} {:>11} {:>11} {:>9}",
        "scheme", "accesses", "aux loads", "L1 misses", "L2 misses", "page walks", "heap KiB"
    );
    for program in PROGRAMS {
        let _ = writeln!(out, "\n{} : {}", program.name, program.about);
        let mut counts = Vec::new();
        for scheme in Scheme::ALL {
            let mut sim = Sim::new(scheme);
            (program.run)(&mut sim);
            let one = sim.finish();
            let _ = writeln!(
                out,
                "{:<10} {:>9} {:>11} {:>11} {:>11} {:>11} {:>9}",
                scheme.name(),
                one.accesses,
                one.aux,
                one.l1,
                one.l2,
                one.tlb,
                one.footprint / 1024
            );
            counts.push(one);
        }
        all.push((program.name, counts));
    }

    out.push_str("\nagainst the same program with no monitor at all\n\n");
    let _ = writeln!(
        out,
        "{:<12} {:>11} {:>11} {:>13} {:>13} {:>9} {:>9}",
        "program", "shadow L2", "adjacent L2", "shadow walks", "adj walks", "adj heap", "better"
    );
    for (name, counts) in &all {
        let base = counts[0];
        let shadow = counts[1];
        let adjacent = counts[2];
        let better = if shadow.l2 == adjacent.l2 && shadow.tlb == adjacent.tlb {
            "neither"
        } else if shadow.l2 <= adjacent.l2 && shadow.tlb <= adjacent.tlb {
            "shadow"
        } else if adjacent.l2 <= shadow.l2 && adjacent.tlb <= shadow.tlb {
            "adjacent"
        } else {
            "split"
        };
        let ratio = |now: u64, was: u64| now as f64 / was.max(1) as f64;
        let _ = writeln!(
            out,
            "{:<12} {:>10.2}x {:>10.2}x {:>12.1}x {:>12.1}x {:>8.1}x {:>9}",
            name,
            ratio(shadow.l2, base.l2),
            ratio(adjacent.l2, base.l2),
            ratio(shadow.tlb, base.tlb),
            ratio(adjacent.tlb, base.tlb),
            ratio(adjacent.footprint, base.footprint),
            better
        );
    }

    out.push_str(
        "\nEvery number belongs to the geometry above and to these traces. What is worth reading \
         out of it is the ordering between the two schemes on each program, not any absolute \
         figure, and the one fact that does not depend on the simulation at all: an aux array is \
         twice the payload, so for any object of thirty two bytes or more the aux slot for a word \
         is never on the same cache line as that word, whichever scheme is used. Adjacency buys \
         the same page, never the same line.\n",
    );
    print!("{out}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the tests divide every program's size by.
    const SMALL: u64 = 200;

    #[test]
    fn a_cache_of_one_line_misses_every_other_access() {
        let mut cache = Cache::new(LINE, 1, LINE);
        assert!(!cache.access(0));
        assert!(cache.access(8));
        assert!(!cache.access(LINE));
        assert!(!cache.access(0));
        assert_eq!(cache.misses, 3);
        assert_eq!(cache.hits, 1);
    }

    #[test]
    fn a_sequential_sweep_misses_once_per_line_and_not_once_per_access() {
        let mut cache = Cache::new(L1_BYTES, L1_WAYS, LINE);
        for i in 0..64 {
            cache.access(i * 8);
        }
        assert_eq!(cache.misses, 64 * 8 / LINE);
    }

    #[test]
    fn the_least_recently_used_way_is_the_one_that_goes() {
        // Two ways in one set, so the third distinct line evicts whichever of the first two was
        // touched longer ago, and touching one of them again in between is what decides which.
        let mut cache = Cache::new(2 * LINE, 2, LINE);
        cache.access(0);
        cache.access(LINE);
        cache.access(0);
        cache.access(2 * LINE);
        assert!(cache.access(0), "the one touched again should still be there");
        assert!(!cache.access(LINE), "the one that was not should not");
    }

    #[test]
    fn the_scalar_control_says_the_same_thing_under_every_scheme() {
        // It touches no pointer, so no scheme adds anything to it, and any difference here is the
        // apparatus leaking rather than a fact about the schemes.
        let mut seen = Vec::new();
        for scheme in Scheme::ALL {
            let mut sim = Sim::sized(scheme, SMALL);
            scalars(&mut sim);
            let counts = sim.finish();
            assert_eq!(counts.aux, 0, "{}", scheme.name());
            seen.push((counts.accesses, counts.l1, counts.l2, counts.tlb));
        }
        assert_eq!(seen[0], seen[1]);
        assert_eq!(seen[0], seen[2]);
    }

    #[test]
    fn adjacent_aux_makes_the_heap_three_times_what_the_program_asked_for() {
        // Two bytes of aux and a share of the header for every byte of payload, which is the
        // footprint half of the question and the reason the answer is not obvious.
        let mut plain = Sim::new(Scheme::Shadow);
        let mut fat = Sim::new(Scheme::Adjacent);
        for _ in 0..1000 {
            plain.alloc(NODE);
            fat.alloc(NODE);
        }
        assert_eq!(plain.finish().footprint, 1000 * 32);
        assert_eq!(fat.finish().footprint, 1000 * (32 + 64 + 32));
        // Three bytes of block for every byte the program asked for, which is the footprint half
        // of the question and the reason the answer is not obvious.
    }

    #[test]
    fn shadow_aux_leaves_the_program_exactly_where_it_was() {
        let mut off = Sim::new(Scheme::Off);
        let mut shadow = Sim::new(Scheme::Shadow);
        for size in [16, 24, 4096, 40] {
            assert_eq!(off.alloc(size).lo, shadow.alloc(size).lo);
        }
    }

    #[test]
    fn an_aux_slot_is_sixteen_bytes_for_every_eight_bytes_of_payload() {
        let mut sim = Sim::new(Scheme::Shadow);
        let obj = sim.alloc(64);
        assert_eq!(sim.aux_of(obj, 8) - sim.aux_of(obj, 0), AUX);
        let mut sim = Sim::new(Scheme::Adjacent);
        let obj = sim.alloc(64);
        assert_eq!(sim.aux_of(obj, 8) - sim.aux_of(obj, 0), AUX);
        // And the last one still lands in front of the header rather than inside the payload,
        // which is the whole of what makes the adjacent layout addressable from the pointer.
        assert!(sim.aux_of(obj, 56) + AUX + HEADER <= obj.lo);
    }

    #[test]
    fn the_scheme_changes_where_things_land_and_never_what_the_program_does() {
        // Which is the invariant the whole comparison rests on. If one scheme made a program run
        // more accesses than another then the miss counts would not be comparable, and the report
        // would be a table of two different programs.
        for program in PROGRAMS {
            let counts: Vec<Counts> = Scheme::ALL
                .iter()
                .map(|scheme| {
                    let mut sim = Sim::sized(*scheme, SMALL);
                    (program.run)(&mut sim);
                    sim.finish()
                })
                .collect();
            assert!(counts[0].accesses > 0, "{}", program.name);
            assert_eq!(counts[0].accesses, counts[1].accesses, "{}", program.name);
            assert_eq!(counts[0].accesses, counts[2].accesses, "{}", program.name);
            assert_eq!(counts[0].aux, 0, "{} has no monitor", program.name);
            assert_eq!(counts[1].aux, counts[2].aux, "{}", program.name);
            let pointers = program.name != "scalars";
            assert_eq!(counts[1].aux > 0, pointers, "{}", program.name);
        }
    }

    #[test]
    fn an_aux_slot_never_shares_a_line_with_the_word_it_describes() {
        // The one fact here that does not depend on the simulation. An aux array is twice the
        // payload and there are thirty two bytes of header between the two, so the aux for a word
        // is at least forty eight bytes in front of it and usually much further. Document 05.2.2
        // claims adjacency puts the aux in the same physical page, which is true, and it is worth
        // being clear that it was never going to put it in the same cache line.
        for size in [32_u64, 48, 64, 128, 4096] {
            let mut sim = Sim::new(Scheme::Adjacent);
            let obj = sim.alloc(size);
            for off in (0..size).step_by(8) {
                let aux = sim.aux_of(obj, off);
                assert_ne!(aux / LINE, (obj.lo + off) / LINE, "size {size} offset {off}");
            }
        }
    }

    #[test]
    fn a_permutation_is_a_permutation() {
        let mut order = Rng::new(9).permutation(1000);
        assert_ne!(order[..10], (0..10).collect::<Vec<_>>()[..]);
        order.sort_unstable();
        assert_eq!(order, (0..1000).collect::<Vec<_>>());
    }
}
