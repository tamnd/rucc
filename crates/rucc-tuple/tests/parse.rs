//! Reading tuples: the spellings that have to work, the spellings that have to fail, and the
//! properties that have to hold for both.

use std::str::FromStr;

use rucc_tuple::{
    Abi, Arch, DataModel, Endian, Env, Error, ObjectFormat, Os, SubArch, TargetTuple,
};

fn parse(s: &str) -> TargetTuple {
    TargetTuple::from_str(s).unwrap_or_else(|e| panic!("`{s}` should parse, got: {e}"))
}

#[test]
fn canonicalization_is_idempotent() {
    // The property that matters for the cache: whatever a user typed, formatting it once gives
    // a spelling that formats to itself. Without this a tuple is not a key.
    for spelling in [
        "x86_64-unknown-linux-gnu",
        "x86_64-linux-gnu",
        "amd64-linux-gnu",
        "aarch64-apple-darwin23",
        "aarch64-macos.14",
        "x86_64-w64-mingw32",
        "x86_64-pc-windows-msvc",
        "arm-none-eabi",
        "thumbv7m-none-eabi",
        "armv7a-unknown-linux-gnueabihf",
        "ppc64le-linux-gnu",
        "wasm32-unknown-unknown",
        "wasm32-wasi",
    ] {
        let once = parse(spelling).to_canonical_string();
        let twice = parse(&once).to_canonical_string();
        assert_eq!(once, twice, "`{spelling}` does not settle");
    }
}

#[test]
fn a_vendor_is_skipped_only_when_the_next_component_is_an_os() {
    // `none` is the vendor in one of these and the OS in the other, and both are real spellings
    // people paste. Getting it wrong makes one of them an error.
    let hosted = parse("arm-none-linux-gnueabi");
    assert_eq!(hosted.os(), Os::Linux);
    assert_eq!(hosted.env(), Env::Gnu);

    let bare = parse("arm-none-eabi");
    assert_eq!(bare.os(), Os::None);
    assert_eq!(bare.env(), Env::None);
    assert_eq!(bare.resolved_abi(), Abi::SoftFloat);
    assert_eq!(bare.to_canonical_string(), "arm-none-eabi");
}

#[test]
fn windows_is_llp64_and_everything_else_is_not() {
    assert_eq!(parse("x86_64-windows-gnu").data_model(), DataModel::Llp64);
    assert_eq!(parse("aarch64-windows-msvc").data_model(), DataModel::Llp64);
    assert_eq!(parse("x86_64-linux-gnu").data_model(), DataModel::Lp64);
    assert_eq!(parse("i686-linux-gnu").data_model(), DataModel::Ilp32);

    // The whole point of the field. Same architecture, same pointer width, different `long`.
    assert_eq!(parse("x86_64-windows-gnu").data_model().long_width(), 32);
    assert_eq!(parse("x86_64-linux-gnu").data_model().long_width(), 64);
    assert_eq!(parse("x86_64-windows-gnu").pointer_width(), 64);
}

#[test]
fn ilp32_on_a_64_bit_machine() {
    let x32 = parse("x86_64-linux-gnux32");
    assert_eq!(x32.arch(), Arch::X86_64);
    assert_eq!(x32.arch().register_width(), 64);
    assert_eq!(x32.pointer_width(), 32);
    assert_eq!(x32.env(), Env::Gnu);
    assert_eq!(x32.to_canonical_string(), "x86_64-linux-gnux32");
}

#[test]
fn byte_order_comes_from_the_tuple_and_not_from_the_architecture() {
    assert_eq!(parse("s390x-linux-gnu").endian(), Endian::Big);
    assert_eq!(parse("powerpc64le-linux-gnu").endian(), Endian::Little);
    assert_eq!(parse("powerpc64-linux-gnu").endian(), Endian::Big);
    assert_eq!(parse("aarch64_be-linux-gnu").endian(), Endian::Big);
    assert_eq!(parse("aarch64-linux-gnu").endian(), Endian::Little);
    assert!(!parse("s390x-linux-gnu").is_little_endian());
}

#[test]
fn a_glibc_version_survives_being_read() {
    // The regression this whole type exists for. The model this replaces dropped the version
    // and compiled against the host's glibc without saying so.
    let pinned = parse("x86_64-linux-gnu.2.28");
    assert_eq!(pinned.env(), Env::Gnu);
    assert_eq!(pinned.env_version().map(|v| v.to_string()), Some("2.28".to_string()));
    assert_eq!(pinned.to_canonical_string(), "x86_64-linux-gnu.2.28");
    assert_ne!(pinned, parse("x86_64-linux-gnu"));

    // LLVM has nowhere to put a glibc version, and emitting one produces a triple it reads as
    // an unknown environment. The pin stays in the canonical spelling, which is the one
    // everything of ours reads.
    assert_eq!(pinned.to_llvm_string(), "x86_64-unknown-linux-gnu");
    // An Android API level is part of the environment name in LLVM, so that one survives.
    assert_eq!(
        parse("aarch64-linux-android31").to_llvm_string(),
        "aarch64-unknown-linux-android31"
    );
}

#[test]
fn an_android_api_level_is_a_version_spelled_differently() {
    let api = parse("aarch64-linux-android31");
    assert_eq!(api.env(), Env::Android);
    assert_eq!(api.env_version().map(|v| v.major_part()), Some(31));
    assert_eq!(api.to_canonical_string(), "aarch64-linux-android31");
}

#[test]
fn a_darwin_kernel_version_becomes_the_product_version() {
    assert_eq!(parse("aarch64-apple-darwin23").to_canonical_string(), "aarch64-macos.14");
    assert_eq!(parse("x86_64-apple-darwin19").to_canonical_string(), "x86_64-macos.10.15");
    assert_eq!(parse("aarch64-macos.13").to_llvm_string(), "aarch64-apple-macosx13.0.0");
}

#[test]
fn the_arm_float_abi_is_reconstructed_on_output() {
    let hard = parse("armv7-linux-gnueabihf");
    assert_eq!(hard.arch(), Arch::Arm);
    assert_eq!(hard.sub_arch(), SubArch::ArmV7A);
    assert_eq!(hard.env(), Env::Gnu);
    assert_eq!(hard.abi(), Abi::DoubleFloat);
    assert_eq!(hard.to_canonical_string(), "armv7-linux-gnueabihf");

    let soft = parse("armv7-linux-gnueabi");
    assert_eq!(soft.abi(), Abi::SoftFloat);
    assert_eq!(soft.to_canonical_string(), "armv7-linux-gnueabi");
    assert_ne!(hard, soft);
}

#[test]
fn wasi_previews_are_different_targets() {
    let p1 = parse("wasm32-wasip1");
    let p3 = parse("wasm32-wasip3");
    assert_eq!(p1.os(), Os::Wasi);
    assert_ne!(p1, p3);
    assert_eq!(p1.object_format(), ObjectFormat::Wasm);
    assert_eq!(p1.to_llvm_string(), "wasm32-unknown-wasi");
    assert_eq!(p3.to_llvm_string(), "wasm32-unknown-wasip3");
}

#[test]
fn freestanding_takes_its_format_from_the_architecture() {
    assert_eq!(parse("x86_64-none").object_format(), ObjectFormat::Elf);
    assert_eq!(parse("wasm32-none").object_format(), ObjectFormat::Wasm);
}

#[test]
fn plain_char_is_signed_on_x86_and_not_on_arm() {
    assert!(parse("x86_64-linux-gnu").char_is_signed());
    assert!(parse("i686-linux-gnu").char_is_signed());
    assert!(parse("s390x-linux-gnu").char_is_signed());
    assert!(!parse("aarch64-linux-gnu").char_is_signed());
    assert!(!parse("riscv64-linux-gnu").char_is_signed());
    // Windows on ARM is the exception, because the Microsoft ABI says signed everywhere.
    assert!(parse("aarch64-windows-msvc").char_is_signed());
    // And Darwin is the other one, because Apple kept it signed for source compatibility with the
    // Intel Macs. These two tuples are the same architecture with opposite answers.
    assert!(parse("aarch64-macos").char_is_signed());
    assert!(!parse("aarch64-linux-gnu").char_is_signed());
}

#[test]
fn leading_underscores_follow_the_object_format() {
    assert!(!parse("x86_64-linux-gnu").leading_underscore());
    assert!(parse("aarch64-macos").leading_underscore());
    assert!(!parse("x86_64-windows-gnu").leading_underscore());
    assert!(parse("i686-windows-gnu").leading_underscore());
}

#[test]
fn nothing_is_silently_discarded() {
    // The behaviour the catch-all arm used to have. Each of these was previously accepted as
    // some other target.
    assert_eq!(
        TargetTuple::from_str("x86_64-linux-gnu-and-then-some"),
        Err(Error::Trailing(vec!["and".to_string(), "then".to_string(), "some".to_string()]))
    );
    assert_eq!(
        TargetTuple::from_str("x86_64-linux-glbic"),
        Err(Error::UnknownEnv("glbic".to_string()))
    );
    assert_eq!(
        TargetTuple::from_str("x86_65-linux-gnu"),
        Err(Error::UnknownArch("x86_65".to_string()))
    );
    assert_eq!(
        TargetTuple::from_str("x86_64-linuxx-gnu"),
        Err(Error::UnknownOs("linuxx".to_string()))
    );
    assert_eq!(TargetTuple::from_str(""), Err(Error::Empty));
}

#[test]
fn combinations_that_do_not_describe_a_machine_are_refused() {
    assert_eq!(
        TargetTuple::from_str("x86_64-macos-musl"),
        Err(Error::EnvMismatch { os: Os::MacOs, env: Env::Musl })
    );
    assert_eq!(
        TargetTuple::from_str("x86_64-linux-msvc"),
        Err(Error::EnvMismatch { os: Os::Linux, env: Env::Msvc })
    );
    // A float ABI on a machine with one calling convention is a mistake, not a configuration.
    assert_eq!(
        TargetTuple::from_str("x86_64-linux-gnueabihf"),
        Err(Error::AbiMismatch { arch: Arch::X86_64, abi: Abi::DoubleFloat })
    );
    // Solaris is excluded on principle by the matrix, so it is unknown rather than recognized.
    assert!(matches!(TargetTuple::from_str("x86_64-sun-solaris"), Err(Error::UnknownOs(_))));
}

#[test]
fn the_error_says_which_component_was_wrong() {
    let message = TargetTuple::from_str("x86_64-linux-glbic").unwrap_err().to_string();
    assert!(message.contains("glbic"), "message does not name the component: {message}");
    assert!(message.contains("environment"), "message does not say what it wanted: {message}");
}

#[test]
fn the_two_spellings_are_both_available() {
    let target = parse("aarch64-linux-gnu");
    assert_eq!(target.to_canonical_string(), "aarch64-linux-gnu");
    assert_eq!(target.to_llvm_string(), "aarch64-unknown-linux-gnu");
}
