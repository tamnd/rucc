//! Which functions a pass is allowed to run on.
//!
//! Section 41.6 of `spec/optimizer/41-correctness.md` asks for `-fdisable-<pass>[=<range>]` and
//! `-fenable-<pass>[=<range>]` from the point at which there is more than one pass, and gives the
//! reason. A wrong-code bug is two questions, which pass did it and which function did it happen
//! in, and with these two flags they are two independent bisections a script can run without a
//! debugger and without reading a diff of two assembly listings. `-fpass-fuel` narrows the first
//! answer further, to one rewrite inside the guilty pass, so the three of them together take a
//! report of the form "this program is wrong at `-O2`" down to a line of the optimizer.
//!
//! A rule applies only to the functions it names, and the last rule that names a function is the
//! one that decides for it. Everything the rules do not name keeps the answer the optimization
//! level already gave, which is what makes `-fenable-<pass>=3` mean "also run it there" rather
//! than "run it only there". GCC's `override_gate_status` works the same way and the flags are
//! useless if they do not, because a bisection that changes two things at once has bisected
//! nothing.

use crate::pass;

/// The rules `-fdisable-<pass>` and `-fenable-<pass>` left behind, in the order they were given.
///
/// Empty by default, and an empty set of gates answers yes to everything, so the cost of the
/// feature on a compilation nobody is debugging is one test of a `Vec` for emptiness per pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Gates {
    rules: Vec<Rule>,
}

/// One `-fdisable-` or `-fenable-`, remembered as what it said rather than as its effect, because
/// the effect depends on the function being asked about.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    /// The pass it names, which is checked against the pass list when the rule is added.
    pass: String,
    /// Whether it turns the pass on or off for what it covers.
    on: bool,
    /// Which functions it covers.
    scope: Scope,
}

/// What a rule was written against.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Scope {
    /// The flag had no `=`, so it covers every function in the module.
    Everything,
    /// The flag had a range list, so it covers what the list picks out.
    These(Vec<Pick>),
}

/// One item of a range list.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pick {
    /// One function, by the position it has in the module, counting from zero.
    Id(u32),
    /// Every function whose position is between these two, both ends included.
    Span(u32, u32),
    /// One function, by the name it has in the source.
    Name(String),
}

impl Pick {
    /// Whether this item picks out that function.
    fn covers(&self, id: u32, name: &str) -> bool {
        match self {
            Pick::Id(want) => *want == id,
            Pick::Span(low, high) => (*low..=*high).contains(&id),
            Pick::Name(want) => want == name,
        }
    }
}

impl Scope {
    /// Whether this scope covers that function.
    fn covers(&self, id: u32, name: &str) -> bool {
        match self {
            Scope::Everything => true,
            Scope::These(picks) => picks.iter().any(|pick| pick.covers(id, name)),
        }
    }
}

impl Gates {
    /// Adds one `-fdisable-<pass>[=<range>]` or `-fenable-<pass>[=<range>]`, with `on` saying
    /// which of the two it was and `spec` being everything after the second hyphen.
    ///
    /// # Errors
    ///
    /// When the pass is not one this compiler has, when the range list is empty, when an item of
    /// it is empty, or when a span runs backwards. A misspelled pass name is the error worth
    /// catching here: it would otherwise look exactly like a pass that is not guilty, and the
    /// bisection would carry on past the one thing it was looking for.
    pub fn add(&mut self, on: bool, spec: &str) -> Result<(), String> {
        let (name, list) = match spec.split_once('=') {
            Some((name, list)) => (name, Some(list)),
            None => (spec, None),
        };
        if pass::find(name).is_none() {
            return Err(format!("`{name}` is not a pass this compiler has, see --print-pipeline"));
        }
        let scope = match list {
            None => Scope::Everything,
            Some(list) => Scope::These(picks(list)?),
        };
        self.rules.push(Rule { pass: name.to_owned(), on, scope });
        Ok(())
    }

    /// Whether anything was asked for at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Whether that pass runs over that function, where `default` is what the optimization level
    /// already decided about the pass.
    ///
    /// The function is identified both ways at once because both spellings are useful and neither
    /// is available in both places. A script bisecting a file it has never read counts functions
    /// and gives numbers. A person who has just read `-fopt-info` gives the name it printed.
    #[must_use]
    pub fn allows(&self, pass: &str, default: bool, id: u32, name: &str) -> bool {
        let mut answer = default;
        for rule in &self.rules {
            if rule.pass == pass && rule.scope.covers(id, name) {
                answer = rule.on;
            }
        }
        answer
    }

    /// Every pass some rule turns on, in the order the rules were given, without repeats.
    ///
    /// A pass named by `-fenable-` that the level did not choose has to join the pipeline, or the
    /// flag would be a way of asking for something and being given nothing. That is also how the
    /// flag reaches a pass at `-O0`, which is where a bisection would rather start.
    #[must_use]
    pub fn enabled(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for rule in self.rules.iter().filter(|rule| rule.on) {
            let name = rule.pass.as_str();
            if !out.contains(&name) {
                out.push(name);
            }
        }
        out
    }

    /// What `--print-pipeline` says after a pass a rule mentions, or nothing when no rule does.
    ///
    /// The listing is the answer to why a program came out the way it did, and a pass that is in
    /// the list and did not run on the function being asked about is exactly the kind of thing
    /// that answer has to include.
    #[must_use]
    pub fn note(&self, pass: &str) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        for rule in self.rules.iter().filter(|rule| rule.pass == pass) {
            let word = if rule.on { "on" } else { "off" };
            parts.push(match &rule.scope {
                Scope::Everything => word.to_owned(),
                Scope::These(picks) => format!("{word} for {}", render(picks)),
            });
        }
        match parts.is_empty() {
            true => None,
            false => Some(parts.join(", ")),
        }
    }
}

/// The items of a range list, in the order they were written.
fn picks(list: &str) -> Result<Vec<Pick>, String> {
    if list.is_empty() {
        return Err("the list of functions after the `=` is empty".to_owned());
    }
    let mut out = Vec::new();
    for item in list.split(',') {
        out.push(pick(item)?);
    }
    Ok(out)
}

/// One item of a range list.
///
/// An item that starts with a digit is a number or a span of them, and anything else is a name,
/// which is unambiguous because no identifier in C starts with a digit.
fn pick(item: &str) -> Result<Pick, String> {
    if item.is_empty() {
        return Err("there is an empty item in the list of functions".to_owned());
    }
    if !item.starts_with(|c: char| c.is_ascii_digit()) {
        return Ok(Pick::Name(item.to_owned()));
    }
    let Some((low, high)) = item.split_once('-') else {
        return Ok(Pick::Id(number(item)?));
    };
    let (low, high) = (number(low)?, number(high)?);
    if low > high {
        return Err(format!("the range `{item}` ends before it starts"));
    }
    Ok(Pick::Span(low, high))
}

/// One function number.
fn number(text: &str) -> Result<u32, String> {
    text.parse().map_err(|_| format!("`{text}` is not the number of a function"))
}

/// A range list written back out, for the pipeline listing.
fn render(picks: &[Pick]) -> String {
    let parts: Vec<String> = picks
        .iter()
        .map(|pick| match pick {
            Pick::Id(id) => id.to_string(),
            Pick::Span(low, high) => format!("{low}-{high}"),
            Pick::Name(name) => name.clone(),
        })
        .collect();
    parts.join(",")
}

#[cfg(test)]
mod tests {
    use super::Gates;

    /// Gates that say what these flags said, or the reason they could not be added.
    fn gates(flags: &[(bool, &str)]) -> Gates {
        let mut gates = Gates::default();
        for (on, spec) in flags {
            gates.add(*on, spec).expect("the test asked for a gate this compiler refuses");
        }
        gates
    }

    #[test]
    fn nothing_asked_for_means_the_level_decides() {
        let gates = Gates::default();
        assert!(gates.is_empty());
        assert!(gates.allows("fold", true, 0, "main"));
        assert!(!gates.allows("fold", false, 0, "main"));
        assert_eq!(gates.note("fold"), None);
    }

    #[test]
    fn disabling_a_pass_with_no_range_takes_it_away_from_every_function() {
        let gates = gates(&[(false, "fold")]);
        assert!(!gates.allows("fold", true, 0, "main"));
        assert!(!gates.allows("fold", true, 7, "other"));
        assert!(gates.allows("dce", true, 0, "main"), "one pass named is not every pass named");
    }

    #[test]
    fn a_range_leaves_every_function_it_does_not_name_alone() {
        let gates = gates(&[(false, "fold=1-3")]);
        assert!(gates.allows("fold", true, 0, "a"));
        assert!(!gates.allows("fold", true, 1, "b"));
        assert!(!gates.allows("fold", true, 3, "d"));
        assert!(gates.allows("fold", true, 4, "e"));
    }

    #[test]
    fn a_function_can_be_named_as_well_as_numbered() {
        let gates = gates(&[(false, "dce=parse_line,9")]);
        assert!(!gates.allows("dce", true, 0, "parse_line"));
        assert!(!gates.allows("dce", true, 9, "whatever"));
        assert!(gates.allows("dce", true, 0, "main"));
    }

    #[test]
    fn the_last_rule_that_covers_a_function_is_the_one_that_decides() {
        let gates = gates(&[(false, "fold"), (true, "fold=2")]);
        assert!(!gates.allows("fold", true, 1, "a"));
        assert!(gates.allows("fold", true, 2, "b"), "the second rule covers this one");
    }

    #[test]
    fn enabling_a_pass_reaches_one_the_level_did_not_choose() {
        let gates = gates(&[(true, "narrow=2")]);
        assert!(!gates.allows("narrow", false, 1, "a"));
        assert!(gates.allows("narrow", false, 2, "b"));
        assert_eq!(gates.enabled(), ["narrow"]);
    }

    #[test]
    fn a_pass_enabled_twice_is_named_once() {
        let gates = gates(&[(true, "narrow=2"), (false, "fold"), (true, "narrow=5")]);
        assert_eq!(gates.enabled(), ["narrow"]);
    }

    #[test]
    fn the_listing_says_what_was_asked_for() {
        let gates = gates(&[(false, "fold"), (true, "fold=2-4,main")]);
        assert_eq!(gates.note("fold").as_deref(), Some("off, on for 2-4,main"));
        assert_eq!(gates.note("dce"), None);
    }

    #[test]
    fn a_pass_this_compiler_does_not_have_is_refused_rather_than_ignored() {
        let mut gates = Gates::default();
        let why = gates.add(false, "nosuch").expect_err("a pass that does not exist was accepted");
        assert!(why.contains("not a pass"), "{why}");
        assert!(gates.is_empty());
    }

    #[test]
    fn a_range_list_that_says_nothing_is_refused() {
        let mut gates = Gates::default();
        assert!(gates.add(false, "fold=").is_err());
        assert!(gates.add(false, "fold=1,,3").is_err());
    }

    #[test]
    fn a_range_that_runs_backwards_is_refused() {
        let mut gates = Gates::default();
        let why = gates.add(false, "fold=9-2").expect_err("a backwards range was accepted");
        assert!(why.contains("ends before it starts"), "{why}");
    }

    #[test]
    fn a_number_that_is_not_one_is_refused() {
        let mut gates = Gates::default();
        assert!(gates.add(false, "fold=1x").is_err());
        assert!(gates.add(false, "fold=1-x").is_err());
    }
}
