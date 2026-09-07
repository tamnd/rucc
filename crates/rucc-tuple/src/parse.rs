//! Reading a tuple that somebody typed, pasted from a Clang invocation, or copied out of a
//! distribution's package name.
//!
//! Three rules run through all of it. Accept every spelling anyone else writes, because a user
//! pasting `x86_64-unknown-linux-gnu` from a Rust target list is not making a mistake. Emit one
//! spelling, so that the cache has one key per target. And never skip a component: the model
//! this replaces had a catch-all that dropped anything it did not recognize, which turned a
//! typo into a silently different target.

use crate::{
    Abi, Arch, DataModel, Endian, Env, Error, Os, SubArch, TargetTuple, TupleBuilder, Version,
};

/// Vendor components, which carry no information and exist to be skipped.
const VENDORS: &[&str] = &[
    "unknown",
    "pc",
    "apple",
    "w64",
    "none",
    "ibm",
    "sun",
    "suse",
    "redhat",
    "alpine",
    "poky",
    "buildroot",
    "nvidia",
    "amd",
    "intel",
    "linaro",
];

/// Read a target tuple.
///
/// # Errors
///
/// Returns the component that could not be read, or the combination that does not describe a
/// machine.
pub(crate) fn parse(input: &str) -> Result<TargetTuple, Error> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(Error::Empty);
    }

    let parts: Vec<&str> = trimmed.split('-').collect();
    if parts[0].is_empty() {
        return Err(Error::Empty);
    }

    let (arch, sub_arch, arch_endian) = parse_arch(parts[0])?;
    let mut index = 1;

    // A vendor is skipped only when the component after it is an OS. That test is what keeps
    // `arm-none-eabi` working, where `none` is the OS, at the same time as
    // `arm-none-linux-gnueabi`, where the same word is the vendor.
    if index + 1 < parts.len()
        && VENDORS.contains(&parts[index])
        && parse_os(parts[index + 1]).is_some()
    {
        index += 1;
    }

    let (os, os_version, implied_env) = match parts.get(index) {
        None => (Os::None, None, None),
        Some(component) => match parse_os(component) {
            Some(parsed) => {
                index += 1;
                parsed
            }
            None => return Err(Error::UnknownOs((*component).to_string())),
        },
    };

    let mut env = implied_env;
    let mut env_version = None;
    let mut abi = Abi::Default;
    let mut data_model = None;

    if let Some(component) = parts.get(index) {
        let parsed = parse_env(component)?;
        index += 1;
        if let Some(e) = parsed.env {
            env = Some(e);
        }
        env_version = parsed.version;
        abi = parsed.abi;
        data_model = parsed.data_model;
    }

    if index < parts.len() {
        let rest: Vec<String> = parts[index..].iter().map(|s| (*s).to_string()).collect();
        return Err(Error::Trailing(rest));
    }

    let mut builder = TupleBuilder::new(arch, os).sub_arch(sub_arch).abi(abi);
    if let Some(endian) = arch_endian {
        builder = builder.endian(endian);
    }
    if let Some(model) = data_model {
        builder = builder.data_model(model);
    }
    if let Some(version) = os_version {
        builder = builder.os_version(version);
    }
    if let Some(e) = env {
        builder = builder.env(e);
    }
    if let Some(version) = env_version {
        builder = builder.env_version(version);
    }
    builder.build()
}

/// Read the leading component, which carries the architecture and, depending on the family, the
/// baseline and the byte order as well.
fn parse_arch(s: &str) -> Result<(Arch, SubArch, Option<Endian>), Error> {
    let lower = s.to_ascii_lowercase();
    let unknown = || Error::UnknownArch(s.to_string());

    let simple = match lower.as_str() {
        "x86_64" | "amd64" | "x64" | "x86_64h" => Some((Arch::X86_64, Endian::Little)),
        "i386" | "i486" | "i586" | "i686" | "i786" | "x86" | "ia32" => {
            Some((Arch::X86, Endian::Little))
        }
        "aarch64" | "arm64" | "aarch64_32" => Some((Arch::Aarch64, Endian::Little)),
        "aarch64_be" | "aarch64be" | "arm64_be" => Some((Arch::Aarch64, Endian::Big)),
        "arm64ec" => Some((Arch::Arm64Ec, Endian::Little)),
        "riscv64" | "rv64" => Some((Arch::Riscv64, Endian::Little)),
        "riscv32" | "rv32" => Some((Arch::Riscv32, Endian::Little)),
        "loongarch64" | "la64" => Some((Arch::LoongArch64, Endian::Little)),
        "powerpc64le" | "ppc64le" => Some((Arch::PowerPc64, Endian::Little)),
        "powerpc64" | "ppc64" => Some((Arch::PowerPc64, Endian::Big)),
        "s390x" => Some((Arch::S390x, Endian::Big)),
        "wasm32" => Some((Arch::Wasm32, Endian::Little)),
        _ => None,
    };
    if let Some((arch, endian)) = simple {
        return Ok((arch, SubArch::None, Some(endian)));
    }

    let rest = lower
        .strip_prefix("armeb")
        .map(|r| (r, Endian::Big))
        .or_else(|| lower.strip_prefix("thumbeb").map(|r| (r, Endian::Big)))
        .or_else(|| lower.strip_prefix("arm").map(|r| (r, Endian::Little)))
        .or_else(|| lower.strip_prefix("thumb").map(|r| (r, Endian::Little)));

    let Some((tail, prefix_endian)) = rest else {
        return Err(unknown());
    };

    // `armv7eb` puts the byte order after the baseline; `armeb` puts it before. Both spellings
    // are in use and neither is worth arguing about.
    let (tail, endian) = match tail.strip_suffix("eb") {
        Some(stripped) => (stripped, Endian::Big),
        None => (tail, prefix_endian),
    };

    let sub_arch = match tail {
        "" => SubArch::None,
        "v5" | "v5t" | "v5te" => SubArch::ArmV5Te,
        "v6" | "v6k" | "v6kz" | "v6t2" | "v6z" => SubArch::ArmV6,
        "v6m" | "v6s-m" | "v6sm" => SubArch::ArmV6M,
        "v7" | "v7a" | "v7ve" | "v7l" => SubArch::ArmV7A,
        "v7r" => SubArch::ArmV7R,
        "v7m" => SubArch::ArmV7M,
        "v7em" | "v7e-m" => SubArch::ArmV7Em,
        "v8" | "v8a" | "v8.1a" | "v8l" => SubArch::ArmV8A,
        "v8m" | "v8m.base" | "v8m.main" | "v8.1m.main" => SubArch::ArmV8M,
        _ => return Err(unknown()),
    };

    Ok((Arch::Arm, sub_arch, Some(endian)))
}

/// Read the OS component, which may carry a version and, in the mingw spellings, an
/// environment.
fn parse_os(s: &str) -> Option<(Os, Option<Version>, Option<Env>)> {
    let lower = s.to_ascii_lowercase();

    // Fixed spellings first, because splitting a version off `mingw32` would produce an OS
    // called `mingw` at version 32.
    match lower.as_str() {
        "mingw32" | "mingw64" | "mingw" => return Some((Os::Windows, None, Some(Env::Gnu))),
        "win32" | "windows" => return Some((Os::Windows, None, None)),
        "none" | "unknown" | "freestanding" | "elf" => return Some((Os::None, None, None)),
        "wasi" => return Some((Os::Wasi, Some(Version::major(1)), None)),
        _ => {}
    }

    if let Some(preview) = lower.strip_prefix("wasip") {
        let major: u32 = preview.parse().ok()?;
        return Some((Os::Wasi, Some(Version::major(major)), None));
    }

    let (base, version_text) = split_version(&lower);
    let version = if version_text.is_empty() { None } else { parse_version(version_text) };
    if !version_text.is_empty() && version.is_none() {
        return None;
    }

    match base {
        "linux" => Some((Os::Linux, None, None)),
        "macos" | "macosx" | "osx" => Some((Os::MacOs, version, None)),
        // A darwin version is the kernel's, not the product's, and the two differ by a fixed
        // offset that changed once. Converting here rather than storing it means one version
        // scale exists inside the tuple, which is the one users of `-mmacosx-version-min` type.
        "darwin" => Some((Os::MacOs, version.and_then(darwin_to_macos), None)),
        "ios" | "iphoneos" => Some((Os::IOs, version, None)),
        "freebsd" => Some((Os::FreeBsd, None, None)),
        "netbsd" => Some((Os::NetBsd, None, None)),
        "openbsd" => Some((Os::OpenBsd, None, None)),
        "illumos" => Some((Os::Illumos, None, None)),
        _ => None,
    }
}

/// Convert a Darwin kernel version to the macOS product version it belongs to.
fn darwin_to_macos(v: Version) -> Option<Version> {
    match v.major_part() {
        major if major >= 20 => Some(Version::major(major - 9)),
        major if major >= 5 => Some(Version::new(10, major - 4)),
        _ => None,
    }
}

/// What the environment component turned out to say.
struct ParsedEnv {
    env: Option<Env>,
    version: Option<Version>,
    abi: Abi,
    data_model: Option<DataModel>,
}

/// Read the environment component, undoing the suffixes GCC fuses into it.
fn parse_env(s: &str) -> Result<ParsedEnv, Error> {
    let lower = s.to_ascii_lowercase();

    let mut abi = Abi::Default;
    let mut data_model = None;
    let mut base = lower.as_str();

    // Longest suffix first: `eabihf` also ends in `hf` and starts with `eabi`.
    if let Some(stripped) = base.strip_suffix("eabihf") {
        abi = Abi::DoubleFloat;
        base = stripped;
    } else if let Some(stripped) = base.strip_suffix("eabi") {
        abi = Abi::SoftFloat;
        base = stripped;
    } else if let Some(stripped) = base.strip_suffix("x32") {
        data_model = Some(DataModel::Ilp32On64);
        base = stripped;
    }

    let (name, version_text) = split_version(base);
    let version = if version_text.is_empty() {
        None
    } else {
        match parse_version(version_text) {
            Some(v) => Some(v),
            None => {
                return Err(Error::BadVersion {
                    component: "environment",
                    found: version_text.to_string(),
                });
            }
        }
    };

    let env = match name {
        "" | "none" | "unknown" | "elf" => None,
        "gnu" | "glibc" => Some(Env::Gnu),
        "musl" => Some(Env::Musl),
        "msvc" => Some(Env::Msvc),
        "android" | "androideabi" => Some(Env::Android),
        "sim" | "simulator" => Some(Env::Simulator),
        "macabi" | "catalyst" => Some(Env::MacAbi),
        _ => return Err(Error::UnknownEnv(s.to_string())),
    };

    Ok(ParsedEnv { env, version, abi, data_model })
}

/// Split a component into its name and its version text, where the version starts at the first
/// digit or at a dot. The separating dot, if there is one, is dropped.
fn split_version(s: &str) -> (&str, &str) {
    match s.find(|c: char| c.is_ascii_digit() || c == '.') {
        None => (s, ""),
        Some(at) => {
            let (name, rest) = s.split_at(at);
            (name, rest.strip_prefix('.').unwrap_or(rest))
        }
    }
}

/// Read one, two or three dot separated numbers. Anything else is not a version.
fn parse_version(s: &str) -> Option<Version> {
    let mut parts = s.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor = match parts.next() {
        None => return Some(Version::major(major)),
        Some(text) => text.parse::<u32>().ok()?,
    };
    let patch = match parts.next() {
        None => return Some(Version::new(major, minor)),
        Some(text) => text.parse::<u32>().ok()?,
    };
    if parts.next().is_some() {
        return None;
    }
    Some(Version::full(major, minor, patch))
}
