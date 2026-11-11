//! The options gcc takes for C++ or Objective-C and not for C, which a C build passes anyway.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.1.
//!
//! A build that compiles C++ and C with one set of flags passes `-fno-rtti`, `-nostdinc++` or
//! `-std=gnu++14` to its C and assembly units too. NetBSD and FreeBSD both do. gcc takes such an
//! option on a C unit with a warning from cc1 and a zero exit status, and says nothing on a unit of
//! preprocessed assembly, because cc1 takes every C family option there. `-w` stops the warning
//! and `-Werror` makes it an error, but only when the `-Werror` comes before the option on the
//! line cc1 is given. The driver puts the options on that line in the order `-nostdinc++`, then
//! the `-W` ones as they were written, then `-std=`, then the `-f` ones, so `-nostdinc++` is never
//! made an error, a `-W` option is when `-Werror` was written before it, and the other two are
//! when the last word on `-Werror` is yes. A preprocessing run has `-std=` before the `-W` options.
//!
//! The list of options is in `data/gcc-foreign-options.txt` beside the program that made it from
//! gcc's own option files.

const LIST: &str = include_str!("../data/gcc-foreign-options.txt");

/// What gcc does with the option on a C unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// cc1 warns that the option is valid for other languages and not for C.
    Warn,
    /// The driver warns that the option is no longer supported, for every unit.
    Removed,
    /// Nothing is said.
    Ignore,
}

/// An option of the list, as gcc reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Foreign {
    /// The option as gcc names it in its warning, after it reads an alias as the option it is for.
    pub(crate) text: String,
    /// The languages gcc names in its warning, such as `C++/ObjC++`.
    pub(crate) langs: &'static str,
    pub(crate) kind: Kind,
    /// The first gcc release that has the option, or 0 for one gcc 14 has.
    pub(crate) since: u32,
}

/// One line of the list.
struct Row {
    name: &'static str,
    langs: &'static str,
    kind: &'static str,
    negative: bool,
    alias: &'static str,
    since: u32,
}

fn rows() -> impl Iterator<Item = Row> {
    LIST.lines().filter(|line| !line.is_empty() && !line.starts_with('#')).filter_map(|line| {
        let mut cols = line.split('\t');
        Some(Row {
            name: cols.next()?,
            langs: cols.next()?,
            kind: cols.next()?,
            negative: cols.next()? == "neg",
            alias: cols.next()?,
            since: cols.next()?.parse().ok()?,
        })
    })
}

/// The option `arg` is, with its dash, if it is one of the list, as gcc's own option reader
/// finds it: the whole name first, then the name with `no-` taken out of a `-fno-` or `-Wno-`
/// form, then the longest name that takes a joined value.
pub(crate) fn lookup(arg: &str) -> Option<Foreign> {
    let body = arg.strip_prefix('-')?;
    let joined = |row: &Row| row.kind == "joined";
    if let Some(row) = rows().find(|row| !joined(row) && row.name == body) {
        return Some(read(&row, arg, None, true));
    }
    let mut chars = body.chars();
    if let Some(letter @ ('f' | 'W')) = chars.next() {
        if let Some(rest) = chars.as_str().strip_prefix("no-") {
            let positive = format!("{letter}{rest}");
            if let Some(row) =
                rows().find(|row| row.negative && !joined(row) && row.name == positive)
            {
                return Some(read(&row, arg, None, false));
            }
        }
    }
    let row = rows()
        .filter(|row| joined(row) && body.starts_with(row.name))
        .max_by_key(|row| row.name.len())?;
    let value = &body[row.name.len()..];
    Some(read(&row, arg, Some(value), true))
}

/// What gcc makes of `row`, written as `arg`, with the joined value if it takes one and whether
/// it was the positive form.
fn read(row: &Row, arg: &str, value: Option<&str>, positive: bool) -> Foreign {
    let kind = match row.kind {
        "ignore" => Kind::Ignore,
        "removed" => Kind::Removed,
        _ => Kind::Warn,
    };
    let mut text = arg.to_owned();
    let mut langs = row.langs;
    if row.alias != "-" {
        let mut parts = row.alias.split(',');
        let target = parts.next().unwrap_or_default();
        let values: Vec<&str> = parts.collect();
        langs = rows().find(|other| other.name == target).map_or(langs, |other| other.langs);
        text = match (values.as_slice(), value) {
            // An alias that sets a value, which is the positive one or the negative one.
            ([on, off], _) => format!("-{target}{}", if positive { on } else { off }),
            ([on], _) => format!("-{target}{on}"),
            // An alias that takes the value it was given, such as `-ftemplate-depth-`.
            (_, Some(value)) => format!("-{target}{value}"),
            (_, None) if positive => format!("-{target}"),
            (_, None) => format!("-{}no-{}", &target[..1], &target[1..]),
        };
    }
    Foreign { text, langs, kind, since: row.since }
}

/// Where an option goes on the line the driver gives cc1, which decides whether `-Werror`
/// is in force when cc1 reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Group {
    Nostdinc,
    Warning,
    Std,
    Flag,
}

/// A foreign option the command line has, kept until the plan says what units it is for.
#[derive(Clone, Debug)]
pub(crate) struct Hit {
    pub(crate) group: Group,
    /// What cc1 says.
    pub(crate) message: String,
    /// Whether `-Werror` was in force where the option was written, which is what decides a `-W`
    /// option.
    pub(crate) werror_before: bool,
}

impl Hit {
    /// The warning for `option`, written in group `group`.
    pub(crate) fn new(group: Group, option: &Foreign, werror_before: bool) -> Hit {
        let message = format!(
            "command-line option '{}' is valid for {} but not for C",
            option.text, option.langs
        );
        Hit { group, message, werror_before }
    }

    /// The warning for `-Werror=` of a warning that C does not have, written as `arg`.
    pub(crate) fn werror(arg: &str, werror_before: bool) -> Hit {
        let message = format!("'-Werror=' argument '{arg}' is not valid for C");
        Hit { group: Group::Warning, message, werror_before }
    }

    /// Whether cc1 makes this an error, given whether it runs the compiler and the last word on
    /// `-Werror`.
    pub(crate) fn is_error(&self, compiles: bool, werror: bool) -> bool {
        match self.group {
            Group::Nostdinc => false,
            Group::Warning => self.werror_before,
            Group::Std => compiles && werror,
            Group::Flag => werror,
        }
    }
}

/// What cc1 says about `hits` on one C unit, in the order it says it, with whether it is an error.
pub(crate) fn said(hits: &[Hit], compiles: bool, werror: bool) -> Vec<(&str, bool)> {
    let place = |hit: &Hit| match (compiles, hit.group) {
        (false, Group::Std) => 1,
        (false, Group::Warning) => 2,
        (_, group) => group as u8,
    };
    let mut ordered: Vec<&Hit> = hits.iter().collect();
    ordered.sort_by_key(|hit| place(hit));
    let mut out: Vec<(&str, bool)> = Vec::new();
    for hit in ordered {
        // cc1 reads a repeated option once.
        if !out.iter().any(|(message, _)| *message == hit.message) {
            out.push((&hit.message, hit.is_error(compiles, werror)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(arg: &str) -> String {
        lookup(arg).unwrap().text
    }

    #[test]
    fn an_option_is_named_as_gcc_names_it() {
        assert_eq!(text("-fno-rtti"), "-fno-rtti");
        assert_eq!(text("-frtti"), "-frtti");
        assert_eq!(text("-nostdinc++"), "-nostdinc++");
        assert_eq!(text("-Wno-abi-tag"), "-Wno-abi-tag");
        assert_eq!(text("-std=gnu++14"), "-std=gnu++14");
        assert_eq!(lookup("-std=gnu++14").unwrap().langs, "C++/ObjC++");
        // Aliases are named as the option gcc reads them as, with gcc 16's values.
        assert_eq!(text("-std=c++1y"), "-std=c++14");
        assert_eq!(text("-ftemplate-depth-5"), "-ftemplate-depth=5");
        assert_eq!(text("-ftemplate-depth=7"), "-ftemplate-depth=7");
        assert_eq!(text("-faligned-new"), "-faligned-new=1");
        assert_eq!(text("-fno-aligned-new"), "-faligned-new=0");
        assert_eq!(text("-Wno-catch-value"), "-Wcatch-value=0");
        assert_eq!(text("-Wcatch-value"), "-Wcatch-value=1");
        assert_eq!(text("-Wno-c++0x-compat"), "-Wno-c++11-compat");
        assert_eq!(text("-Wno-overloaded-virtual"), "-Woverloaded-virtual=0");
        assert_eq!(lookup("-fall-virtual").unwrap().kind, Kind::Removed);
        assert_eq!(lookup("-fconserve-space").unwrap().kind, Kind::Ignore);
        assert_eq!(lookup("-fmodules").unwrap().since, 15);
    }

    #[test]
    fn an_option_c_has_is_not_in_the_list() {
        for arg in ["-fno-exceptions", "-fabi-version=2", "-fno-ipa-icf", "-std=gnu11", "-Wall"] {
            assert_eq!(lookup(arg), None, "{arg}");
        }
        // gcc reads `-fhandle-exceptions` as `-fexceptions`, which C has.
        assert_eq!(lookup("-fhandle-exceptions"), None);
        // A negative form gcc refuses is not one either.
        assert_eq!(lookup("-fno-nostdinc++"), None);
        assert_eq!(lookup("-std=no-c++14"), None);
    }

    #[test]
    fn werror_decides_by_where_the_option_goes() {
        let rtti = Hit::new(Group::Flag, &lookup("-fno-rtti").unwrap(), false);
        let nostdinc = Hit::new(Group::Nostdinc, &lookup("-nostdinc++").unwrap(), true);
        let std = Hit::new(Group::Std, &lookup("-std=gnu++14").unwrap(), false);
        let late = Hit::new(Group::Warning, &lookup("-Wabi-tag").unwrap(), false);
        let hits = [rtti.clone(), std.clone(), late.clone(), nostdinc, rtti];
        let compile = said(&hits, true, true);
        let words: Vec<bool> = compile.iter().map(|(_, error)| *error).collect();
        assert_eq!(words, [false, false, true, true]);
        assert!(compile[0].0.contains("'-nostdinc++'"));
        assert!(compile[1].0.contains("'-Wabi-tag'"));
        assert!(compile[2].0.contains("'-std=gnu++14'"));
        let preprocess = said(&hits, false, true);
        assert!(preprocess[1].0.contains("'-std=gnu++14'") && !preprocess[1].1);
        assert!(said(&[std, late], true, false).iter().all(|(_, error)| !error));
    }
}
