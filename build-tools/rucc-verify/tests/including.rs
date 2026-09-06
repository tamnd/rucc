//! A model written on top of another one.
//!
//! Two rule sets are read against the IR: the lowering rules of `rucc-codegen` and the rewrite
//! rules of `rucc-opt`. Both of them need to be told what `add.i32` means, and telling them
//! twice would be two accounts of one IR with nothing to notice the day they disagreed. So a
//! model may include another, and this is what that is worth.

use std::fs;
use std::path::{Path, PathBuf};

use rucc_verify::Model;

/// A repository, in the only shape this needs: a workspace manifest at the top, because that is
/// what an include counts its path from, and whatever files a test asks for underneath.
fn repository(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("rucc-verify-including-{name}"));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("the root is made");
    fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").expect("a manifest");
    for (path, text) in files {
        let full = root.join(path);
        fs::create_dir_all(full.parent().expect("a directory")).expect("the directory is made");
        fs::write(&full, text).expect("the file is written");
    }
    root
}

fn open(root: &Path, path: &str) -> Model {
    match Model::open(&root.join(path)) {
        Ok(model) => model,
        Err(errors) => panic!("{}", errors[0]),
    }
}

fn refuse(root: &Path, path: &str) -> Vec<String> {
    match Model::open(&root.join(path)) {
        Ok(_) => panic!("that was supposed to be refused"),
        Err(errors) => errors.iter().map(ToString::to_string).collect(),
    }
}

/// The head the including file never mentions is there anyway, which is the whole point.
#[test]
fn a_model_can_be_written_on_top_of_another_one() {
    let root = repository(
        "plain",
        &[
            ("shared/ir.model", "(semantics (add.i32 l r) (bvadd l r))\n"),
            (
                "target/x.model",
                "(include shared/ir.model)\n(semantics (x64.add_rr_32 l r) (bvadd l r))\n",
            ),
        ],
    );
    let model = open(&root, "target/x.model");
    assert!(model.knows("add.i32"));
    assert!(model.knows("x64.add_rr_32"));
}

/// A path in an include is counted from the root of the repository, not from the directory the
/// including file sits in, so the two files above are named the same way wherever they are read
/// from. This is the same model as the test above, opened from two levels deeper.
#[test]
fn an_include_names_its_file_from_the_root_of_the_repository() {
    let root = repository(
        "deep",
        &[
            ("shared/ir.model", "(semantics (add.i32 l r) (bvadd l r))\n"),
            ("a/b/c/x.model", "(include shared/ir.model)\n"),
        ],
    );
    assert!(open(&root, "a/b/c/x.model").knows("add.i32"));
}

/// Two files giving one head a meaning is the mistake the split is meant to make impossible, so
/// it is refused rather than settled by whichever was read last.
#[test]
fn a_head_two_files_give_a_meaning_to_is_refused() {
    let root = repository(
        "twice",
        &[
            ("shared/ir.model", "(semantics (add.i32 l r) (bvadd l r))\n"),
            (
                "target/x.model",
                "(include shared/ir.model)\n(semantics (add.i32 l r) (bvsub l r))\n",
            ),
        ],
    );
    let said = refuse(&root, "target/x.model");
    assert_eq!(said.len(), 1);
    assert!(said[0].contains("`add.i32` is given a meaning here and in"), "{}", said[0]);
    assert!(said[0].contains("target/x.model"), "{}", said[0]);
}

/// A file that is not there is reported at the include that asked for it, which is the line
/// somebody can act on. Reporting it at the first line of a file that does not exist would be
/// naming a place nobody can open.
#[test]
fn an_include_of_a_file_that_is_not_there_is_reported_where_it_was_asked_for() {
    let root = repository("missing", &[("target/x.model", "\n\n(include shared/ir.model)\n")]);
    let said = refuse(&root, "target/x.model");
    assert_eq!(said.len(), 1);
    assert!(said[0].contains("x.model:3:1:"), "{}", said[0]);
    assert!(said[0].contains("ir.model cannot be read"), "{}", said[0]);
}

/// Two models including each other is a loop, and a file already read is not read again, so it
/// terminates and neither file's heads are given a meaning twice.
#[test]
fn two_models_that_include_each_other_are_read_once_each() {
    let root = repository(
        "loop",
        &[
            ("a.model", "(include b.model)\n(semantics (add.i32 l r) (bvadd l r))\n"),
            ("b.model", "(include a.model)\n(semantics (sub.i32 l r) (bvsub l r))\n"),
        ],
    );
    let model = open(&root, "a.model");
    assert!(model.knows("add.i32"));
    assert!(model.knows("sub.i32"));
}

/// An include that names nothing, or names more than one thing, is a line somebody meant
/// something by, so it is refused rather than ignored.
#[test]
fn an_include_that_does_not_name_one_file_is_refused() {
    let root = repository("shapeless", &[("x.model", "(include)\n")]);
    let said = refuse(&root, "x.model");
    assert_eq!(said.len(), 1);
    assert!(
        said[0].contains("an include names one file, from the root of the repository"),
        "{}",
        said[0]
    );
}
