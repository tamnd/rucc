//! Packs glibc's `abilist` files into the blobs this crate carries, one per library.
//!
//! `cargo xtask glibc-blob` runs this, for the reason the other examples give: xtask depends on no
//! crate in the workspace, so a file it produces out of this crate's code has to come out of a
//! program this crate builds.
//!
//! ```text
//! pack-glibc <abilists> <into>
//! ```
//!
//! `<abilists>` is the directory `bin/abilist` in `tamnd/rucc-cross` fills, one subdirectory per
//! architecture holding `libc.abilist`, `libm.abilist` and `librt.abilist`. `<into>` gets
//! `libc.blob`, `libm.blob` and `librt.blob`, and one line is printed per blob with its size.
//!
//! Every architecture in [`rucc_stub::glibc::ARCHITECTURES`] has to be there. A blob missing one
//! would pass every test on the machines that have the others and fail on the first link for the one
//! that is not, so a missing file stops the whole thing rather than being skipped.

use std::path::PathBuf;

use rucc_stub::abilist::{self, Exports};
use rucc_stub::{blob, glibc};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [from, into] = args.as_slice() else {
        eprintln!("pack-glibc: wants the abilists directory and a directory to write into");
        std::process::exit(2);
    };
    let from = PathBuf::from(from);
    let into = PathBuf::from(into);
    if let Err(why) = std::fs::create_dir_all(&into) {
        die(&format!("{} cannot be made: {why}", into.display()));
    }

    for library in glibc::LIBRARIES {
        let mut all: Vec<(&str, Exports)> = Vec::new();
        for &architecture in glibc::ARCHITECTURES {
            let path = from.join(architecture).join(format!("{}.abilist", library.name));
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|why| die(&format!("{} cannot be read: {why}", path.display())));
            let exports = abilist::read(&text)
                .unwrap_or_else(|why| die(&format!("{} is not an abilist: {why}", path.display())));
            all.push((architecture, exports));
        }
        let borrowed: Vec<(&str, &Exports)> = all.iter().map(|(arch, one)| (*arch, one)).collect();
        let bytes = blob::pack(&borrowed)
            .unwrap_or_else(|why| die(&format!("{} will not pack: {why}", library.name)));
        let path = into.join(format!("{}.blob", library.name));
        if let Err(why) = std::fs::write(&path, &bytes) {
            die(&format!("{} cannot be written: {why}", path.display()));
        }
        println!("{}|{}|{}", library.name, bytes.len(), path.display());
    }
}

/// Says what went wrong and stops, because every failure here means there is no blob to commit.
fn die(why: &str) -> ! {
    eprintln!("pack-glibc: {why}");
    std::process::exit(2);
}
