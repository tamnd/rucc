# The target model

`rucc-target`'s `Triple` today is three fields: `Arch`, `Os`, `Env`. It is a good design for three targets and it cannot express the difference between the targets in document 04. This document replaces it, and it is first because every other document reads it.

## 3.1 What the current model cannot say

Read `crates/rucc-target/src/lib.rs` as it stands and list the things a user can write on a command line that it has nowhere to put:

| what the user wrote | what it means | where it goes today |
|---|---|---|
| `--target=x86_64-linux-gnu.2.28` | link against glibc 2.28's symbol set | nowhere |
| `--target=aarch64-macos.13` | deployment target macOS 13.0 | nowhere |
| `--target=armv7-linux-musleabihf` | ARMv7-A, hard float, Thumb-2 | `Arch` has no ARM32; the `hf` is dropped |
| `--target=riscv64-linux-gnu -march=rva23u64` | a named profile of ~30 extensions | nowhere |
| `-march=x86-64-v3` | a micro-architecture level | nowhere |
| `--target=aarch64-linux-android31` | API level 31 stub set | nowhere |
| `--target=arm64ec-pc-windows-msvc` | AArch64 in an x64-shaped ABI | nowhere |
| `--target=wasm32-wasip3` | a different stack-pointer ABI from wasip2 | nowhere |
| `-mabi=lp64d` on riscv64 | one of six psABIs for the same ISA | nowhere |
| `-mbig-endian` on aarch64 | endianness is a target property, not an arch one | `Arch::is_little_endian` is `const` and returns true |

Two of those are outright bugs waiting: `Arch::is_little_endian` returning a constant per architecture is false for AArch64, ARM, PowerPC, MIPS and RISC-V, all of which have both endiannesses; and `Arch::pointer_width` returning a constant is false for x32, ILP32 AArch64, RISC-V ILP32 and every CHERI mode.

The `FromStr` implementation also silently discards what it does not recognize, the loop over `rest` has a `_ => {}` arm, so `x86_64-linux-gnu.2.28` parses as `x86_64-unknown-linux-gnu` and the version is gone with no diagnostic. That is exactly the class of bug document 01.1 records against zig's `_STAT_VER`: the user asked for something specific, got something else, and was not told.

## 3.2 The shape of the replacement

Split the model in three, and make the split load-bearing:

```
        user text                     the model                    the facts
   "aarch64-macos.13"   -parse->   TargetTuple   -resolve->   TargetInfo
   (a serialization)               (an identity)              (what passes read)
```

**`TargetTuple` is the identity of a target.** Two compilations with equal tuples must produce interchangeable objects. It is `Copy`, `Ord`, hashable, and it is what a cache key is made of.

**`TargetInfo` is the derived fact table.** It already exists and its shape is right, parent document 12.1 lists what belongs in it. It gains fields, and it becomes a function of the tuple rather than something constructed alongside one.

**The text is neither.** Parsing is lossy in one direction only: every accepted string maps to exactly one tuple, and every tuple has exactly one canonical string that parses back to it. That round-trip is a property test, and it is the property the current `Display`/`FromStr` pair already tries to hold with its `none` disambiguation comment.

## 3.3 `TargetTuple`

```rust
pub struct TargetTuple {
    /// The instruction set family.
    pub arch: Arch,
    /// The sub-architecture: a baseline within the family. `V7A` for ARM, `RVA23U64`
    /// for RISC-V, `X86_64_V3` for x86-64, `None` for the family's own baseline.
    pub sub: SubArch,
    /// Byte order. Not derivable from `arch`: aarch64, arm, riscv, powerpc and mips
    /// all have both, and one of them is a real target.
    pub endian: Endian,
    /// The data model: pointer, `long` and `size_t` widths as one choice rather than
    /// three independent ones, because only a handful of combinations exist and every
    /// other combination is a bug. LP64, LLP64, ILP32, LP32, ILP32 on a 64-bit ISA.
    pub data_model: DataModel,
    /// The operating system, or `None` for freestanding.
    pub os: Os,
    /// The minimum OS version this output must run on, when the platform has the
    /// concept. macOS and Windows do; Linux does through the kernel headers.
    pub os_version: Option<Version>,
    /// The C library and the ABI variant over it.
    pub env: Env,
    /// The libc version to link against, which for glibc selects a symbol version
    /// node set and for Android selects an API level.
    pub env_version: Option<Version>,
    /// The psABI, when the (arch, os, env) triple does not determine one. RISC-V has
    /// six, ARM has soft/softfp/hard, LoongArch has three per width.
    pub abi: Abi,
    /// The container we write.
    pub object_format: ObjectFormat,
}
```

Nine fields, and the argument for each is that a target in document 04's table differs from another target in exactly that field and nothing else.

**Why `SubArch` and not a feature set.** A feature set (`+neon,+crc,+sve2`) is what `-march=` produces and it belongs in `TargetFeatures`, which is a separate structure that is *not* part of the tuple, because two objects built with different `-mtune=` are interchangeable and two objects built for different ABIs are not. The line is: **if it changes how a function is called or a struct is laid out, it is in the tuple; if it only changes which instructions come out, it is a feature.** That line has one hard case, discussed in 3.7.

**Why `data_model` rather than `pointer_width` and `long_width`.** Because the combinations are enumerable and the invalid ones are numerous. `TargetInfo` still exposes the individual widths; the tuple stores the choice.

**Why `os_version` is in the tuple.** On Darwin it changes the `LC_BUILD_VERSION` load command, which availability macros in the SDK headers read, which changes which declarations exist. On Windows it changes the subsystem version. It is not a tuning knob; it changes the program.

**Why `env_version` is in the tuple.** This is the single most valuable user-facing feature of the whole design, per document 01.7: choosing the glibc version node at compile time is how you stop needing an old machine to build for an old machine.

**What is deliberately absent.** Vendor. rucc's current code already parses and discards it with the comment "no decision in the compiler depends on it, and keeping it would invite one", and that judgement survives everything in this document. `apple`, `pc`, `unknown`, `w64`, `sony`, none of them changes an output byte. Where a vendor field appears to matter it is standing in for something else: `arm64-apple-darwin` differs from `aarch64-unknown-linux-gnu` in `os`, not in vendor.

## 3.4 Parsing, and the normalization trap

Document 01.4 records what happens to projects that treat the triple as a data structure: LLVM's `normalize()` moving an OS into the vendor field, `arm64` and `aarch64` both being canonical, `config.sub` and LLVM disagreeing, and Rust needing a `--print=llvm-target-tuple` so that two toolchains on one machine can agree what the machine is called.

The position taken here:

**We parse a superset and we canonicalize to one spelling.** Everything `config.sub` accepts, everything LLVM accepts, and the GNU multiarch spellings, all map into `TargetTuple`. `arm64` and `aarch64` are the same arch; `darwin`, `macos`, `macosx`, `ios`, `apple-ios` disambiguate into distinct `Os` values rather than collapsing as they do today (the current code maps all four to `Os::Darwin`, which is wrong the moment an iOS target exists, because the deployment-target macros and the `LC_BUILD_VERSION` platform differ).

**We never round-trip through somebody else's normalizer.** `rucc --print-target-triple` prints our canonical form. `rucc --print-llvm-triple` prints the string LLVM tools want, for the benefit of build systems that will hand it to `lld` or to a third-party assembler. Two functions, two names, no pretence that they are the same string. This is Rust's MCP 846 conclusion and it is right.

**Unrecognized components are a diagnostic, not a shrug.** The `_ => {}` arm becomes an error naming the component and listing the accepted values for its position. A user who writes `x86_64-linux-gnu.2.28` on a compiler that does not implement version selection must be told that, not silently given the default.

**Suffix versions parse as versions.** `linux-gnu.2.28`, `macos13.0`, `macosx-13.0`, `android31`, `wasip3`, `msvc19.44`, every platform spells its version differently and all of them appear in the wild. The parser has a per-`Os` version-suffix rule and it is a table, not a chain of `if`s.

## 3.5 `TargetInfo`, extended

The existing structure is the right idea and parent document 12.1 lists its contents correctly. What it gains:

**Endianness and pointer width become fields fed from the tuple**, not `const fn`s on `Arch`. This is a one-line change with a large blast radius and it is the reason this document comes before any backend work.

**A `min_version` for every predefined macro that depends on one.** `__GLIBC__`/`__GLIBC_MINOR__`, `__ANDROID_API__`, `MAC_OS_X_VERSION_MIN_REQUIRED`, `_WIN32_WINNT` defaults. rucc already generates predefined macros from the target description (parent document 00 says so and M1 delivered it), so this is new rows in an existing table.

**A `libc` descriptor** naming the header set, the stub set, the start files and the runtime the target needs, which is what documents 08 through 10 consume. It is a key into the sysroot catalogue, not a path.

**A `reserved` register set on top of `RegFile`.** x18 on Apple platforms, `$r21` on LoongArch under Linux, r9 on some ARM ABIs, x18 again on Windows AArch64 (the TEB pointer), and the frame pointer under `-fno-omit-frame-pointer` as a hard ABI requirement on Darwin rather than a preference. These are per-`(arch, os)` facts and today there is nowhere to put them.

**A `call` that keys on the tuple rather than on `arch`.** `TargetInfo::call` is already the right abstraction, parent document 12 calls it "the other half of that rule and the one with teeth", and the change is that its selection reads `abi` and `os`, not `arch` alone.

## 3.6 Features, separately

```rust
pub struct TargetFeatures {
    enabled: FeatureSet,   // a bitset over a per-arch feature enum
    tune: Option<Cpu>,     // -mtune, which changes cost models and nothing else
}
```

Three things a feature set must do:

**Resolve names in every spelling users write.** `-march=armv8.2-a+sve2`, `-march=x86-64-v3`, `-mavx2`, `-march=rva23u64`, `-mcpu=cortex-a76`, `-mcpu=native`. Named profiles and micro-architecture levels are *macros over the bitset*, expanded once at driver time, and the expansion is printable with `-print-enabled-features` so that a disagreement with GCC is one command away from being visible.

**Answer `__has_builtin`, `__builtin_cpu_supports` and the `target`/`target_clones` attributes from the same table the backend reads.** Parent document 13 already commits to this for GNU extensions; the feature table is the same discipline applied to ISA extensions. Two tables would drift, and the way that drift presents is an intrinsic header that compiles and an instruction that faults.

**Not participate in the ABI.** With one exception, in 3.7.

## 3.7 The one hard case: when a feature is an ABI

Vector arguments break the clean split, and document 01.12 records that the x86-64 psABI list is arguing about exactly this right now: whether x86-64-v5 inherits from v4, whether passing or returning a 512-bit vector should be a compile-time error, and whether the ABI should be changed so 512-bit vectors always travel in memory so AVX10-256 and AVX10-512 code can interoperate.

The same shape appears elsewhere. RISC-V's vector calling convention depends on `VLEN`. AArch64 SVE's is length-agnostic by design but SVE arguments still require the target to have SVE. ARM's soft/softfp/hard float split is entirely a feature that is an ABI, which is why it is spelled in the environment field (`gnueabihf`).

**The rule.** A feature that changes argument or return placement for a type is promoted into `abi` in the tuple. Everything else is a feature. Where a psABI has not settled the question, which is the AVX-512 case, today, in 2026, rucc takes the conservative option and says so in `--print-config`: **a vector wider than the target's ABI-guaranteed width is passed in memory, and passing one by value produces a warning naming the psABI issue.** If the committee settles it the other way we change one row in a table and bump a compatibility note. Guessing the aggressive option and being wrong is a silent ABI break with somebody else's library.

## 3.8 The layer rule, extended

Parent document 18's layer rule is what makes "the optimizer cannot see the AST" a fact about the build. The equivalent fact this document needs is **"no pass knows what architecture it is compiling for"**, and it gets the same treatment: a mechanical check in `cargo xtask`.

The check: no crate of rank ≥ 2 may `match` on `Arch`, `Os`, `Env`, `Abi` or `ObjectFormat`, or name a variant of any of them. Violations are a CI failure with the file and line. Target-specific behaviour reaches a pass as a field it reads or a rule that fires, never as a comparison it makes.

This is claim 3 of document 02 made checkable, and it is worth stating what it costs: there will be places where the honest implementation really is a two-way branch on the object format, and those go behind a method on `TargetInfo` with a name that says what the difference *is* rather than which platform has it. `info.needs_leading_underscore()` rather than `if format == MachO`. That renaming is not bureaucracy; it is the thing that makes the fifth target cheap, because the fifth target sets a boolean instead of adding an arm.

## 3.9 What CHERI would cost, recorded and not paid

Document 02.5 puts CHERI out of scope. The reason to write down what it needs is that two of the requirements are decisions the target model makes *now*, either way:

- **A pointer is not an integer of pointer width.** Purecap has 128-bit capabilities carrying 64-bit addresses plus bounds, permissions and a tag bit. Any code that assumes `pointer_width == address_width` is wrong. `DataModel` should therefore expose `address_width` and `pointer_width` as separate accessors from the start, even though every 1.0 target has them equal, because adding the distinction later is an audit of every use.
- **The data layout needs valid address spaces.** The Rust CHERIoT port needed exactly this upstream change before `riscv32cheriot-unknown-cheriotrtos` could exist.
- CHERIoT is purecap-only; Morello understands bare addresses too. So "CHERI" is not one ABI, and `Abi` would need `Hybrid`, `Purecap` and `Benchmark` variants, CheriBSD ships all three.

The decision: expose `address_width` separately, and do nothing else. Cost today is one accessor.

## 3.10 Migration

The change is mechanical and large, and it lands in one commit per stage so that each stage is bisectable:

1. `TargetTuple` added alongside `Triple`, with `From<Triple>`; nothing reads it.
2. `TargetInfo` construction moves to `TargetInfo::for_tuple`; `Triple` becomes a thin wrapper that builds a tuple with defaults.
3. Endianness, pointer width and long width become tuple-derived fields; the `const fn`s on `Arch` are deleted, which breaks every caller, which is the point.
4. The parser is rewritten with the version-suffix table and the unrecognized-component diagnostic. `--print-target-triple` and `--print-llvm-triple` both land here.
5. `Triple` is deleted. `xtask` gains the rank-2 match check.

Stage 3 is the one with the blast radius and stage 5 is the one that makes claim 3 checkable. Nothing after stage 5 is allowed to reintroduce a `const fn` on `Arch` that answers a question the tuple should.

## 3.11 What this does to compile throughput

Claim 2 of document 02 is at risk here and the mechanism is specific: `TargetInfo` fields that used to be `const fn` calls the optimizer folded become loads from a structure behind a reference.

The mitigation is that `TargetInfo` is constructed once per session, lives in `rucc-session`, and is passed by shared reference to everything, which is already how it works. A field load from a hot, immutable, cache-resident structure is not measurably different from a constant in a compiler whose inner loops are over tokens and IR nodes, and the hypothesis that it is not is testable at stage 3 of the migration with the existing benchmark. If stage 3 costs more than 1%, the finding is recorded and the fields that cost it are moved back to `const` with a per-architecture generic parameter, which is uglier and faster.

Measure at stage 3, before stages 4 and 5 are written. That is the cheapest point at which claim 2 can be falsified.
