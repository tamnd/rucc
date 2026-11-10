//! A small read-only 9P2000 server of a tree in memory.
//!
//! The tests of this crate and of the driver use it to give a tree to the client with no Plan 9
//! system. It answers `version`, `attach`, `walk`, `open`, `read`, `stat`, `clunk` and `flush`,
//! and an `Rerror` with the text of the Plan 9 kernel for each other request. It serves one
//! connection on the thread that calls [`serve`], and a change to the [`Tree`] between two
//! requests changes the version of the file, as a real server does.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::sync::Mutex;

use crate::client::MSIZE;
use crate::wire::{self, NOFID, Qid, Rmsg, Stat, Tmsg, VERSION};

/// The mode bit of a directory in a `stat`.
const DMDIR: u32 = 0x8000_0000;

#[derive(Debug)]
struct Node {
    name: String,
    parent: usize,
    version: u32,
    /// The bytes of a file, or `None` for a directory.
    data: Option<Vec<u8>>,
    children: Vec<usize>,
}

/// A tree of directories and files. The root is a directory.
#[derive(Debug)]
pub struct Tree {
    nodes: Vec<Node>,
}

impl Default for Tree {
    fn default() -> Tree {
        Tree::new()
    }
}

impl Tree {
    /// A tree with only an empty root.
    #[must_use]
    pub fn new() -> Tree {
        let root =
            Node { name: "/".to_owned(), parent: 0, version: 0, data: None, children: Vec::new() };
        Tree { nodes: vec![root] }
    }

    fn child(&self, dir: usize, name: &str) -> Option<usize> {
        self.nodes[dir].children.iter().copied().find(|&c| self.nodes[c].name == name)
    }

    /// Put `bytes` in the file at `path`, and make the directories above it. A file that is
    /// there gets the new bytes and a new version.
    ///
    /// # Panics
    ///
    /// When a name on the path is a file and not a directory.
    pub fn insert(&mut self, path: &str, bytes: impl Into<Vec<u8>>) {
        let names: Vec<&str> = path.split('/').filter(|n| !n.is_empty()).collect();
        let Some((last, dirs)) = names.split_last() else { return };
        let mut at = 0;
        for name in dirs {
            at = match self.child(at, name) {
                Some(c) => c,
                None => self.add(at, name, None),
            };
            assert!(self.nodes[at].data.is_none(), "'{name}' in '{path}' is a file");
        }
        match self.child(at, last) {
            Some(c) => {
                let node = &mut self.nodes[c];
                node.data = Some(bytes.into());
                node.version += 1;
            }
            None => {
                self.add(at, last, Some(bytes.into()));
            }
        }
    }

    fn add(&mut self, parent: usize, name: &str, data: Option<Vec<u8>>) -> usize {
        let index = self.nodes.len();
        self.nodes.push(Node {
            name: name.to_owned(),
            parent,
            version: 0,
            data,
            children: Vec::new(),
        });
        self.nodes[parent].children.push(index);
        index
    }

    fn qid(&self, at: usize) -> Qid {
        let node = &self.nodes[at];
        let kind = if node.data.is_none() { Qid::DIR } else { 0 };
        Qid { kind, version: node.version, path: at as u64 }
    }

    fn stat(&self, at: usize) -> Stat {
        let node = &self.nodes[at];
        let (mode, length) = match &node.data {
            Some(data) => (0o444, data.len() as u64),
            None => (DMDIR | 0o555, 0),
        };
        let user = || "glenda".to_owned();
        Stat {
            qid: self.qid(at),
            mode,
            length,
            name: node.name.clone(),
            uid: user(),
            gid: user(),
            muid: user(),
            ..Stat::default()
        }
    }
}

/// A fid of the session: the node that it names, and whether it is open.
#[derive(Debug, Clone, Copy)]
struct Fid {
    node: usize,
    open: bool,
}

fn error(ename: &str) -> Rmsg {
    Rmsg::Error { ename: ename.to_owned() }
}

/// Serve `tree` on `stream` until the client closes it.
///
/// # Errors
///
/// When the stream fails, or a message is not 9P.
///
/// # Panics
///
/// When the lock of the tree is poisoned.
pub fn serve<S: Read + Write>(mut stream: S, tree: &Mutex<Tree>) -> io::Result<()> {
    let mut msize = MSIZE;
    let mut fids: HashMap<u32, Fid> = HashMap::new();
    loop {
        let msg = match wire::read_msg(&mut stream, msize) {
            Ok(msg) => msg,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
        let (tag, reply) = match wire::decode_t(&msg) {
            Ok((tag, Tmsg::Version { msize: asked, version })) => {
                msize = asked.min(MSIZE);
                fids.clear();
                let version = if version.starts_with(VERSION) { VERSION } else { "unknown" };
                (tag, Rmsg::Version { msize, version: version.to_owned() })
            }
            Ok((tag, t)) => {
                let tree = tree.lock().expect("the tree is not poisoned");
                (tag, answer(&tree, &mut fids, t))
            }
            // The tag is at the same place in each message.
            Err(_) => (u16::from_le_bytes([msg[5], msg[6]]), error("bad message")),
        };
        wire::write_msg(&mut stream, &wire::encode_r(tag, &reply)?)?;
    }
}

fn answer(tree: &Tree, fids: &mut HashMap<u32, Fid>, t: Tmsg) -> Rmsg {
    match t {
        Tmsg::Version { .. } => unreachable!("serve answers version"),
        Tmsg::Attach { fid, afid, .. } => {
            if afid != NOFID {
                return error("authentication not required");
            }
            if fids.contains_key(&fid) {
                return error("fid in use");
            }
            fids.insert(fid, Fid { node: 0, open: false });
            Rmsg::Attach { qid: tree.qid(0) }
        }
        Tmsg::Walk { fid, newfid, names } => {
            let Some(&from) = fids.get(&fid) else { return error("unknown fid") };
            if from.open {
                return error("walk of open file");
            }
            if newfid != fid && fids.contains_key(&newfid) {
                return error("fid in use");
            }
            let mut at = from.node;
            let mut qids = Vec::new();
            for name in &names {
                let next = if tree.nodes[at].data.is_some() {
                    None
                } else if name == ".." {
                    Some(tree.nodes[at].parent)
                } else {
                    tree.child(at, name)
                };
                let Some(next) = next else { break };
                at = next;
                qids.push(tree.qid(at));
            }
            if qids.is_empty() && !names.is_empty() {
                return error("file does not exist");
            }
            if qids.len() == names.len() {
                fids.insert(newfid, Fid { node: at, open: false });
            }
            Rmsg::Walk { qids }
        }
        Tmsg::Open { fid, mode } => {
            let Some(f) = fids.get_mut(&fid) else { return error("unknown fid") };
            if mode & 3 != wire::OREAD {
                return error("permission denied");
            }
            f.open = true;
            Rmsg::Open { qid: tree.qid(f.node), iounit: 0 }
        }
        Tmsg::Read { fid, offset, count } => {
            let Some(f) = fids.get(&fid).copied() else { return error("unknown fid") };
            if !f.open {
                return error("fid not open");
            }
            match &tree.nodes[f.node].data {
                Some(data) => {
                    let start = usize::try_from(offset).unwrap_or(usize::MAX).min(data.len());
                    let end = start.saturating_add(count as usize).min(data.len());
                    Rmsg::Read { data: data[start..end].to_vec() }
                }
                None => read_dir(tree, f.node, offset, count),
            }
        }
        Tmsg::Stat { fid } => match fids.get(&fid) {
            Some(f) => Rmsg::Stat { stat: tree.stat(f.node) },
            None => error("unknown fid"),
        },
        Tmsg::Clunk { fid } => match fids.remove(&fid) {
            Some(_) => Rmsg::Clunk,
            None => error("unknown fid"),
        },
        Tmsg::Flush { .. } => Rmsg::Flush,
    }
}

/// A read of a directory: the whole `stat` entries from `offset` that fit in `count` bytes.
fn read_dir(tree: &Tree, dir: usize, offset: u64, count: u32) -> Rmsg {
    let mut at = 0u64;
    let mut data = Vec::new();
    for &child in &tree.nodes[dir].children {
        let Ok(entry) = wire::encode_stat(&tree.stat(child)) else { return error("name too long") };
        let len = entry.len() as u64;
        if at >= offset {
            if data.len() + entry.len() > count as usize {
                break;
            }
            data.extend_from_slice(&entry);
        } else if at + len > offset {
            return error("bad offset in directory read");
        }
        at += len;
    }
    Rmsg::Read { data }
}
