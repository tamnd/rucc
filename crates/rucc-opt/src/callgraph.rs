//! Who calls whom in one translation unit, which components they form, and the order to walk them.
//!
//! Design: `spec/optimizer/34-ipa.md` sections 34.1 and 34.6. Section 34.6 puts this first and
//! prices it at roughly five hundred lines: "A callgraph over the unit's functions, with direct
//! edges from calls and a flag for indirect ones; visibility computed per symbol per 34.1; the
//! condensation and its topological order; and an SCC-iterating driver that a pass supplies a
//! transfer function to." Section 34.1 adds why the order is the one it is: "The traversal order is
//! the callgraph's condensation in topological order, and every pass in this document is either
//! callee-to-caller or caller-to-callee over it, with strongly connected components iterated to a
//! fixpoint."
//!
//! Nothing in the pipeline reads this yet. It is here on its own because it is the walk every
//! interprocedural analysis in document 34 performs, and a walk is much easier to argue about
//! before there is an analysis sitting on top of it to argue about at the same time.
//!
//! # Why the components rather than a loop until nothing changes
//!
//! [`crate::nofree`] already does this walk by hand, and what it does is go round every body in the
//! module until no answer moves. That is correct and it is what a first one of these looks like, and
//! it costs a round over the whole unit for every step an answer has to travel. A chain of callers a
//! hundred deep is a hundred rounds over every function in the file, and the SQLite amalgamation has
//! two and a half thousand of them.
//!
//! Over the condensation it is one visit per component in an order that settles each of them before
//! anything that calls into it, so an answer never has to travel between components twice. The only
//! iteration left is inside a component, which is exactly where recursion is, and a component is
//! almost always one function. That is not a micro-optimization, it is the difference between the
//! analysis being quadratic in the unit and being linear in its edges, which is the thing section
//! 34.5 warns about under "The analysis is quadratic on a large unit".
//!
//! # What a node is
//!
//! A node is a name, not a body. Every function the module has, defined or only declared, gets one,
//! and so does every name some body calls that the module has no function of any kind for. That last
//! kind exists because `rucc-safety` emits calls to names it interns without adding a function to
//! hang them on, which is what tamnd/rucc#810 was about, and a graph that only knew about the
//! functions would have no node to put those calls on and would quietly drop the edge.
//!
//! An alias gets a node as well when what it aliases is a function this module has, because a call
//! to the alias is a call to that body and an analysis walking callee to caller needs the edge to
//! see it. An ifunc does not get that edge: what it resolves to is chosen at load time and is not
//! something this module can name, so the ifunc's node reaches the unknown and the resolver it names
//! is recorded as having had its address taken, because the dynamic linker is going to call it and
//! no edge here says so.
//!
//! # Trusting a body, which is section 34.1's gate
//!
//! "Every fact derived from a function body is conditional on the body being the one that runs."
//! [`CallGraph::trusted_body`] is the only way to reach a body through this graph and it hands one
//! back only when three things hold: the module has a definition, the linkage is not one the linker
//! may throw away in favour of another object's, and the symbol is not one the dynamic linker may
//! interpose. That is the same test [`crate::nofree`] applies and it is the same test for the same
//! reason, and it is here so that the next analysis does not write it a third time.
//!
//! # Reaching the unknown
//!
//! [`CallGraph::reaches_unknown`] is one bit per node and it says the node can get to code this
//! graph has no node for. It is set for a call through an address, for inline assembly, and for a
//! target intrinsic, which is the "flag for indirect ones" section 34.6 asks for. It is also set for
//! every node with no trusted body at all, which is the part worth saying out loud: a declaration
//! calls nothing as far as this graph can see, and an analysis that read the edges alone would
//! conclude that a call to `printf` reaches nothing and is therefore harmless. Folding that into the
//! same bit means the safe reading is the one a consumer gets without having to remember anything.
//!
//! # Determinism
//!
//! Spec 03 requires the same input to give the same output, and this is one of the places where it
//! is easy to lose by accident. Nodes come out in the order the module has its functions, then its
//! aliases, then in the order the bodies first mention a name that had no node. Edges come out in
//! the order the body makes the calls, with a repeat of the same callee dropped. A component's nodes
//! are sorted by node index, and a component is iterated in that order. Nothing here iterates a hash
//! map: the one that is here answers "which node is this name" and is never walked.
//!
//! # Where this lives
//!
//! Section 34.1 names a crate `rucc-ipa` once and document 15's crate table has no such crate, so
//! there is nothing to be consistent with. Every module-level analysis this compiler has is already
//! in `rucc-opt`, which is [`crate::nofree`], [`crate::heap`], [`crate::params`],
//! [`crate::extents`], [`crate::image`] and [`crate::outside`], and the pipeline that would build
//! this is in `rucc-opt` too. A crate holding one file that only `rucc-opt` calls is a layer
//! boundary that buys nothing today. [`crate::nofree`] records the same kind of deviation for the
//! same kind of reason.

use std::collections::HashMap;

use rucc_base::Symbol;
use rucc_ir::{AliasKind, Datum, Extra, Func, FuncId, Inst, Linkage, Module, Opcode, Pic};

/// One name in a [`CallGraph`].
///
/// A name rather than a body, because a call names something and whether this module has the body
/// behind it is a separate question the graph answers separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Node(u32);

impl Node {
    /// Where this node sits, which is what indexes the answers [`CallGraph::solve`] hands back.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// What the graph knows about one name.
#[derive(Debug, Clone)]
struct Entry {
    /// The name itself.
    name: Symbol,
    /// The module's function of that name, defined or only declared, and nothing when the name is
    /// an alias or is something a body calls that the module never declared.
    func: Option<FuncId>,
    /// The same function again, and only when its body is the one that will run.
    body: Option<FuncId>,
    /// The nodes this one calls, in the order the body makes the calls, each of them once.
    calls: Vec<Node>,
    /// Whether this node can reach code the graph has no node for.
    unknown: bool,
    /// Whether anything other than a call in this unit can reach this function.
    address_taken: bool,
}

/// The unit's call graph, its condensation, and the order to walk it in.
///
/// Built once from the module, for the reason every module-level analysis here is built once: the
/// answer belongs to the callee and a pass is handed one function.
#[derive(Debug, Clone, Default)]
pub struct CallGraph {
    /// One per name, in the order described under "Determinism" in the module comment.
    entries: Vec<Entry>,
    /// Which node a name is. Asked, never walked.
    by_name: HashMap<Symbol, Node>,
    /// The strongly connected components, callees before callers, each sorted by node index.
    components: Vec<Vec<Node>>,
    /// Which component each node landed in, indexed by node.
    component_of: Vec<u32>,
}

impl CallGraph {
    /// Builds the graph over everything the module can name.
    ///
    /// The `pic` argument is what the link is going to be, and it decides which definitions may be
    /// interposed. See [`CallGraph::trusted_body`].
    #[must_use]
    pub fn of(module: &Module, pic: Pic) -> Self {
        let mut graph = Self::default();
        for id in module.funcs() {
            let func = &module[id];
            let body = (!func.is_declaration() && trusted(func, pic)).then_some(id);
            let node = graph.intern(func.name);
            let at = node.index();
            graph.entries[at].func = Some(id);
            graph.entries[at].body = body;
            // A declaration reaches whatever the definition somewhere else reaches, and so does a
            // definition this link is allowed to replace. Both are the unknown.
            graph.entries[at].unknown = body.is_none();
        }
        // Aliases next, and before the bodies are read, so that a call to an alias finds the node
        // rather than making a second one for the same name.
        for id in module.aliases() {
            let alias = &module[id];
            let node = graph.intern(alias.name);
            match alias.kind {
                // A second name for a body this module may have. The edge is the whole point: a
                // caller of the alias is a caller of what it aliases.
                AliasKind::Alias => {
                    let to = graph.intern(alias.target);
                    graph.entries[node.index()].calls.push(to);
                    // An alias of a name this module has no function for reaches the unknown
                    // through it, which the target node's own bit already says.
                }
                // What an ifunc resolves to is decided when the program is loaded and is not a name
                // this module has. The resolver is called by the dynamic linker rather than by
                // anything here, so its address is taken in the only sense that matters.
                AliasKind::IFunc => {
                    graph.entries[node.index()].unknown = true;
                    let resolver = graph.intern(alias.target);
                    graph.entries[resolver.index()].address_taken = true;
                }
            }
        }
        // The bodies, which is where the edges come from and where a name with no function of any
        // kind first turns up.
        for id in module.funcs() {
            let func = &module[id];
            if func.is_declaration() {
                continue;
            }
            let from = graph.by_name[&func.name];
            for block in func.blocks() {
                for inst in func.insts(block) {
                    graph.read(func, inst, from);
                }
            }
        }
        // And the addresses written into the images, which is how a table of function pointers puts
        // a body somewhere an indirect call can find it.
        for id in module.globals() {
            let init = module[id].init.map(|list| &module[list]).unwrap_or_default();
            for datum in init {
                if let Datum::Addr(reloc) | Datum::Away(reloc) = *datum {
                    graph.took_the_address_of(module[reloc].symbol);
                }
            }
        }
        graph.condense();
        graph
    }

    /// Every node, in the graph's order.
    pub fn nodes(&self) -> impl Iterator<Item = Node> + use<> {
        (0..self.entries.len() as u32).map(Node)
    }

    /// How many nodes there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the module named nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The node for that name, if the graph has one.
    #[must_use]
    pub fn node(&self, name: Symbol) -> Option<Node> {
        self.by_name.get(&name).copied()
    }

    /// The name this node is.
    #[must_use]
    pub fn name(&self, node: Node) -> Symbol {
        self.entries[node.index()].name
    }

    /// The module's function of this name, whether or not it has a body and whether or not the body
    /// may be read.
    ///
    /// For the attributes and the signature, which are what the declaration is there to say. To read
    /// the body use [`CallGraph::trusted_body`] instead.
    #[must_use]
    pub fn func(&self, node: Node) -> Option<FuncId> {
        self.entries[node.index()].func
    }

    /// The body an analysis may derive facts from, and nothing when there is not one.
    ///
    /// Section 34.1's gate. Three things have to hold. The module has to define the function rather
    /// than only declare it. The linkage has to be one the linker will keep, which rules out `weak`
    /// and `common`, because either of those is a definition another object is allowed to win over.
    /// And the symbol has to be one the dynamic linker cannot interpose, which under `-fPIC` means
    /// hidden, protected or internal, unless the build promised `-fno-semantic-interposition`.
    #[must_use]
    pub fn trusted_body(&self, node: Node) -> Option<FuncId> {
        self.entries[node.index()].body
    }

    /// The nodes this one calls directly, each once, in the order the body calls them.
    #[must_use]
    pub fn calls(&self, node: Node) -> &[Node] {
        &self.entries[node.index()].calls
    }

    /// Whether this node can reach code the graph has no node for.
    ///
    /// True for a body with a call through an address, inline assembly or a target intrinsic in it,
    /// true for an ifunc, and true for every node with no trusted body, since a declaration's edges
    /// are not in this unit. An analysis that ignores this and reads only [`CallGraph::calls`] will
    /// decide that a call to `printf` reaches nothing.
    #[must_use]
    pub fn reaches_unknown(&self, node: Node) -> bool {
        self.entries[node.index()].unknown
    }

    /// Whether anything other than a direct call in this unit can reach this function.
    ///
    /// A `global_addr` naming it in some body, a relocation naming it in some global's image, or an
    /// ifunc resolving through it. What it is for is the exclusion section 34.5 states for parameter
    /// removal, "which is why a function whose address escapes is excluded", and the same question
    /// the inliner asks before it considers a function to have no callers left.
    ///
    /// It is not a statement about who calls it. A `static` function whose address is never taken
    /// and whose callers are all in this unit is the case every caller-to-callee analysis wants, and
    /// that is this being false together with the linkage being internal.
    #[must_use]
    pub fn address_taken(&self, node: Node) -> bool {
        self.entries[node.index()].address_taken
    }

    /// The strongly connected components, callees before callers.
    ///
    /// Tarjan gives them in that order already, because it closes a component only once everything
    /// reachable from it has been closed, and the edges here point from a caller to a callee. A
    /// component of one node is the usual case and a component of more than one is recursion, either
    /// a function calling itself or a cycle of them calling each other.
    #[must_use]
    pub fn components(&self) -> &[Vec<Node>] {
        &self.components
    }

    /// Which component this node landed in, as an index into [`CallGraph::components`].
    #[must_use]
    pub fn component_of(&self, node: Node) -> usize {
        self.component_of[node.index()] as usize
    }

    /// Walks the condensation callee before caller, settling each component before moving on.
    ///
    /// `start` gives each node the value the walk begins at and `transfer` works out a node's value
    /// from everything already known. The slice `transfer` is handed is indexed by
    /// [`Node::index`] and holds the current value of every node, which for a callee outside this
    /// component is its settled answer and for a callee inside it is wherever it has got to.
    ///
    /// Inside a component the nodes are visited in ascending index and the round repeats until no
    /// value changes. A component of one node with no edge back to itself is not a cycle, so it is
    /// evaluated once and not checked again, which is the shape of almost every component in a real
    /// unit.
    ///
    /// `transfer` has to be monotone over a lattice of finite height, in the sense that a value it
    /// produces from larger inputs is not smaller. That is what makes the round terminate, and it is
    /// the consumer's to get right: section 34.5 is specific that the optimistic start this enables
    /// "is only sound after the fixpoint, so nothing may read the lattice mid-flight".
    ///
    /// # Panics
    ///
    /// In a checked build, if a component has not settled after a number of rounds far past what any
    /// lattice this is for could need. That is a transfer function that is not monotone rather than
    /// anything about the graph.
    pub fn solve<T, S, F>(&self, start: S, mut transfer: F) -> Vec<T>
    where
        T: Clone + PartialEq,
        S: Fn(Node) -> T,
        F: FnMut(Node, &[T]) -> T,
    {
        let mut answers: Vec<T> = self.nodes().map(&start).collect();
        for part in &self.components {
            if let [only] = part[..] {
                if !self.entries[only.index()].calls.contains(&only) {
                    answers[only.index()] = transfer(only, &answers);
                    continue;
                }
            }
            // Far more rounds than a lattice of finite height could need over a component this
            // size. Nothing is decided by the number and no code is any different either side of
            // it: a walk that reaches it has been handed a transfer function that is not monotone,
            // which is a bug in the caller rather than a component that wanted more rounds.
            let ceiling = 1000 + part.len() * 64;
            let mut rounds = 0usize;
            loop {
                let mut settled = true;
                for &node in part {
                    let now = transfer(node, &answers);
                    if now != answers[node.index()] {
                        answers[node.index()] = now;
                        settled = false;
                    }
                }
                if settled {
                    break;
                }
                rounds += 1;
                debug_assert!(rounds < ceiling, "the transfer function is not monotone");
            }
        }
        answers
    }

    /// The node for that name, made if it is not there yet.
    fn intern(&mut self, name: Symbol) -> Node {
        if let Some(&node) = self.by_name.get(&name) {
            return node;
        }
        let node = Node(self.entries.len() as u32);
        // A name that arrived without a function of its own is a name whose body is somewhere else,
        // so it starts out reaching the unknown. The loop over the module's functions clears it for
        // the ones it can read.
        self.entries.push(Entry {
            name,
            func: None,
            body: None,
            calls: Vec::new(),
            unknown: true,
            address_taken: false,
        });
        self.by_name.insert(name, node);
        node
    }

    /// Records that something other than a call got hold of that name, if it is one of ours.
    ///
    /// A lookup rather than an [`CallGraph::intern`], and that is the whole of the difference
    /// between this and the rest. `global_addr` is how any name at all becomes a value and a
    /// relocation in an image is the same, so most of what arrives here is a global variable rather
    /// than a function. Interning those would have been harmless and it would also have put two
    /// thousand nodes that are not functions into the graph over the SQLite amalgamation, which is a
    /// third of it. Every function whose address this unit can take has to be declared in this unit
    /// for the source to have named it, so a name that has no node by the time this is asked is not
    /// a function.
    fn took_the_address_of(&mut self, name: Symbol) {
        if let Some(&node) = self.by_name.get(&name) {
            self.entries[node.index()].address_taken = true;
        }
    }

    /// Records what one instruction of a body does to the graph.
    fn read(&mut self, func: &Func, inst: Inst, from: Node) {
        let data = &func[inst];
        match data.opcode {
            Opcode::Call | Opcode::TailCall => {
                let name = match data.extra {
                    Extra::Call(at) => func[at].callee,
                    _ => None,
                };
                // A direct call with no name on it should not happen and is treated the way a call
                // through an address is, which is the conservative of the two.
                let Some(name) = name else {
                    self.entries[from.index()].unknown = true;
                    return;
                };
                let to = self.intern(name);
                let calls = &mut self.entries[from.index()].calls;
                if !calls.contains(&to) {
                    calls.push(to);
                }
            }
            // The flag section 34.6 asks for. What is at the other end could be anything with a
            // body, including something this unit never saw.
            // A call built from a block of saved arguments is a call through an address as well.
            Opcode::CallIndirect | Opcode::Apply => self.entries[from.index()].unknown = true,
            // A template the compiler does not read, and the open half of the intrinsic set, which
            // is named rather than enumerated so nothing here knows what one does. `crate::purity`
            // answers the same way about both for the same reason.
            Opcode::InlineAsm | Opcode::TargetIntrinsic => {
                self.entries[from.index()].unknown = true;
            }
            // The address of a function, handed to whoever wanted it.
            Opcode::GlobalAddr => {
                if let Extra::Symbol(name) = data.extra {
                    self.took_the_address_of(name);
                }
            }
            _ => {}
        }
    }

    /// Tarjan, iteratively, filling in the components and which one each node is in.
    ///
    /// Iteratively because the recursion depth is the depth of the call graph and a generated C
    /// file can have a chain of thousands. The order the components come out in is the one
    /// [`CallGraph::components`] promises and is Tarjan's own, not something sorted afterwards.
    fn condense(&mut self) {
        let count = self.entries.len();
        // `u32::MAX` for a node the walk has not reached, which is a value no real index can be
        // because the graph would have run out of memory long before.
        let mut index = vec![u32::MAX; count];
        let mut low = vec![0u32; count];
        let mut on_stack = vec![false; count];
        let mut stack: Vec<u32> = Vec::new();
        let mut frames: Vec<(u32, usize)> = Vec::new();
        let mut next = 0u32;
        self.component_of = vec![u32::MAX; count];
        for root in 0..count as u32 {
            if index[root as usize] != u32::MAX {
                continue;
            }
            index[root as usize] = next;
            low[root as usize] = next;
            next += 1;
            stack.push(root);
            on_stack[root as usize] = true;
            frames.push((root, 0));
            while let Some(&(node, at)) = frames.last() {
                let edges = &self.entries[node as usize].calls;
                if at < edges.len() {
                    let to = edges[at].0;
                    frames.last_mut().expect("the frame just read").1 += 1;
                    if index[to as usize] == u32::MAX {
                        index[to as usize] = next;
                        low[to as usize] = next;
                        next += 1;
                        stack.push(to);
                        on_stack[to as usize] = true;
                        frames.push((to, 0));
                    } else if on_stack[to as usize] {
                        low[node as usize] = low[node as usize].min(index[to as usize]);
                    }
                    continue;
                }
                frames.pop();
                if low[node as usize] == index[node as usize] {
                    let mut part = Vec::new();
                    while let Some(top) = stack.pop() {
                        on_stack[top as usize] = false;
                        part.push(Node(top));
                        if top == node {
                            break;
                        }
                    }
                    part.sort_unstable();
                    let which = self.components.len() as u32;
                    for member in &part {
                        self.component_of[member.index()] = which;
                    }
                    self.components.push(part);
                }
                if let Some(&(above, _)) = frames.last() {
                    low[above as usize] = low[above as usize].min(low[node as usize]);
                }
            }
        }
        debug_assert!(
            self.component_of.iter().all(|&which| which != u32::MAX),
            "every node is in a component"
        );
    }
}

/// Whether the definition in hand is the one that will run.
///
/// The same two questions [`crate::nofree`] asks and the same answers, written here because section
/// 34.1 makes this the graph's own gate rather than each analysis's. A `weak` or `common` definition
/// is one the linker may throw away in favour of another object's, so the body read here is one that
/// may never run. An ordinary external definition can be replaced at load time by `LD_PRELOAD` or by
/// an earlier object in the search order, which is what `Pic::replaceable` answers, and
/// `-fno-semantic-interposition` and `-fvisibility=hidden` are the two ways a build says it will not
/// happen.
fn trusted(func: &Func, pic: Pic) -> bool {
    !matches!(func.linkage, Linkage::Weak | Linkage::Common)
        && !pic.replaceable(func.linkage, func.visibility)
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Alias, AliasKind, AsmInfo, BlockCallList, Builder, CallInfo, Datum, Extra, Flags, Func,
        Global, InstData, Linkage, Module, Opcode, Pic, Reloc, Signature, Type, Visibility,
    };
    use rucc_target::{TargetInfo, Triple};

    use super::{CallGraph, Node};

    /// A module with nothing in it yet.
    fn blank() -> (Interner, Module) {
        let mut names = Interner::new();
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let module = Module::new(names.intern("t.c"), &target);
        (names, module)
    }

    /// Adds a function of that name whose body calls each of those names once, in order.
    fn calling(names: &mut Interner, module: &mut Module, name: &str, callees: &[&str]) {
        let mut func = Func::new(names.intern(name), Signature::new());
        let block = func.create_block();
        let mut build = Builder::new(&mut func, block);
        let signature = build.func().add_signature(Signature::new());
        for callee in callees {
            build.call(names.intern(callee), signature, &[]);
        }
        build.ret(&[]);
        module.add_func(func);
    }

    /// Adds a declaration of that name and nothing else.
    fn declaring(names: &mut Interner, module: &mut Module, name: &str) {
        module.add_func(Func::new(names.intern(name), Signature::new()));
    }

    /// The node for that name, which the test expects to be there.
    fn node(graph: &CallGraph, names: &mut Interner, name: &str) -> Node {
        let name = names.intern(name);
        graph.node(name).unwrap_or_else(|| panic!("no node for {}", names.resolve(name)))
    }

    /// Every component, spelled, in the order the walk produced them.
    fn order(graph: &CallGraph, names: &Interner) -> Vec<Vec<String>> {
        graph
            .components()
            .iter()
            .map(|part| part.iter().map(|&it| names.resolve(graph.name(it)).to_string()).collect())
            .collect()
    }

    #[test]
    fn a_call_is_an_edge_and_the_callee_gets_a_node_of_its_own() {
        let (mut names, mut module) = blank();
        calling(&mut names, &mut module, "f", &["g"]);
        calling(&mut names, &mut module, "g", &[]);
        let graph = CallGraph::of(&module, Pic::Executable);
        let (f, g) = (node(&graph, &mut names, "f"), node(&graph, &mut names, "g"));
        assert_eq!(graph.calls(f), [g]);
        assert_eq!(graph.calls(g), []);
    }

    #[test]
    fn the_same_callee_twice_is_one_edge() {
        let (mut names, mut module) = blank();
        calling(&mut names, &mut module, "f", &["g", "h", "g"]);
        calling(&mut names, &mut module, "g", &[]);
        calling(&mut names, &mut module, "h", &[]);
        let graph = CallGraph::of(&module, Pic::Executable);
        let f = node(&graph, &mut names, "f");
        let (g, h) = (node(&graph, &mut names, "g"), node(&graph, &mut names, "h"));
        assert_eq!(graph.calls(f), [g, h], "the order the body calls them, each once");
    }

    #[test]
    fn a_name_the_module_never_declared_still_gets_a_node() {
        let (mut names, mut module) = blank();
        calling(&mut names, &mut module, "f", &["witness"]);
        let graph = CallGraph::of(&module, Pic::Executable);
        let witness = node(&graph, &mut names, "witness");
        assert_eq!(graph.calls(node(&graph, &mut names, "f")), [witness]);
        assert_eq!(graph.func(witness), None);
        assert_eq!(graph.trusted_body(witness), None);
        assert!(graph.reaches_unknown(witness), "its body is somewhere this graph cannot see");
    }

    #[test]
    fn a_declaration_has_a_function_and_no_body_and_reaches_the_unknown() {
        let (mut names, mut module) = blank();
        declaring(&mut names, &mut module, "printf");
        let graph = CallGraph::of(&module, Pic::Executable);
        let printf = node(&graph, &mut names, "printf");
        assert!(graph.func(printf).is_some());
        assert_eq!(graph.trusted_body(printf), None);
        assert!(graph.reaches_unknown(printf));
    }

    #[test]
    fn a_body_this_link_will_keep_is_one_an_analysis_may_read() {
        let (mut names, mut module) = blank();
        calling(&mut names, &mut module, "f", &[]);
        let graph = CallGraph::of(&module, Pic::Executable);
        let f = node(&graph, &mut names, "f");
        assert!(graph.trusted_body(f).is_some());
        assert!(!graph.reaches_unknown(f));
    }

    #[test]
    fn a_weak_definition_is_not_a_body_this_analysis_may_read() {
        let (mut names, mut module) = blank();
        calling(&mut names, &mut module, "f", &[]);
        let id = module.funcs().next().expect("the one function");
        module[id].linkage = Linkage::Weak;
        let graph = CallGraph::of(&module, Pic::Executable);
        let f = node(&graph, &mut names, "f");
        assert!(graph.func(f).is_some(), "the declaration is still there");
        assert_eq!(graph.trusted_body(f), None, "another object may win over it");
        assert!(graph.reaches_unknown(f));
    }

    #[test]
    fn an_exported_definition_in_a_library_may_be_interposed_and_a_hidden_one_may_not() {
        let (mut names, mut module) = blank();
        calling(&mut names, &mut module, "f", &[]);
        let id = module.funcs().next().expect("the one function");
        let graph = CallGraph::of(&module, Pic::Library);
        assert_eq!(graph.trusted_body(node(&graph, &mut names, "f")), None);
        module[id].visibility = Visibility::Hidden;
        let graph = CallGraph::of(&module, Pic::Library);
        assert!(graph.trusted_body(node(&graph, &mut names, "f")).is_some());
    }

    #[test]
    fn a_call_through_an_address_is_the_flag_and_not_an_edge() {
        let (mut names, mut module) = blank();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let block = func.create_block();
        let mut build = Builder::new(&mut func, block);
        let extra = Extra::Symbol(names.intern("g"));
        let target =
            build.value(InstData { extra, ..InstData::new(Opcode::GlobalAddr) }, Type::PTR);
        let signature = build.func().add_signature(Signature::new());
        let varargs = build.func().push_abis(&[]);
        let info = build.func().add_call(CallInfo { callee: None, signature, varargs });
        let args = build.func().push_values(&[target]);
        build.inst(
            InstData { args, extra: Extra::Call(info), ..InstData::new(Opcode::CallIndirect) },
            &[],
        );
        build.ret(&[]);
        module.add_func(func);
        calling(&mut names, &mut module, "g", &[]);
        let graph = CallGraph::of(&module, Pic::Executable);
        let f = node(&graph, &mut names, "f");
        assert_eq!(graph.calls(f), [], "nothing here names what is at the other end");
        assert!(graph.reaches_unknown(f));
        assert!(graph.address_taken(node(&graph, &mut names, "g")));
    }

    #[test]
    fn a_function_named_in_an_image_has_had_its_address_taken() {
        let (mut names, mut module) = blank();
        calling(&mut names, &mut module, "handler", &[]);
        let handler = names.intern("handler");
        let reloc = module.add_reloc(Reloc { symbol: handler, addend: 0, size: 8 });
        let mut table = Global::new(names.intern("table"), 8, 8);
        table.init = Some(module.push_data(&[Datum::Addr(reloc)]));
        module.add_global(table);
        let graph = CallGraph::of(&module, Pic::Executable);
        assert!(graph.address_taken(node(&graph, &mut names, "handler")));
    }

    #[test]
    fn taking_the_address_of_a_variable_puts_nothing_in_the_graph() {
        let (mut names, mut module) = blank();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let block = func.create_block();
        let mut build = Builder::new(&mut func, block);
        let extra = Extra::Symbol(names.intern("counter"));
        build.value(InstData { extra, ..InstData::new(Opcode::GlobalAddr) }, Type::PTR);
        build.ret(&[]);
        module.add_func(func);
        module.add_global(Global::new(names.intern("counter"), 4, 4));
        let graph = CallGraph::of(&module, Pic::Executable);
        assert_eq!(graph.len(), 1, "the one function and nothing else");
        assert_eq!(graph.node(names.intern("counter")), None);
    }

    #[test]
    fn an_alias_is_an_edge_to_what_it_aliases() {
        let (mut names, mut module) = blank();
        calling(&mut names, &mut module, "caller", &["shorthand"]);
        calling(&mut names, &mut module, "real", &[]);
        module.add_alias(Alias::new(names.intern("shorthand"), names.intern("real")));
        let graph = CallGraph::of(&module, Pic::Executable);
        let shorthand = node(&graph, &mut names, "shorthand");
        let real = node(&graph, &mut names, "real");
        assert_eq!(graph.calls(node(&graph, &mut names, "caller")), [shorthand]);
        assert_eq!(graph.calls(shorthand), [real], "a call to the alias is a call to the body");
    }

    #[test]
    fn an_ifunc_reaches_the_unknown_and_its_resolver_has_had_its_address_taken() {
        let (mut names, mut module) = blank();
        calling(&mut names, &mut module, "resolve", &[]);
        let mut alias = Alias::new(names.intern("memcpy"), names.intern("resolve"));
        alias.kind = AliasKind::IFunc;
        module.add_alias(alias);
        let graph = CallGraph::of(&module, Pic::Executable);
        let memcpy = node(&graph, &mut names, "memcpy");
        assert_eq!(graph.calls(memcpy), [], "what it resolves to is not a name this module has");
        assert!(graph.reaches_unknown(memcpy));
        assert!(graph.address_taken(node(&graph, &mut names, "resolve")));
        assert!(!graph.address_taken(memcpy));
    }

    #[test]
    fn inline_assembly_reaches_the_unknown() {
        let (mut names, mut module) = blank();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let block = func.create_block();
        let mut build = Builder::new(&mut func, block);
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
        build.ret(&[]);
        module.add_func(func);
        let graph = CallGraph::of(&module, Pic::Executable);
        assert!(graph.reaches_unknown(node(&graph, &mut names, "f")));
    }

    #[test]
    fn a_target_intrinsic_reaches_the_unknown() {
        let (mut names, mut module) = blank();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let block = func.create_block();
        let mut build = Builder::new(&mut func, block);
        let extra = Extra::Symbol(names.intern("x86.pause"));
        build.inst(InstData { extra, ..InstData::new(Opcode::TargetIntrinsic) }, &[]);
        build.ret(&[]);
        module.add_func(func);
        let graph = CallGraph::of(&module, Pic::Executable);
        assert!(graph.reaches_unknown(node(&graph, &mut names, "f")));
    }

    #[test]
    fn a_chain_of_callers_comes_out_callee_before_caller() {
        let (mut names, mut module) = blank();
        calling(&mut names, &mut module, "top", &["middle"]);
        calling(&mut names, &mut module, "middle", &["bottom"]);
        calling(&mut names, &mut module, "bottom", &[]);
        let graph = CallGraph::of(&module, Pic::Executable);
        assert_eq!(order(&graph, &names), [["bottom"], ["middle"], ["top"]]);
    }

    #[test]
    fn a_function_that_calls_itself_is_a_component_of_one_that_is_a_cycle() {
        let (mut names, mut module) = blank();
        calling(&mut names, &mut module, "spin", &["spin"]);
        let graph = CallGraph::of(&module, Pic::Executable);
        let spin = node(&graph, &mut names, "spin");
        assert_eq!(graph.calls(spin), [spin]);
        assert_eq!(order(&graph, &names), [["spin"]]);
    }

    #[test]
    fn two_functions_that_call_each_other_are_one_component() {
        let (mut names, mut module) = blank();
        calling(&mut names, &mut module, "even", &["odd"]);
        calling(&mut names, &mut module, "odd", &["even"]);
        calling(&mut names, &mut module, "main", &["even"]);
        let graph = CallGraph::of(&module, Pic::Executable);
        assert_eq!(order(&graph, &names), [vec!["even", "odd"], vec!["main"]]);
        let (even, odd) = (node(&graph, &mut names, "even"), node(&graph, &mut names, "odd"));
        assert_eq!(graph.component_of(even), graph.component_of(odd));
    }

    #[test]
    fn a_component_holds_its_nodes_in_the_graphs_own_order() {
        let (mut names, mut module) = blank();
        // Written so the walk leaves the stack in the opposite order from the one the module has
        // them in, which is what the sort inside the component is there to undo.
        calling(&mut names, &mut module, "a", &["c"]);
        calling(&mut names, &mut module, "b", &["a"]);
        calling(&mut names, &mut module, "c", &["b"]);
        let graph = CallGraph::of(&module, Pic::Executable);
        assert_eq!(order(&graph, &names), [["a", "b", "c"]]);
        let a = node(&graph, &mut names, "a");
        assert_eq!(graph.components()[graph.component_of(a)][0], a);
    }

    #[test]
    fn the_walk_settles_a_component_before_anything_that_calls_into_it() {
        let (mut names, mut module) = blank();
        calling(&mut names, &mut module, "top", &["middle"]);
        calling(&mut names, &mut module, "middle", &["bottom"]);
        calling(&mut names, &mut module, "bottom", &[]);
        let graph = CallGraph::of(&module, Pic::Executable);
        // The deepest call under each function, which only comes out right if every callee already
        // has its answer by the time the caller is asked.
        let depth = graph.solve(
            |_| 0usize,
            |node, answers| {
                graph.calls(node).iter().map(|&it| answers[it.index()] + 1).max().unwrap_or(0)
            },
        );
        assert_eq!(depth[node(&graph, &mut names, "bottom").index()], 0);
        assert_eq!(depth[node(&graph, &mut names, "middle").index()], 1);
        assert_eq!(depth[node(&graph, &mut names, "top").index()], 2);
    }

    #[test]
    fn a_component_of_one_with_no_edge_to_itself_is_asked_once() {
        let (mut names, mut module) = blank();
        calling(&mut names, &mut module, "f", &["g"]);
        calling(&mut names, &mut module, "g", &[]);
        let graph = CallGraph::of(&module, Pic::Executable);
        let mut asked = 0usize;
        let answers: Vec<bool> = graph.solve(
            |_| false,
            |_, _| {
                asked += 1;
                true
            },
        );
        assert_eq!(asked, 2, "one question each, with nothing to settle");
        assert!(answers[node(&graph, &mut names, "f").index()]);
    }

    #[test]
    fn a_cycle_is_iterated_until_nothing_moves() {
        let (mut names, mut module) = blank();
        calling(&mut names, &mut module, "even", &["odd"]);
        calling(&mut names, &mut module, "odd", &["even"]);
        let graph = CallGraph::of(&module, Pic::Executable);
        // Optimistic and lowered on contradiction, which is the shape section 34.5 asks purity to
        // have. `odd` is told outright that it is not, and `even` has to find out through the cycle.
        let odd = node(&graph, &mut names, "odd");
        let settled = graph.solve(
            |_| true,
            |node, answers| node != odd && graph.calls(node).iter().all(|&it| answers[it.index()]),
        );
        assert!(!settled[odd.index()]);
        assert!(!settled[node(&graph, &mut names, "even").index()], "through the cycle");
    }

    #[test]
    fn an_empty_module_is_an_empty_graph() {
        let (_, module) = blank();
        let graph = CallGraph::of(&module, Pic::Executable);
        assert!(graph.is_empty());
        assert_eq!(graph.len(), 0);
        assert!(graph.components().is_empty());
        let answers: Vec<usize> = graph.solve(|_| 0, |_, _| 0);
        assert!(answers.is_empty());
    }

    #[test]
    fn the_graph_is_the_same_graph_every_time_it_is_built() {
        let (mut names, mut module) = blank();
        for name in ["one", "two", "three", "four", "five"] {
            calling(&mut names, &mut module, name, &["helper", "one"]);
        }
        calling(&mut names, &mut module, "helper", &[]);
        let first = CallGraph::of(&module, Pic::Executable);
        let spelling = |graph: &CallGraph| {
            graph
                .nodes()
                .map(|it| {
                    let calls: Vec<&str> =
                        graph.calls(it).iter().map(|&to| names.resolve(graph.name(to))).collect();
                    (names.resolve(graph.name(it)).to_string(), calls.join(" "))
                })
                .collect::<Vec<_>>()
        };
        for _ in 0..8 {
            let again = CallGraph::of(&module, Pic::Executable);
            assert_eq!(spelling(&first), spelling(&again));
            assert_eq!(order(&first, &names), order(&again, &names));
        }
    }

    #[test]
    fn a_chain_deeper_than_a_recursive_walk_could_manage_still_comes_out_in_order() {
        let (mut names, mut module) = blank();
        let deep = 20_000;
        for at in 0..deep {
            let next = format!("f{}", at + 1);
            calling(&mut names, &mut module, &format!("f{at}"), &[next.as_str()]);
        }
        let graph = CallGraph::of(&module, Pic::Executable);
        assert_eq!(graph.components().len(), deep + 1, "the tail name gets one of its own");
        let top = node(&graph, &mut names, "f0");
        assert_eq!(graph.component_of(top), deep, "settled last, after everything under it");
    }
}
