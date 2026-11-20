//! What goes wrong when a tuple is read, and what the user is told about it.

use core::fmt;

use crate::{Abi, Arch, Endian, Env, Os, SubArch};

/// A tuple that could not be read, or that was read and does not describe a machine.
///
/// Every variant names the component it is unhappy about. That is the whole design goal of this
/// type: the model this replaces had a catch-all arm that skipped anything it did not recognize,
/// so `x86_64-linux-gnu.2.28` parsed as `x86_64-linux-gnu` and the version was thrown away with
/// no diagnostic. A user who pins a glibc version and gets the host's is not going to find out
/// until the binary fails on the target machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The tuple was empty or all separators.
    Empty,
    /// The leading component is not an architecture we know.
    UnknownArch(String),
    /// The OS component is not an OS we know.
    UnknownOs(String),
    /// The environment component is not an environment we know.
    UnknownEnv(String),
    /// Components were left over after the environment was read.
    ///
    /// This is the case the old catch-all silently ate, so it is an error with the leftovers
    /// quoted rather than a shrug.
    Trailing(Vec<String>),
    /// A version was attached to a component and could not be read as one.
    BadVersion {
        /// The component the version was attached to, for the message.
        component: &'static str,
        /// What was found where a version was expected.
        found: String,
    },
    /// A sub-architecture was named that does not belong to the architecture.
    SubArchMismatch {
        /// The architecture that was named.
        arch: Arch,
        /// The baseline that does not belong to it.
        sub_arch: SubArch,
    },
    /// A byte order was named that the architecture does not have.
    EndianUnsupported {
        /// The architecture that was named.
        arch: Arch,
        /// The byte order it does not have.
        endian: Endian,
    },
    /// An environment was named that the OS does not have.
    EnvMismatch {
        /// The OS that was named.
        os: Os,
        /// The environment it does not have.
        env: Env,
    },
    /// A float ABI was named for an architecture that has one calling convention.
    AbiMismatch {
        /// The architecture that was named.
        arch: Arch,
        /// The ABI it cannot select.
        abi: Abi,
    },
    /// A version was attached to a component that does not take one.
    ///
    /// `x86_64-linux.5.15-gnu` is not a request for a kernel version, it is a mistake, and
    /// accepting it would mean the tuple carries a field nothing reads.
    VersionNotAccepted {
        /// The component the version was attached to.
        component: &'static str,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Empty => write!(f, "empty target tuple"),
            Error::UnknownArch(found) => {
                write!(f, "unknown architecture `{found}`")
            }
            Error::UnknownOs(found) => write!(f, "unknown operating system `{found}`"),
            Error::UnknownEnv(found) => write!(f, "unknown environment `{found}`"),
            Error::Trailing(rest) => {
                write!(f, "unexpected trailing component")?;
                if rest.len() > 1 {
                    f.write_str("s")?;
                }
                for (i, part) in rest.iter().enumerate() {
                    if i == 0 {
                        write!(f, " `{part}`")?;
                    } else {
                        write!(f, ", `{part}`")?;
                    }
                }
                Ok(())
            }
            Error::BadVersion { component, found } => {
                write!(f, "`{found}` is not a version for the {component} component")
            }
            Error::SubArchMismatch { arch, sub_arch } => {
                write!(f, "`{}` is not a baseline of `{arch}`", sub_arch.as_str())
            }
            Error::EndianUnsupported { arch, endian } => {
                write!(f, "`{arch}` has no {endian} endian mode")
            }
            Error::EnvMismatch { os, env } => {
                write!(f, "`{os}` has no `{env}` environment")
            }
            Error::AbiMismatch { arch, abi } => {
                write!(f, "`{arch}` has one calling convention and cannot select `{abi}`")
            }
            Error::VersionNotAccepted { component } => {
                write!(f, "the {component} component does not take a version")
            }
        }
    }
}

impl std::error::Error for Error {}
