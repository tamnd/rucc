//! The messages of 9P2000 and their bytes.
//!
//! Each message is `size[4] type[1] tag[2]` and then the fields of its type, all little-endian.
//! A string is `len[2]` and then that many bytes of UTF-8. The manual pages in section 5 of the
//! Plan 9 manual give each message, and `intro(5)` gives the header.
//!
//! Only the messages that a reader of a file tree needs are here: `version`, `attach`, `walk`,
//! `open`, `read`, `stat`, `clunk` and `flush`, and `error` in reply. A message of another type
//! is refused with [`std::io::ErrorKind::InvalidData`].

use std::io::{self, Read, Write};

/// The tag of `Tversion`, which is outside the tags of the session.
pub const NOTAG: u16 = 0xFFFF;
/// The `afid` of an `attach` with no authentication.
pub const NOFID: u32 = 0xFFFF_FFFF;
/// The version string of the protocol.
pub const VERSION: &str = "9P2000";
/// The most names in one `walk`.
pub const MAXWELEM: usize = 16;
/// The bytes of a `read` reply that are not data: the header and the count.
pub const IOHDRSZ: u32 = 24;
/// The mode of an `open` for reading.
pub const OREAD: u8 = 0;

const TVERSION: u8 = 100;
const RVERSION: u8 = 101;
const TATTACH: u8 = 104;
const RATTACH: u8 = 105;
const RERROR: u8 = 107;
const TFLUSH: u8 = 108;
const RFLUSH: u8 = 109;
const TWALK: u8 = 110;
const RWALK: u8 = 111;
const TOPEN: u8 = 112;
const ROPEN: u8 = 113;
const TREAD: u8 = 116;
const RREAD: u8 = 117;
const TCLUNK: u8 = 120;
const RCLUNK: u8 = 121;
const TSTAT: u8 = 124;
const RSTAT: u8 = 125;

/// The identity of a file on the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Qid {
    /// The type bits. [`Qid::DIR`] is set for a directory.
    pub kind: u8,
    /// The version, which the server changes when the file changes.
    pub version: u32,
    /// The path, which is unique to the file on its server.
    pub path: u64,
}

impl Qid {
    /// The type bit of a directory.
    pub const DIR: u8 = 0x80;

    /// Whether the file is a directory.
    #[must_use]
    pub const fn is_dir(self) -> bool {
        self.kind & Qid::DIR != 0
    }
}

/// The `stat` of a file, as `stat(5)` gives it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Stat {
    /// The type of the server.
    pub kind: u16,
    /// The device of the server.
    pub dev: u32,
    /// The identity of the file.
    pub qid: Qid,
    /// The permissions, with the type bits of the qid in the high byte.
    pub mode: u32,
    /// The time of the last read.
    pub atime: u32,
    /// The time of the last write.
    pub mtime: u32,
    /// The length in bytes.
    pub length: u64,
    /// The last element of the path.
    pub name: String,
    /// The owner.
    pub uid: String,
    /// The group.
    pub gid: String,
    /// The user who changed the file last.
    pub muid: String,
}

/// A request from the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tmsg {
    /// Agree on the largest message and the version.
    Version {
        /// The largest message that the client takes.
        msize: u32,
        /// The version that the client speaks.
        version: String,
    },
    /// Give `fid` the root of the tree `aname` for the user `uname`.
    Attach {
        /// The new fid.
        fid: u32,
        /// The authentication fid, or [`NOFID`].
        afid: u32,
        /// The user.
        uname: String,
        /// The tree.
        aname: String,
    },
    /// Walk from `fid` through `names` and give the result to `newfid`.
    Walk {
        /// Where the walk starts.
        fid: u32,
        /// The fid of the result, which can be `fid`.
        newfid: u32,
        /// The names to walk, at most [`MAXWELEM`].
        names: Vec<String>,
    },
    /// Open `fid` for I/O.
    Open {
        /// The fid to open.
        fid: u32,
        /// The mode, [`OREAD`] here.
        mode: u8,
    },
    /// Read from an open fid.
    Read {
        /// The fid.
        fid: u32,
        /// Where the read starts.
        offset: u64,
        /// The most bytes to read.
        count: u32,
    },
    /// Give the `stat` of `fid`.
    Stat {
        /// The fid.
        fid: u32,
    },
    /// Forget `fid`.
    Clunk {
        /// The fid.
        fid: u32,
    },
    /// Stop the request with tag `oldtag`.
    Flush {
        /// The tag of the request.
        oldtag: u16,
    },
}

/// A reply from the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rmsg {
    /// The largest message and the version that the server agrees to.
    Version {
        /// The largest message.
        msize: u32,
        /// The version, or `unknown`.
        version: String,
    },
    /// The root of the tree.
    Attach {
        /// Its identity.
        qid: Qid,
    },
    /// The request failed.
    Error {
        /// What the server says.
        ename: String,
    },
    /// The identity of each name that the walk passed. A walk that stopped early gives fewer
    /// than it was given, and the new fid is not made.
    Walk {
        /// The identities.
        qids: Vec<Qid>,
    },
    /// The fid is open.
    Open {
        /// The identity of the file.
        qid: Qid,
        /// The most bytes that one read gives with no split, or zero.
        iounit: u32,
    },
    /// The bytes that were read. None at the end of the file.
    Read {
        /// The bytes.
        data: Vec<u8>,
    },
    /// The `stat` of the file.
    Stat {
        /// The `stat`.
        stat: Stat,
    },
    /// The fid is forgotten.
    Clunk,
    /// The request is stopped.
    Flush,
}

fn bad(what: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.into())
}

/// The bytes of a message, written field by field.
#[derive(Debug)]
struct Out(Vec<u8>);

impl Out {
    fn new(kind: u8, tag: u16) -> Out {
        let mut out = Out(vec![0; 4]);
        out.u8(kind);
        out.u16(tag);
        out
    }
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn len16(&mut self, len: usize) -> io::Result<()> {
        self.u16(u16::try_from(len).map_err(|_| bad("a field is longer than 65535 bytes"))?);
        Ok(())
    }
    fn str(&mut self, s: &str) -> io::Result<()> {
        self.len16(s.len())?;
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
    fn qid(&mut self, q: Qid) {
        self.u8(q.kind);
        self.u32(q.version);
        self.u64(q.path);
    }
    fn stat(&mut self, s: &Stat) -> io::Result<()> {
        let mut body = Out(Vec::new());
        body.u16(s.kind);
        body.u32(s.dev);
        body.qid(s.qid);
        body.u32(s.mode);
        body.u32(s.atime);
        body.u32(s.mtime);
        body.u64(s.length);
        for text in [&s.name, &s.uid, &s.gid, &s.muid] {
            body.str(text)?;
        }
        self.len16(body.0.len())?;
        self.0.extend_from_slice(&body.0);
        Ok(())
    }
    fn finish(mut self) -> io::Result<Vec<u8>> {
        let size = u32::try_from(self.0.len()).map_err(|_| bad("the message is too long"))?;
        self.0[..4].copy_from_slice(&size.to_le_bytes());
        Ok(self.0)
    }
}

/// The fields of a message, read one by one.
#[derive(Debug)]
struct In<'a>(&'a [u8]);

impl In<'_> {
    fn take(&mut self, n: usize) -> io::Result<&[u8]> {
        if self.0.len() < n {
            return Err(bad("the message ends inside a field"));
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> io::Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> io::Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> io::Result<u64> {
        let b = self.take(8)?;
        let mut a = [0; 8];
        a.copy_from_slice(b);
        Ok(u64::from_le_bytes(a))
    }
    fn str(&mut self) -> io::Result<String> {
        let len = self.u16()?;
        let b = self.take(usize::from(len))?;
        String::from_utf8(b.to_vec()).map_err(|_| bad("a string is not UTF-8"))
    }
    fn qid(&mut self) -> io::Result<Qid> {
        Ok(Qid { kind: self.u8()?, version: self.u32()?, path: self.u64()? })
    }
    fn stat(&mut self) -> io::Result<Stat> {
        let len = self.u16()?;
        let mut s = In(self.take(usize::from(len))?);
        Ok(Stat {
            kind: s.u16()?,
            dev: s.u32()?,
            qid: s.qid()?,
            mode: s.u32()?,
            atime: s.u32()?,
            mtime: s.u32()?,
            length: s.u64()?,
            name: s.str()?,
            uid: s.str()?,
            gid: s.str()?,
            muid: s.str()?,
        })
    }
    fn end(&self) -> io::Result<()> {
        if self.0.is_empty() { Ok(()) } else { Err(bad("the message has bytes after its fields")) }
    }
}

/// The bytes of the request `t` with tag `tag`.
///
/// # Errors
///
/// When a string is longer than 65535 bytes, or a walk has more than [`MAXWELEM`] names.
pub fn encode_t(tag: u16, t: &Tmsg) -> io::Result<Vec<u8>> {
    let mut out;
    match t {
        Tmsg::Version { msize, version } => {
            out = Out::new(TVERSION, tag);
            out.u32(*msize);
            out.str(version)?;
        }
        Tmsg::Attach { fid, afid, uname, aname } => {
            out = Out::new(TATTACH, tag);
            out.u32(*fid);
            out.u32(*afid);
            out.str(uname)?;
            out.str(aname)?;
        }
        Tmsg::Walk { fid, newfid, names } => {
            if names.len() > MAXWELEM {
                return Err(bad("a walk has more than 16 names"));
            }
            out = Out::new(TWALK, tag);
            out.u32(*fid);
            out.u32(*newfid);
            out.len16(names.len())?;
            for name in names {
                out.str(name)?;
            }
        }
        Tmsg::Open { fid, mode } => {
            out = Out::new(TOPEN, tag);
            out.u32(*fid);
            out.u8(*mode);
        }
        Tmsg::Read { fid, offset, count } => {
            out = Out::new(TREAD, tag);
            out.u32(*fid);
            out.u64(*offset);
            out.u32(*count);
        }
        Tmsg::Stat { fid } => {
            out = Out::new(TSTAT, tag);
            out.u32(*fid);
        }
        Tmsg::Clunk { fid } => {
            out = Out::new(TCLUNK, tag);
            out.u32(*fid);
        }
        Tmsg::Flush { oldtag } => {
            out = Out::new(TFLUSH, tag);
            out.u16(*oldtag);
        }
    }
    out.finish()
}

/// The bytes of the reply `r` with tag `tag`.
///
/// # Errors
///
/// When a string is longer than 65535 bytes.
pub fn encode_r(tag: u16, r: &Rmsg) -> io::Result<Vec<u8>> {
    let mut out;
    match r {
        Rmsg::Version { msize, version } => {
            out = Out::new(RVERSION, tag);
            out.u32(*msize);
            out.str(version)?;
        }
        Rmsg::Attach { qid } => {
            out = Out::new(RATTACH, tag);
            out.qid(*qid);
        }
        Rmsg::Error { ename } => {
            out = Out::new(RERROR, tag);
            out.str(ename)?;
        }
        Rmsg::Walk { qids } => {
            out = Out::new(RWALK, tag);
            out.len16(qids.len())?;
            for q in qids {
                out.qid(*q);
            }
        }
        Rmsg::Open { qid, iounit } => {
            out = Out::new(ROPEN, tag);
            out.qid(*qid);
            out.u32(*iounit);
        }
        Rmsg::Read { data } => {
            out = Out::new(RREAD, tag);
            out.u32(u32::try_from(data.len()).map_err(|_| bad("a read is too long"))?);
            out.0.extend_from_slice(data);
        }
        Rmsg::Stat { stat } => {
            out = Out::new(RSTAT, tag);
            // The reply holds the `stat` behind a count of its own, so the bytes of the `stat`
            // are counted twice: once here and once at its start.
            let mut body = Out(Vec::new());
            body.stat(stat)?;
            out.len16(body.0.len())?;
            out.0.extend_from_slice(&body.0);
        }
        Rmsg::Clunk => out = Out::new(RCLUNK, tag),
        Rmsg::Flush => out = Out::new(RFLUSH, tag),
    }
    out.finish()
}

/// Split a message into its type, its tag and its fields.
fn header(msg: &[u8]) -> io::Result<(u8, u16, In<'_>)> {
    let mut i = In(msg);
    let size = i.u32()?;
    if usize::try_from(size).ok() != Some(msg.len()) {
        return Err(bad("the size of the message is wrong"));
    }
    Ok((i.u8()?, i.u16()?, i))
}

/// The tag and the request in `msg`, which is one whole message.
///
/// # Errors
///
/// When the message is cut short, has bytes after its fields, or is of a type that is not here.
pub fn decode_t(msg: &[u8]) -> io::Result<(u16, Tmsg)> {
    let (kind, tag, mut i) = header(msg)?;
    let t = match kind {
        TVERSION => Tmsg::Version { msize: i.u32()?, version: i.str()? },
        TATTACH => Tmsg::Attach { fid: i.u32()?, afid: i.u32()?, uname: i.str()?, aname: i.str()? },
        TWALK => {
            let fid = i.u32()?;
            let newfid = i.u32()?;
            let n = i.u16()?;
            if usize::from(n) > MAXWELEM {
                return Err(bad("a walk has more than 16 names"));
            }
            let names = (0..n).map(|_| i.str()).collect::<io::Result<_>>()?;
            Tmsg::Walk { fid, newfid, names }
        }
        TOPEN => Tmsg::Open { fid: i.u32()?, mode: i.u8()? },
        TREAD => Tmsg::Read { fid: i.u32()?, offset: i.u64()?, count: i.u32()? },
        TSTAT => Tmsg::Stat { fid: i.u32()? },
        TCLUNK => Tmsg::Clunk { fid: i.u32()? },
        TFLUSH => Tmsg::Flush { oldtag: i.u16()? },
        other => return Err(bad(format!("request type {other} is not supported"))),
    };
    i.end()?;
    Ok((tag, t))
}

/// The tag and the reply in `msg`, which is one whole message.
///
/// # Errors
///
/// When the message is cut short, has bytes after its fields, or is of a type that is not here.
pub fn decode_r(msg: &[u8]) -> io::Result<(u16, Rmsg)> {
    let (kind, tag, mut i) = header(msg)?;
    let r = match kind {
        RVERSION => Rmsg::Version { msize: i.u32()?, version: i.str()? },
        RATTACH => Rmsg::Attach { qid: i.qid()? },
        RERROR => Rmsg::Error { ename: i.str()? },
        RWALK => {
            let n = i.u16()?;
            if usize::from(n) > MAXWELEM {
                return Err(bad("a walk reply has more than 16 qids"));
            }
            Rmsg::Walk { qids: (0..n).map(|_| i.qid()).collect::<io::Result<_>>()? }
        }
        ROPEN => Rmsg::Open { qid: i.qid()?, iounit: i.u32()? },
        RREAD => {
            let n = i.u32()?;
            let data = i.take(usize::try_from(n).map_err(|_| bad("a read is too long"))?)?;
            Rmsg::Read { data: data.to_vec() }
        }
        RSTAT => {
            let n = i.u16()?;
            let mut s = In(i.take(usize::from(n))?);
            let stat = s.stat()?;
            s.end()?;
            Rmsg::Stat { stat }
        }
        RCLUNK => Rmsg::Clunk,
        RFLUSH => Rmsg::Flush,
        other => return Err(bad(format!("reply type {other} is not supported"))),
    };
    i.end()?;
    Ok((tag, r))
}

/// The bytes of one `stat`, as a read of a directory gives it: its count and then its fields.
///
/// # Errors
///
/// When a string is longer than 65535 bytes.
pub fn encode_stat(stat: &Stat) -> io::Result<Vec<u8>> {
    let mut out = Out(Vec::new());
    out.stat(stat)?;
    Ok(out.0)
}

/// The `stat` of each file in the bytes of a directory read.
///
/// # Errors
///
/// When a `stat` is cut short or has bytes after its fields.
pub fn decode_stats(bytes: &[u8]) -> io::Result<Vec<Stat>> {
    let mut i = In(bytes);
    let mut out = Vec::new();
    while !i.0.is_empty() {
        out.push(i.stat()?);
    }
    Ok(out)
}

/// Read one whole message from `r`. A message larger than `msize` is refused.
///
/// # Errors
///
/// When the stream fails or ends, or the message is too small or too large.
pub fn read_msg(r: &mut impl Read, msize: u32) -> io::Result<Vec<u8>> {
    let mut size = [0; 4];
    r.read_exact(&mut size)?;
    let n = u32::from_le_bytes(size);
    if n < 7 || n > msize {
        return Err(bad(format!("a message of {n} bytes is outside 7 to {msize}")));
    }
    let mut msg = vec![0; usize::try_from(n).map_err(|_| bad("the message is too long"))?];
    msg[..4].copy_from_slice(&size);
    r.read_exact(&mut msg[4..])?;
    Ok(msg)
}

/// Write one whole message to `w` and flush it.
///
/// # Errors
///
/// When the stream fails.
pub fn write_msg(w: &mut impl Write, msg: &[u8]) -> io::Result<()> {
    w.write_all(msg)?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_has_the_bytes_of_the_manual() {
        let walk = Tmsg::Walk { fid: 1, newfid: 2, names: vec!["sys".into(), "include".into()] };
        let bytes = encode_t(7, &walk).unwrap();
        let want: &[u8] = &[
            31, 0, 0, 0, 110, 7, 0, 1, 0, 0, 0, 2, 0, 0, 0, 2, 0, 3, 0, b's', b'y', b's', 7, 0,
            b'i', b'n', b'c', b'l', b'u', b'd', b'e',
        ];
        assert_eq!(bytes, want);
        assert_eq!(decode_t(&bytes).unwrap(), (7, walk));
    }

    #[test]
    fn every_message_comes_back_the_same() {
        let qid = Qid { kind: Qid::DIR, version: 3, path: 0x0102_0304_0506_0708 };
        let requests = [
            Tmsg::Version { msize: 8192, version: VERSION.into() },
            Tmsg::Attach { fid: 0, afid: NOFID, uname: "glenda".into(), aname: String::new() },
            Tmsg::Open { fid: 4, mode: OREAD },
            Tmsg::Read { fid: 4, offset: 1 << 40, count: 8168 },
            Tmsg::Stat { fid: 4 },
            Tmsg::Clunk { fid: 4 },
            Tmsg::Flush { oldtag: 9 },
        ];
        for t in requests {
            assert_eq!(decode_t(&encode_t(1, &t).unwrap()).unwrap(), (1, t));
        }
        let stat =
            Stat { qid, length: 99, name: "libc.h".into(), uid: "sys".into(), ..Stat::default() };
        let replies = [
            Rmsg::Version { msize: 8192, version: VERSION.into() },
            Rmsg::Attach { qid },
            Rmsg::Error { ename: "file does not exist".into() },
            Rmsg::Walk { qids: vec![qid, qid] },
            Rmsg::Open { qid, iounit: 0 },
            Rmsg::Read { data: b"#pragma lib".to_vec() },
            Rmsg::Stat { stat },
            Rmsg::Clunk,
            Rmsg::Flush,
        ];
        for r in replies {
            assert_eq!(decode_r(&encode_r(2, &r).unwrap()).unwrap(), (2, r));
        }
    }

    #[test]
    fn a_bad_message_is_refused() {
        let mut bytes = encode_t(1, &Tmsg::Clunk { fid: 1 }).unwrap();
        bytes.push(0);
        assert!(decode_t(&bytes).is_err());
        bytes[0] += 1;
        assert!(decode_t(&bytes).unwrap_err().to_string().contains("after its fields"));
        assert!(decode_t(&bytes[..9]).is_err());
        let mut short = &[3u8, 0, 0, 0][..];
        assert!(read_msg(&mut short, 8192).is_err());
        let names = vec!["a".to_string(); 17];
        assert!(encode_t(1, &Tmsg::Walk { fid: 0, newfid: 1, names }).is_err());
    }
}
