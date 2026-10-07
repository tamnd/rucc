//! The part of GCC's spec file language that distributions pass with `-specs=`.
//!
//! Design: `~/notes` platform/linux document 10.5. A spec file changes how the GCC driver builds
//! each command line. rucc has no spec strings of its own, but Debian, Ubuntu and Fedora pass a
//! spec file in the flags of each package build, and a build system cannot remove it. These files
//! use a small part of the language:
//!
//! ```text
//! *self_spec:
//! + %{!r:%{!fpie:%{!fPIE:%{!fpic:%{!fPIC:%{!fno-pic:-fPIE}}}}}}
//! ```
//!
//! rucc reads the sections `*self_spec:`, `*cc1_options:`, `*cpp_options:` and `*link:`. It
//! ignores `*cc1plus_options:`, because rucc is not a C++ compiler. Each value is `+` and then
//! words, `%{name:...}` and `%{!name:...}`. A name that ends in `*` matches each switch that starts with
//! it. The flags from the first three sections go on the end of the command line, and the words of
//! `*link:` go to the linker. Each other construct is an error that names the file and the line.

use std::path::Path;

/// The command line with each `-specs=` file replaced by the flags that it adds.
///
/// # Errors
///
/// A message for a file that cannot be read, or that uses a part of the language that rucc does
/// not have.
pub fn expand(args: Vec<String>) -> Result<Vec<String>, String> {
    let mut files = Vec::new();
    let mut rest = Vec::with_capacity(args.len());
    for arg in args {
        match arg.strip_prefix("-specs=").or_else(|| arg.strip_prefix("--specs=")) {
            Some(file) => files.push(file.to_owned()),
            None => rest.push(arg),
        }
    }
    if files.is_empty() {
        return Ok(rest);
    }
    let mut sections = Vec::new();
    for (index, file) in files.iter().enumerate() {
        let text =
            std::fs::read_to_string(Path::new(file)).map_err(|e| format!("-specs={file}: {e}"))?;
        let read = read(&text).map_err(|e| format!("-specs={file}: {e}"))?;
        sections.extend(read.into_iter().map(|section| Section { file: index, ..section }));
    }

    // The self specs first, in order, and each one sees the flags that the ones before it added,
    // as in gcc. Then the options for the compiler and the linker, which see all of them.
    let (own, others): (Vec<&Section>, Vec<&Section>) =
        sections.iter().partition(|section| section.kind == Kind::Own);
    let mut added = Vec::new();
    for section in own.into_iter().chain(others) {
        let at = format!("-specs={}: line {}", files[section.file], section.line);
        if let Some(plugin) = section.text.split_whitespace().find(|w| w.starts_with("-fplugin=")) {
            return Err(format!(
                "{at}: {plugin} loads a GCC plugin, and rucc cannot load one. For annobin, turn \
                 it off in the recipe with %undefine _annotated_build"
            ));
        }
        let seen: Vec<&str> = rest.iter().chain(&added).map(String::as_str).collect();
        let words = eval(&section.text, &seen).map_err(|e| format!("{at}: {e}"))?;
        match section.kind {
            Kind::Link => {
                added.extend(words.into_iter().flat_map(|word| ["-Xlinker".to_owned(), word]));
            }
            Kind::Own | Kind::Compiler => added.extend(words),
        }
    }
    rest.extend(added);
    Ok(rest)
}

/// Where the words of a section go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `*self_spec:`, flags for the driver.
    Own,
    /// `*cc1_options:` and `*cpp_options:`, flags for the compiler.
    Compiler,
    /// `*link:`, words for the linker.
    Link,
}

/// One section of a spec file that rucc uses.
#[derive(Debug)]
struct Section {
    kind: Kind,
    text: String,
    /// The index of the file in the list of `-specs=` files.
    file: usize,
    /// The line of the value, for the messages.
    line: usize,
}

/// The sections of one file. A section is a line `*name:` and then the lines of its value, up to a
/// blank line or the next section.
fn read(text: &str) -> Result<Vec<Section>, String> {
    let mut sections: Vec<Section> = Vec::new();
    // The kind of the open section, if one is open and rucc uses it, and whether its value has
    // started.
    let mut open: Option<(Option<Kind>, bool)> = None;
    for (at, line) in text.lines().enumerate() {
        let number = at + 1;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            open = None;
            continue;
        }
        if let Some(name) = trimmed.strip_prefix('*').and_then(|rest| rest.strip_suffix(':')) {
            let kind = match name {
                "self_spec" => Some(Kind::Own),
                "cc1_options" | "cpp_options" => Some(Kind::Compiler),
                "link" => Some(Kind::Link),
                "cc1plus_options" => None,
                _ => return Err(format!("line {number}: rucc does not have the spec *{name}")),
            };
            open = Some((kind, false));
            continue;
        }
        if trimmed.starts_with('%') && !trimmed.starts_with("%{") {
            return Err(format!("line {number}: rucc does not have the directive {trimmed}"));
        }
        match open {
            None => return Err(format!("line {number}: this text is not in a section")),
            Some((None, _)) => {}
            Some((Some(_), true)) => {
                let last = sections.last_mut().expect("a value has started");
                last.text.push(' ');
                last.text.push_str(trimmed);
            }
            Some((Some(kind), false)) => {
                let Some(value) = trimmed.strip_prefix('+') else {
                    return Err(format!(
                        "line {number}: this value replaces the spec of GCC, and rucc can only \
                         add to it with +"
                    ));
                };
                sections.push(Section { kind, text: value.to_owned(), file: 0, line: number });
                open = Some((Some(kind), true));
            }
        }
    }
    Ok(sections)
}

/// The words that a spec gives for the switches on the command line.
fn eval(text: &str, switches: &[&str]) -> Result<Vec<String>, String> {
    let mut out = String::new();
    expand_into(text, switches, &mut out)?;
    Ok(out.split_whitespace().map(str::to_owned).collect())
}

fn expand_into(text: &str, switches: &[&str], out: &mut String) -> Result<(), String> {
    let mut rest = text;
    while let Some(at) = rest.find('%') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(inner) = rest.strip_prefix("%{") else {
            let what: String = rest.chars().take(2).collect();
            return Err(format!("rucc does not have the spec construct {what}"));
        };
        let end = closing(inner).ok_or("a %{ has no }")?;
        let (condition, body) = inner[..end].split_once(':').ok_or_else(|| {
            format!("rucc does not have the spec construct %{{{}}}", &inner[..end])
        })?;
        let (negated, name) = match condition.strip_prefix('!') {
            Some(name) => (true, name),
            None => (false, condition),
        };
        if name.is_empty() || name.contains(['|', '&', ',', '.', ';', '%', '{']) {
            return Err(format!("rucc does not have the spec condition %{{{condition}:"));
        }
        if given(name, switches) != negated {
            out.push(' ');
            expand_into(body, switches, out)?;
            out.push(' ');
        }
        rest = &inner[end + 1..];
    }
    out.push_str(rest);
    Ok(())
}

/// The index of the `}` that closes a `%{`, in the text after it.
fn closing(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (at, c) in text.char_indices() {
        match c {
            '{' => depth += 1,
            '}' if depth == 0 => return Some(at),
            '}' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// Whether the command line has the switch: `fPIE` is `-fPIE`, and `fuse-ld*` is each switch that
/// starts with `-fuse-ld`.
fn given(name: &str, switches: &[&str]) -> bool {
    switches.iter().filter_map(|arg| arg.strip_prefix('-')).any(|switch| {
        match name.strip_suffix('*') {
            Some(prefix) => switch.starts_with(prefix),
            None => switch == name,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIE_COMPILE: &str = "*self_spec:\n+ %{!r:%{!fpie:%{!fPIE:%{!fpic:%{!fPIC:%{!fno-pic:%{!fno-PIE:%{!no-pie:-fPIE}}}}}}}}\n";
    const NO_PIE_LINK: &str = "*self_spec:\n+ %{!shared:%{!r:%{!fPIE:%{!pie:-fno-PIE -no-pie}}}}\n";
    const HARDENED_CC1: &str = "*cc1_options:\n+ %{!r:%{!fpie:%{!fPIE:%{!fpic:%{!fPIC:%{!fno-pic:-fPIE}}}}}}\n\n*cpp_options:\n+ %{!r:%{!fpie:%{!fPIE:%{!fpic:%{!fPIC:%{!fno-pic:-fPIE}}}}}}\n";
    const ANNOBIN: &str = "*cc1_options:\n+ %{!-fno-use-annobin:%{!iplugindir*:%:find-plugindir()} -fplugin=annobin}\n\n";

    fn on(text: &str, line: &[&str]) -> Result<Vec<String>, String> {
        let sections = read(text)?;
        let mut words = Vec::new();
        for section in sections {
            words.extend(eval(&section.text, line)?);
        }
        Ok(words)
    }

    #[test]
    fn the_dpkg_and_red_hat_files_give_what_gcc_gives() {
        assert_eq!(on(PIE_COMPILE, &["-c", "a.c"]).unwrap(), ["-fPIE"]);
        assert!(on(PIE_COMPILE, &["-fPIC", "-c", "a.c"]).unwrap().is_empty());
        assert!(on(PIE_COMPILE, &["-no-pie", "a.c"]).unwrap().is_empty());
        assert_eq!(on(NO_PIE_LINK, &["a.c"]).unwrap(), ["-fno-PIE", "-no-pie"]);
        assert!(on(NO_PIE_LINK, &["-shared", "a.c"]).unwrap().is_empty());
        assert_eq!(on(HARDENED_CC1, &["-c", "a.c"]).unwrap(), ["-fPIE", "-fPIE"]);
        assert!(on(HARDENED_CC1, &["-r", "a.o"]).unwrap().is_empty());
        let errors =
            "*self_spec:\n+ %{!fuse-ld*:%{!r:-Wl,--error-rwx-segments -Wl,--error-execstack}}";
        assert_eq!(on(errors, &["a.c"]).unwrap().len(), 2);
        assert!(on(errors, &["-fuse-ld=lld", "a.c"]).unwrap().is_empty());
    }

    #[test]
    fn a_construct_that_rucc_does_not_have_is_an_error_with_its_line() {
        let wrong = on(ANNOBIN, &["a.c"]).unwrap_err();
        assert!(wrong.contains("%:"), "{wrong}");
        assert!(read("%include <x>\n").unwrap_err().contains("line 1"));
        assert!(read("*startfile:\n+ crt0.o\n").unwrap_err().contains("*startfile"));
        assert!(read("*link:\n-z now\n").unwrap_err().contains("with +"));
        assert!(on("*link:\n+ %{s|S:-x}\n", &[]).unwrap_err().contains("%{s|S:"));
    }

    #[test]
    fn a_value_goes_on_to_the_next_line() {
        assert_eq!(
            on("*link:\n+ -z\nnow\n\n*cc1plus_options:\n+ -fno-rtti\n", &[]).unwrap(),
            ["-z", "now"]
        );
    }

    #[test]
    fn the_files_are_read_in_order_and_the_link_words_go_to_the_linker() {
        let dir = std::env::temp_dir().join(format!("rucc-specs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let compile = dir.join("no-pie-compile.specs");
        let link = dir.join("link.specs");
        let plugin = dir.join("annobin");
        std::fs::write(
            &compile,
            "*self_spec:\n+ %{!r:%{!fpie:%{!fPIE:%{!fpic:%{!fPIC:%{!fno-pic:-fno-PIE}}}}}}\n",
        )
        .unwrap();
        std::fs::write(&link, format!("{NO_PIE_LINK}\n*link:\n+ -z now\n")).unwrap();
        std::fs::write(&plugin, ANNOBIN.replace("%{!iplugindir*:%:find-plugindir()} ", ""))
            .unwrap();
        let line = |extra: &[&std::path::Path]| {
            let mut args = vec!["-O2".to_owned()];
            args.extend(extra.iter().map(|path| format!("-specs={}", path.display())));
            args.push("a.c".to_owned());
            expand(args)
        };
        let args = line(&[&compile, &link]).unwrap();
        assert_eq!(
            args,
            ["-O2", "a.c", "-fno-PIE", "-fno-PIE", "-no-pie", "-Xlinker", "-z", "-Xlinker", "now"]
        );
        let wrong = line(&[&plugin]).unwrap_err();
        assert!(
            wrong.contains("-fplugin=annobin") && wrong.contains("_annotated_build"),
            "{wrong}"
        );
        let missing = expand(vec!["-specs=/no/such/file".to_owned()]).unwrap_err();
        assert!(missing.starts_with("-specs=/no/such/file"), "{missing}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
