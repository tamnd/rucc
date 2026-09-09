//! The make rule the `-M` family writes.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.4.
//!
//! What comes out is read by `make` and not by a person, so the shape of it is not a matter of
//! taste: a build that includes the file expects the rule GCC would have written, and a name in
//! it that is escaped differently is a prerequisite `make` will look for under the wrong name.
//! Every rule here was read off gcc 16.2.0 rather than off its documentation, which is why the
//! column limit is a number and not a range.

use rucc_pp::Dependency;
use rucc_session::Deps;

/// Where a line is allowed to end, in columns.
///
/// GCC's number. It matters because the output is a file people put under version control, and a
/// compiler that wrapped at a different width would rewrite every dependency file in a tree the
/// first time it was used on one.
const WIDTH: usize = 72;

/// A file name with the characters `make` gives meaning to escaped.
///
/// Three of them and each one differently, which is `make`'s own doing rather than a scheme. A
/// space and a `#` are escaped with a backslash, because that is how a rule says the name has one
/// in it. A `$` is doubled, because a backslash in front of one means a literal backslash
/// followed by a variable reference. There is no escape that works for a newline and GCC does not
/// try to invent one.
#[must_use]
pub fn escaped(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        match ch {
            ' ' | '#' => {
                out.push('\\');
                out.push(ch);
            }
            '$' => out.push_str("$$"),
            _ => out.push(ch),
        }
    }
    out
}

/// Appends one name to a rule, wrapping the line first if it will not fit.
///
/// `column` is how far along the current line is. A name longer than the whole width goes on a
/// line of its own and overruns it, which is the only thing that can be done with it and is what
/// GCC does: a path is not something that can be broken in half.
fn write_name(out: &mut String, column: &mut usize, name: &str) {
    let width = name.chars().count();
    if *column > 0 && *column + 1 + width > WIDTH {
        out.push_str(" \\\n ");
        *column = 1;
    } else if *column > 0 {
        out.push(' ');
        *column += 1;
    }
    out.push_str(name);
    *column += width;
}

/// The name to the left of the colon when `-MT` and `-MQ` said nothing.
///
/// `output` is the `-o` argument when it names something this compilation produces, which is the
/// object under `-c` and the program under a link, and `None` when there is no `-o` or when the
/// `-o` names where the rule itself goes. GCC uses it exactly as written when it is there, so a
/// `-o build/foo.o` gives a rule about `build/foo.o` and a build that puts its objects in a
/// directory gets rules that name them.
///
/// Without one the answer is the source with its directory taken off and its suffix replaced,
/// which is where `a.o` comes from for `sub/a.c`. That is the only place in the family where a
/// path is shortened, and it is GCC's rule rather than a good one: it is what an unadorned
/// `make` would have named the object, from the days when everything was built in one directory.
#[must_use]
pub fn default_target(source: &str, output: Option<&str>) -> String {
    if let Some(name) = output {
        return escaped(name);
    }
    escaped(&with_suffix(base_name(source), "o"))
}

/// Where the rule goes when `-MF` did not say, or `None` for standard output.
///
/// Standard output is the answer when the rule replaces the compilation, since there is nothing
/// else the run produces. Otherwise it is the output file with its suffix replaced by `.d`, and
/// the same fallback to the source's base name that the target uses when there is no `-o`.
#[must_use]
pub fn default_file(opts: &Deps, source: &str, output: Option<&str>) -> Option<String> {
    if let Some(name) = &opts.file {
        return Some(name.clone());
    }
    if opts.instead_of_compiling {
        return None;
    }
    Some(match output {
        Some(name) => with_suffix(name, "d"),
        None => with_suffix(base_name(source), "d"),
    })
}

/// A path with everything after its last dot replaced, or with the suffix added when it has none.
///
/// The dot is looked for after the last separator, so a directory with a dot in its name does not
/// swallow the file's own suffix. `objnoext` becomes `objnoext.d`, which is what gcc writes.
fn with_suffix(name: &str, suffix: &str) -> String {
    let start = name.rfind(['/', '\\']).map_or(0, |at| at + 1);
    match name[start..].rfind('.') {
        Some(dot) => format!("{}{suffix}", &name[..start + dot + 1]),
        None => format!("{name}.{suffix}"),
    }
}

/// A path with its directory components taken off.
///
/// Both separators, because a command line on Windows may use either and a rule naming
/// `sub\a.o` when the build expected `a.o` is a rule that never fires.
fn base_name(name: &str) -> &str {
    match name.rfind(['/', '\\']) {
        Some(at) => &name[at + 1..],
        None => name,
    }
}

/// The rule for one source file, as the text to write.
///
/// `targets` is what goes to the left of the colon, already escaped by whichever of `-MT` and
/// `-MQ` put it there. `source` and each path in `found` are escaped here, since nobody has had a
/// chance to.
#[must_use]
pub fn rule(opts: &Deps, targets: &[String], source: &str, found: &[Dependency]) -> String {
    let listed: Vec<String> = found
        .iter()
        .filter(|dep| opts.system_headers || !dep.is_system)
        .map(|dep| escaped(&dep.path.to_string_lossy()))
        .collect();

    let mut out = String::new();
    let mut column = 0;
    for (at, target) in targets.iter().enumerate() {
        // The separator between two targets is a space and nothing else, and the colon goes
        // after the last of them. A rule with two targets is how a build asks for the object and
        // the dependency file both to be remade when a header changes.
        if at > 0 {
            out.push(' ');
            column += 1;
        }
        out.push_str(target);
        column += target.chars().count();
    }
    out.push(':');
    column += 1;
    write_name(&mut out, &mut column, &escaped(source));
    for name in &listed {
        write_name(&mut out, &mut column, name);
    }
    out.push('\n');

    // `-MP`. Each prerequisite becomes a target of its own that is already up to date, which is
    // what keeps `make` from stopping on a header that has been deleted rather than changed. The
    // source is left out because a missing source is a real failure and hiding it would be a
    // build that quietly compiles nothing.
    if opts.phony {
        for name in &listed {
            out.push_str(name);
            out.push_str(":\n");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn dep(path: &str, is_system: bool) -> Dependency {
        Dependency { path: PathBuf::from(path), is_system }
    }

    fn plain() -> Deps {
        Deps { emit: true, system_headers: true, ..Deps::default() }
    }

    #[test]
    fn the_source_is_the_first_prerequisite_and_the_headers_follow_it() {
        let found = [dep("sub/loc.h", false), dep("sub/deep.h", false)];
        let text = rule(&plain(), &["a.o".to_owned()], "sub/a.c", &found);
        assert_eq!(text, "a.o: sub/a.c sub/loc.h sub/deep.h\n");
    }

    #[test]
    fn a_system_header_is_dropped_when_the_flag_said_to_drop_it() {
        let found = [dep("/usr/include/stdio.h", true), dep("sub/loc.h", false)];
        let opts = Deps { emit: true, system_headers: false, ..Deps::default() };
        assert_eq!(rule(&opts, &["a.o".to_owned()], "a.c", &found), "a.o: a.c sub/loc.h\n");
        assert_eq!(
            rule(&plain(), &["a.o".to_owned()], "a.c", &found),
            "a.o: a.c /usr/include/stdio.h sub/loc.h\n"
        );
    }

    /// The three characters and the two ways of escaping them, checked against what gcc 16.2.0
    /// writes for a file whose name holds each one.
    #[test]
    fn the_characters_make_reads_are_escaped_the_way_make_reads_them() {
        let found = [dep("sp ace.h", false), dep("ha#sh.h", false), dep("dol$lar.h", false)];
        let text = rule(&plain(), &["q.o".to_owned()], "q.c", &found);
        assert_eq!(text, "q.o: q.c sp\\ ace.h ha\\#sh.h dol$$lar.h\n");
    }

    /// A line ends at the width and the next one starts with a space, which is what makes the
    /// continuation a separator as well as a wrap. The names here are the ones the probe against
    /// gcc used, so the break lands in the same place.
    #[test]
    fn a_long_rule_wraps_where_gcc_wraps_it() {
        let found = [
            dep("/usr/include/stdc-predef.h", true),
            dep("/usr/include/stdio.h", true),
            dep("/usr/include/x86_64-linux-gnu/bits/libc-header-start.h", true),
            dep("/usr/include/features.h", true),
        ];
        let text = rule(&plain(), &["a.o".to_owned()], "sub/a.c", &found);
        assert_eq!(
            text,
            "a.o: sub/a.c /usr/include/stdc-predef.h /usr/include/stdio.h \\\n \
             /usr/include/x86_64-linux-gnu/bits/libc-header-start.h \\\n \
             /usr/include/features.h\n"
        );
        for line in text.lines() {
            assert!(line.chars().count() <= WIDTH + 2, "{line} is wider than the wrap allows");
        }
    }

    /// A name with no room to wrap into. Nothing can be done with it and the line is over the
    /// width, which is better than a rule naming half a path.
    #[test]
    fn a_name_wider_than_the_line_is_written_anyway() {
        let long = format!("/{}.h", "d".repeat(WIDTH * 2));
        let found = [dep(&long, false)];
        let text = rule(&plain(), &["a.o".to_owned()], "a.c", &found);
        assert_eq!(text, format!("a.o: a.c \\\n {long}\n"));
    }

    #[test]
    fn a_phony_target_is_added_for_every_prerequisite_except_the_source() {
        let found = [dep("loc.h", false), dep("/usr/include/stdio.h", true)];
        let opts = Deps { emit: true, system_headers: false, phony: true, ..Deps::default() };
        assert_eq!(rule(&opts, &["a.o".to_owned()], "a.c", &found), "a.o: a.c loc.h\nloc.h:\n");
    }

    /// Nothing but the source, which is the case where `-MP` has nothing to say. GCC writes the
    /// rule and stops, and a build that got a stray empty target here would be reading a rule
    /// for a file that does exist.
    #[test]
    fn a_file_that_includes_nothing_gets_no_phony_targets() {
        let opts = Deps { emit: true, phony: true, ..Deps::default() };
        assert_eq!(rule(&opts, &["n.o".to_owned()], "n.c", &[]), "n.o: n.c\n");
    }

    #[test]
    fn more_than_one_target_shares_the_one_colon() {
        let text = rule(&plain(), &["one".to_owned(), "two".to_owned()], "a.c", &[]);
        assert_eq!(text, "one two: a.c\n");
    }

    /// Every one of these was read off gcc 16.2.0 rather than off its manual, because the manual
    /// says the driver works the name out and does not say from which of the two names.
    #[test]
    fn the_target_is_the_output_file_where_there_is_one_and_the_source_otherwise() {
        assert_eq!(default_target("sub/a.c", None), "a.o");
        assert_eq!(default_target("sub/a.c", Some("sub/obj.o")), "sub/obj.o");
        assert_eq!(default_target("sub/a.c", Some("prog")), "prog");
        assert_eq!(default_target("a.c", Some("out dir/a.o")), "out\\ dir/a.o");
    }

    #[test]
    fn the_rule_goes_beside_the_output_unless_it_replaces_the_compilation() {
        let write = Deps { emit: true, ..Deps::default() };
        assert_eq!(default_file(&write, "sub/a.c", None).as_deref(), Some("a.d"));
        assert_eq!(
            default_file(&write, "sub/a.c", Some("sub/obj.o")).as_deref(),
            Some("sub/obj.d")
        );
        assert_eq!(
            default_file(&write, "sub/a.c", Some("objnoext")).as_deref(),
            Some("objnoext.d")
        );

        let print = Deps { emit: true, instead_of_compiling: true, ..Deps::default() };
        assert_eq!(default_file(&print, "sub/a.c", None), None);

        // `-MF` beats both, and it is the one way to write the rule of a `-M` to a file while
        // still having the run stop after it.
        let named = Deps { file: Some("named.dep".to_owned()), ..print.clone() };
        assert_eq!(default_file(&named, "sub/a.c", None).as_deref(), Some("named.dep"));
    }

    /// A directory whose name has a dot in it does not take the file's suffix with it, and a
    /// file with no suffix at all gets one rather than losing its name.
    #[test]
    fn a_suffix_is_replaced_only_where_the_last_component_has_one() {
        assert_eq!(with_suffix("a.c", "d"), "a.d");
        assert_eq!(with_suffix("dir.v2/a", "d"), "dir.v2/a.d");
        assert_eq!(with_suffix("dir.v2/a.c", "d"), "dir.v2/a.d");
        assert_eq!(with_suffix("a.tar.gz", "d"), "a.tar.d");
    }
}
