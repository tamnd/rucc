//! A 9P2000 client over one byte stream.
//!
//! The client sends one request and waits for its reply before it sends the next, so one tag is
//! enough. A compiler reads a header, and then it reads the headers that the header includes, so
//! the order of its reads is fixed and requests in flight at the same time give it little.
//!
//! [`Client::read_file`] keeps what it read, keyed by the path and the version of the qid. A
//! server changes the version of a file when the file changes, so a walk to the file tells the
//! client whether the bytes it has are still correct, and a header that a build reads many
//! times costs one walk after the first read.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::wire::{self, IOHDRSZ, MAXWELEM, NOFID, NOTAG, OREAD, Qid, Rmsg, Stat, Tmsg, VERSION};

/// The largest message that the client asks for: 64 KiB of data and the header of a read.
pub const MSIZE: u32 = 65536 + IOHDRSZ;

/// What the client did, for `-ftime-report` and the tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// The requests sent, each of which waited for its reply.
    pub round_trips: u64,
    /// The time from each request to its reply, added up.
    pub wait: Duration,
    /// The bytes of file data received.
    pub bytes: u64,
    /// The reads of a whole file that the cache gave with no read from the server.
    pub cache_hits: u64,
}

/// A 9P2000 session with one server, with the root of one tree attached.
#[derive(Debug)]
pub struct Client<S> {
    stream: S,
    msize: u32,
    root: u32,
    next_fid: u32,
    free: Vec<u32>,
    tag: u16,
    stats: Stats,
    cache: HashMap<(u64, u32), Arc<[u8]>>,
}

/// The error of a reply that is not of the type that the request asks for.
fn unexpected(what: &str, r: &Rmsg) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("the reply to {what} is {r:?}"))
}

/// The error for the text of an `Rerror`. The texts are those of the Plan 9 kernel and of
/// `lib9p`, which most servers use.
fn server_error(ename: String) -> io::Error {
    let kind = if ename.contains("does not exist") || ename.contains("not found") {
        io::ErrorKind::NotFound
    } else if ename.contains("permission denied") {
        io::ErrorKind::PermissionDenied
    } else if ename.contains("is a directory") {
        io::ErrorKind::IsADirectory
    } else if ename.contains("not a directory") {
        io::ErrorKind::NotADirectory
    } else {
        io::ErrorKind::Other
    };
    io::Error::new(kind, ename)
}

/// The names of `path`, with the empty names and `.` removed.
fn names(path: &str) -> Vec<String> {
    path.split('/').filter(|n| !n.is_empty() && *n != ".").map(str::to_owned).collect()
}

impl<S: Read + Write> Client<S> {
    /// Agree on the version with the server on `stream` and attach the tree `aname` as `uname`.
    ///
    /// # Errors
    ///
    /// When the stream fails, the server does not speak 9P2000, or the attach is refused.
    pub fn connect(stream: S, uname: &str, aname: &str) -> io::Result<Client<S>> {
        let mut client = Client {
            stream,
            msize: MSIZE,
            root: 0,
            next_fid: 1,
            free: Vec::new(),
            tag: 0,
            stats: Stats::default(),
            cache: HashMap::new(),
        };
        let version = Tmsg::Version { msize: MSIZE, version: VERSION.to_owned() };
        match client.rpc_tag(NOTAG, &version)? {
            Rmsg::Version { msize, version } if version == VERSION && msize > IOHDRSZ => {
                client.msize = msize.min(MSIZE);
            }
            Rmsg::Version { version, .. } => {
                let text = format!("the server speaks `{version}` and not 9P2000");
                return Err(io::Error::new(io::ErrorKind::Unsupported, text));
            }
            r => return Err(unexpected("version", &r)),
        }
        let attach = Tmsg::Attach {
            fid: client.root,
            afid: NOFID,
            uname: uname.to_owned(),
            aname: aname.to_owned(),
        };
        match client.rpc(&attach)? {
            Rmsg::Attach { .. } => Ok(client),
            r => Err(unexpected("attach", &r)),
        }
    }

    /// What the client did up to now.
    #[must_use]
    pub const fn stats(&self) -> Stats {
        self.stats
    }

    /// The largest message that the server and the client agreed on.
    #[must_use]
    pub const fn msize(&self) -> u32 {
        self.msize
    }

    fn rpc(&mut self, t: &Tmsg) -> io::Result<Rmsg> {
        // NOTAG is for `version` only, so the tags of the session go round below it.
        self.tag = if self.tag >= NOTAG - 1 { 0 } else { self.tag + 1 };
        self.rpc_tag(self.tag, t)
    }

    fn rpc_tag(&mut self, tag: u16, t: &Tmsg) -> io::Result<Rmsg> {
        let start = Instant::now();
        wire::write_msg(&mut self.stream, &wire::encode_t(tag, t)?)?;
        let msg = wire::read_msg(&mut self.stream, self.msize)?;
        self.stats.wait += start.elapsed();
        self.stats.round_trips += 1;
        let (got, r) = wire::decode_r(&msg)?;
        if got != tag {
            let text = format!("the reply has tag {got} and the request has tag {tag}");
            return Err(io::Error::new(io::ErrorKind::InvalidData, text));
        }
        match r {
            Rmsg::Error { ename } => Err(server_error(ename)),
            r => Ok(r),
        }
    }

    fn new_fid(&mut self) -> u32 {
        self.free.pop().unwrap_or_else(|| {
            self.next_fid += 1;
            self.next_fid - 1
        })
    }

    /// Walk from the root to `path`, which is relative to the root, and give a new fid for it
    /// and its qid. The walk sends [`MAXWELEM`] names in each request.
    ///
    /// # Errors
    ///
    /// When a name on the path does not exist, which is [`io::ErrorKind::NotFound`], or the
    /// stream fails.
    pub fn walk(&mut self, path: &str) -> io::Result<(u32, Qid)> {
        let names = names(path);
        let newfid = self.new_fid();
        let mut fid = self.root;
        let mut qid = None;
        let mut chunks = names.chunks(MAXWELEM);
        // A walk with no names gives a copy of the root, and its qid comes from a stat.
        let first = chunks.next().unwrap_or(&[]);
        for chunk in std::iter::once(first).chain(chunks) {
            let walk = Tmsg::Walk { fid, newfid, names: chunk.to_vec() };
            let reply = self.rpc(&walk);
            let qids = match reply {
                Ok(Rmsg::Walk { qids }) => qids,
                Ok(r) => return Err(self.abandon(fid, newfid, unexpected("walk", &r))),
                Err(e) => return Err(self.abandon(fid, newfid, e)),
            };
            if qids.len() < chunk.len() {
                // The new fid is made only when the walk passes every name.
                let missing = &chunk[qids.len()];
                let e =
                    io::Error::new(io::ErrorKind::NotFound, format!("'{missing}' does not exist"));
                return Err(self.abandon(fid, newfid, e));
            }
            qid = qids.last().copied().or(qid);
            fid = newfid;
        }
        let qid = match qid {
            Some(qid) => qid,
            None => self.stat_fid(newfid)?.qid,
        };
        Ok((newfid, qid))
    }

    /// Give back `newfid` after a walk that failed. When the walk made it, it is clunked.
    fn abandon(&mut self, fid: u32, newfid: u32, e: io::Error) -> io::Error {
        if fid == newfid {
            let _ = self.clunk(newfid);
        } else {
            self.free.push(newfid);
        }
        e
    }

    /// Open `fid` for reading, and give the qid and the most bytes that one read gives.
    ///
    /// # Errors
    ///
    /// When the server refuses the open, or the stream fails.
    pub fn open(&mut self, fid: u32) -> io::Result<(Qid, u32)> {
        let max = self.msize - IOHDRSZ;
        match self.rpc(&Tmsg::Open { fid, mode: OREAD })? {
            Rmsg::Open { qid, iounit } => {
                Ok((qid, if iounit == 0 { max } else { iounit.min(max) }))
            }
            r => Err(unexpected("open", &r)),
        }
    }

    /// Read at most `count` bytes from the open `fid` at `offset`.
    ///
    /// # Errors
    ///
    /// When the server refuses the read, or the stream fails.
    pub fn read(&mut self, fid: u32, offset: u64, count: u32) -> io::Result<Vec<u8>> {
        match self.rpc(&Tmsg::Read { fid, offset, count })? {
            Rmsg::Read { data } => {
                self.stats.bytes += data.len() as u64;
                Ok(data)
            }
            r => Err(unexpected("read", &r)),
        }
    }

    /// Read the open `fid` from the start to the end.
    fn read_to_end(&mut self, fid: u32, iounit: u32) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        loop {
            let data = self.read(fid, out.len() as u64, iounit)?;
            if data.is_empty() {
                return Ok(out);
            }
            out.extend_from_slice(&data);
        }
    }

    /// Forget `fid`. The server forgets it even when it gives an error.
    ///
    /// # Errors
    ///
    /// When the server gives an error, or the stream fails.
    pub fn clunk(&mut self, fid: u32) -> io::Result<()> {
        let r = self.rpc(&Tmsg::Clunk { fid });
        self.free.push(fid);
        match r? {
            Rmsg::Clunk => Ok(()),
            r => Err(unexpected("clunk", &r)),
        }
    }

    fn stat_fid(&mut self, fid: u32) -> io::Result<Stat> {
        match self.rpc(&Tmsg::Stat { fid })? {
            Rmsg::Stat { stat } => Ok(stat),
            r => Err(unexpected("stat", &r)),
        }
    }

    /// The `stat` of `path`.
    ///
    /// # Errors
    ///
    /// When the path does not exist, or the stream fails.
    pub fn stat(&mut self, path: &str) -> io::Result<Stat> {
        let (fid, _) = self.walk(path)?;
        let stat = self.stat_fid(fid);
        self.clunk(fid)?;
        stat
    }

    /// All the bytes of the file at `path`. A file that the client read before and that did not
    /// change since comes from the cache.
    ///
    /// # Errors
    ///
    /// When the path does not exist, is a directory, which is [`io::ErrorKind::IsADirectory`],
    /// or the stream fails.
    pub fn read_file(&mut self, path: &str) -> io::Result<Arc<[u8]>> {
        let (fid, qid) = self.walk(path)?;
        if qid.is_dir() {
            self.clunk(fid)?;
            return Err(io::Error::new(
                io::ErrorKind::IsADirectory,
                format!("'{path}' is a directory"),
            ));
        }
        if let Some(bytes) = self.cache.get(&(qid.path, qid.version)).cloned() {
            self.clunk(fid)?;
            self.stats.cache_hits += 1;
            return Ok(bytes);
        }
        let bytes = self.open(fid).and_then(|(_, iounit)| self.read_to_end(fid, iounit));
        self.clunk(fid)?;
        let bytes: Arc<[u8]> = bytes?.into();
        // An older version of the file is of no more use.
        self.cache.retain(|&(path, _), _| path != qid.path);
        self.cache.insert((qid.path, qid.version), Arc::clone(&bytes));
        Ok(bytes)
    }

    /// The `stat` of each file in the directory at `path`.
    ///
    /// # Errors
    ///
    /// When the path does not exist, is not a directory, or the stream fails.
    pub fn read_dir(&mut self, path: &str) -> io::Result<Vec<Stat>> {
        let (fid, qid) = self.walk(path)?;
        if !qid.is_dir() {
            self.clunk(fid)?;
            let text = format!("'{path}' is not a directory");
            return Err(io::Error::new(io::ErrorKind::NotADirectory, text));
        }
        let bytes = self.open(fid).and_then(|(_, iounit)| self.read_to_end(fid, iounit));
        self.clunk(fid)?;
        wire::decode_stats(&bytes?)
    }
}

#[cfg(test)]
mod tests {
    use super::{names, server_error};
    use std::io::ErrorKind;

    #[test]
    fn a_path_gives_its_names() {
        assert_eq!(names("/sys//include/./libc.h"), ["sys", "include", "libc.h"]);
        assert_eq!(names("../x"), ["..", "x"]);
        assert!(names("/").is_empty());
    }

    #[test]
    fn the_text_of_an_error_gives_its_kind() {
        assert_eq!(server_error("file does not exist".into()).kind(), ErrorKind::NotFound);
        assert_eq!(server_error("'x' not found".into()).kind(), ErrorKind::NotFound);
        assert_eq!(server_error("permission denied".into()).kind(), ErrorKind::PermissionDenied);
        assert_eq!(server_error("file is a directory".into()).kind(), ErrorKind::IsADirectory);
        assert_eq!(server_error("i/o error".into()).kind(), ErrorKind::Other);
    }
}
