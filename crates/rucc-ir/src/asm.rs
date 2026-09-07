//! Reading an assembly statement's operands back off the instruction.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.2, which owns what a constraint means.
//!
//! [`crate::AsmInfo`] carries one comma separated constraint list, in the order the template
//! numbers its operands, which is the outputs and then the inputs. The values are somewhere else:
//! an output travelling in a register is a result of the instruction and everything else is an
//! operand of it. Nothing records which entry took which, because the list already says it, and a
//! second copy of the answer would be a second thing to keep in step with the first.
//!
//! [`AsmOperands::read`] is that answer worked out once, so that the back end and anything else
//! that wants it are reading the same rule rather than each writing the scan out again.
//!
//! # Why it can fail
//!
//! Which file an operand travels in is the front end's decision and the front end knows the type,
//! which this does not. A structure is handed over as an address whatever its constraint says, and
//! there is nothing in `"=r"` that says so. So the scan works out what the constraint alone implies
//! and then counts what it worked out against the results and the operands the instruction actually
//! has. A disagreement means the constraint is not the whole story for that statement, and the
//! answer is nothing at all rather than a guess, because the caller's next move is to place values
//! and placing them by a guess is a wrong program rather than a refused one.

use crate::Value;

/// What one entry of a constraint list is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsmRole {
    /// An output, written `=` or `+`, which the assembly writes.
    Output,
    /// An input, which it reads.
    Input,
}

/// One operand of an assembly statement, as its constraint describes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsmOperand {
    /// Which side of the colon it was written on.
    pub role: AsmRole,
    /// Whether the assembly is handed the address of an object rather than a value.
    pub memory: bool,
    /// The result it produces, for an output that travels in a register.
    pub result: Option<Value>,
    /// The value it reads, which every entry but a register output written `=` has.
    ///
    /// An output written `+` is read as well as written, so it has both this and a result. An
    /// output in memory has this and no result, because what the assembly was handed is the address
    /// and the address is an operand like any other.
    pub value: Option<Value>,
    /// The output an input written as a number shares its place with.
    ///
    /// `"0"` is the whole of what a matching constraint says: this input and that output are one
    /// place, so whatever the assembly leaves there is what the output gets.
    pub tied: Option<usize>,
}

/// The operands of one assembly statement, in the order the template counts them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsmOperands {
    list: Vec<AsmOperand>,
}

impl AsmOperands {
    /// The operands of a statement with that constraint list, those results and those values.
    ///
    /// Nothing when the list does not describe what the instruction carries, which is what a
    /// constraint that is not the whole story looks like from here. See the module documentation.
    #[must_use]
    pub fn read(constraints: &str, results: &[Value], values: &[Value]) -> Option<AsmOperands> {
        // An empty list is no operands and not one operand spelled with nothing, which is what
        // splitting the empty string on commas would otherwise give.
        let written: Vec<&str> =
            if constraints.is_empty() { Vec::new() } else { constraints.split(',').collect() };

        let mut list = Vec::with_capacity(written.len());
        let mut result = results.iter();
        let mut value = values.iter();
        for text in written {
            let entry = Entry::read(text)?;
            // An output in a register is the one entry that takes a result, and it takes a value as
            // well when it was written `+`, because that is an output the assembly reads first.
            let register = entry.role == AsmRole::Output && !entry.memory;
            let taken = if register { Some(result.next().copied()?) } else { None };
            let read = if register && !entry.updates { None } else { Some(value.next().copied()?) };
            list.push(AsmOperand {
                role: entry.role,
                memory: entry.memory,
                result: taken,
                value: read,
                tied: entry.tied,
            });
        }

        // Every result and every value accounted for. One left over means the list describes fewer
        // operands than the statement has, which is the disagreement this is looking for.
        if result.next().is_some() || value.next().is_some() {
            return None;
        }
        // A number naming an output that is not there, or naming one that has no result to share,
        // is a list that cannot be placed however the counts came out.
        for entry in &list {
            if let Some(tied) = entry.tied {
                if !list.get(tied).is_some_and(|output| output.result.is_some()) {
                    return None;
                }
            }
        }
        Some(AsmOperands { list })
    }

    /// The operands, in the order the template counts them.
    pub fn iter(&self) -> impl Iterator<Item = &AsmOperand> {
        self.list.iter()
    }

    /// The value an output shares its place with, which is what a matching constraint asks for.
    ///
    /// Written `+` on the output itself, or written as that output's number on an input, and the
    /// two mean the same thing to whatever has to place the values.
    #[must_use]
    pub fn tied_to(&self, output: usize) -> Option<Value> {
        let written = self.list.get(output)?;
        if written.value.is_some() {
            return written.value;
        }
        self.list.iter().find(|entry| entry.tied == Some(output)).and_then(|entry| entry.value)
    }

    /// How many operands there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// Whether there are none, which is what an `asm` with a bare template has.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}

/// One constraint, read for the three things the scan above needs from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    role: AsmRole,
    memory: bool,
    updates: bool,
    tied: Option<usize>,
}

impl Entry {
    /// One constraint, or nothing for one with a letter this does not know.
    ///
    /// Not knowing a letter is the same answer as a count that does not add up and for the same
    /// reason. A letter nobody has read is a letter that may mean the operand is somewhere other
    /// than where the rest of the constraint suggests.
    fn read(text: &str) -> Option<Entry> {
        let mut role = AsmRole::Input;
        let mut updates = false;
        let mut memory = false;
        let mut register = false;
        let mut tied = None;

        let mut rest = text.chars().peekable();
        while let Some(letter) = rest.next() {
            match letter {
                '=' => role = AsmRole::Output,
                '+' => {
                    role = AsmRole::Output;
                    updates = true;
                }
                // Earlyclobber and commutative, which say when the operand is written and whether
                // it may swap with the one after it. Neither changes where it lives, and where it
                // lives is the whole of what this reads.
                '&' | '%' => {}
                // The memory forms. `o` and `V` are the offsettable and the non offsettable halves
                // of `m`, and all three are an address as far as anything here is concerned.
                'm' | 'o' | 'V' => memory = true,
                // A register, an immediate, or either. `g` and `X` allow memory as well, and the
                // front end takes the register when it has the choice, so they count as registers
                // here for the same reason `rm` does.
                'r' | 'g' | 'X' | 'i' | 'n' | 's' | 'a' | 'b' | 'c' | 'd' | 'S' | 'D' | 'A'
                | 'q' | 'Q' | 'f' | 't' | 'u' | 'x' | 'y' | 'v' | 'l' | 'e' | 'k' | 'h' | 'j'
                | 'z' | 'w' => register = true,
                // The immediate ranges, which are `I` through `P` on x86 and are a constant
                // wherever they are read.
                'E' | 'F' | 'G' | 'H' | 'I' | 'J' | 'K' | 'L' | 'M' | 'N' | 'O' | 'P' => {
                    register = true;
                }
                // A matching constraint, which is the number of the output this shares a place
                // with. More than one digit is a statement with more than ten operands, and the
                // number is the whole run of them rather than the first.
                '0'..='9' => {
                    let mut number = letter.to_digit(10)? as usize;
                    while let Some(next) = rest.peek().and_then(|&c| c.to_digit(10)) {
                        number = number * 10 + next as usize;
                        rest.next();
                    }
                    tied = Some(number);
                    register = true;
                }
                _ => return None,
            }
        }

        // A constraint that named nothing at all is not one the front end would have produced, and
        // reading it as a register would be reading it as something it does not say.
        if !memory && !register {
            return None;
        }
        Some(Entry { role, memory: memory && !register, updates, tied })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Value;

    /// Values numbered from zero, which is all the scan looks at.
    fn values(count: usize) -> Vec<Value> {
        (0..count).map(Value::from_usize).collect()
    }

    #[test]
    fn a_statement_with_no_operands_has_none() {
        let read =
            AsmOperands::read("", &[], &[]).expect("an empty list describes an empty statement");
        assert!(read.is_empty());
    }

    #[test]
    fn an_input_in_a_register_reads_a_value_and_produces_nothing() {
        let values = values(1);
        let read = AsmOperands::read("r", &[], &values).expect("one input");
        let [operand] = read.iter().copied().collect::<Vec<_>>()[..] else { panic!("one operand") };
        assert_eq!(operand.role, AsmRole::Input);
        assert!(!operand.memory);
        assert_eq!(operand.result, None);
        assert_eq!(operand.value, Some(values[0]));
    }

    #[test]
    fn an_output_in_a_register_produces_a_result_and_reads_nothing() {
        let results = values(1);
        let read = AsmOperands::read("=r", &results, &[]).expect("one output");
        let operand = read.iter().next().copied().expect("one operand");
        assert_eq!(operand.role, AsmRole::Output);
        assert_eq!(operand.result, Some(results[0]));
        assert_eq!(operand.value, None);
        assert_eq!(read.tied_to(0), None);
    }

    #[test]
    fn an_output_written_plus_reads_the_value_it_overwrites() {
        let results = values(1);
        let args = values(1);
        let read = AsmOperands::read("+r", &results, &args).expect("one output read and written");
        let operand = read.iter().next().copied().expect("one operand");
        assert_eq!(operand.result, Some(results[0]));
        assert_eq!(operand.value, Some(args[0]));
        assert_eq!(read.tied_to(0), Some(args[0]));
    }

    #[test]
    fn an_input_written_as_a_number_shares_the_place_of_that_output() {
        let results = values(1);
        let args = values(1);
        let read = AsmOperands::read("=r,0", &results, &args).expect("an output and its match");
        let list: Vec<AsmOperand> = read.iter().copied().collect();
        assert_eq!(list[1].tied, Some(0));
        assert_eq!(read.tied_to(0), Some(args[0]));
    }

    #[test]
    fn an_operand_in_memory_is_an_address_whichever_side_it_is_on() {
        let args = values(2);
        let read = AsmOperands::read("=m,m", &[], &args).expect("an output and an input in memory");
        let list: Vec<AsmOperand> = read.iter().copied().collect();
        assert!(list[0].memory && list[1].memory);
        assert_eq!(list[0].result, None);
        assert_eq!(list[0].value, Some(args[0]));
        assert_eq!(list[1].value, Some(args[1]));
    }

    #[test]
    fn a_constraint_that_allows_a_register_or_memory_takes_the_register() {
        let results = values(1);
        let read = AsmOperands::read("=rm", &results, &[]).expect("an output that could be either");
        let operand = read.iter().next().copied().expect("one operand");
        assert!(!operand.memory);
        assert_eq!(operand.result, Some(results[0]));
    }

    #[test]
    fn a_list_describing_more_results_than_there_are_is_refused() {
        assert_eq!(AsmOperands::read("=r,=r", &values(1), &[]), None);
    }

    #[test]
    fn a_list_describing_fewer_values_than_there_are_is_refused() {
        assert_eq!(AsmOperands::read("r", &[], &values(2)), None);
    }

    #[test]
    fn a_number_naming_an_output_that_is_not_there_is_refused() {
        assert_eq!(AsmOperands::read("=r,3", &values(1), &values(1)), None);
    }

    #[test]
    fn a_number_naming_an_output_in_memory_is_refused() {
        assert_eq!(AsmOperands::read("=m,0", &[], &values(2)), None);
    }

    #[test]
    fn a_letter_nobody_has_read_is_refused() {
        assert_eq!(AsmOperands::read("^", &[], &values(1)), None);
    }

    #[test]
    fn a_constraint_that_names_nowhere_at_all_is_refused() {
        assert_eq!(AsmOperands::read("&", &[], &values(1)), None);
    }

    #[test]
    fn a_number_of_more_than_one_digit_is_the_whole_run() {
        let results = values(11);
        let args = values(1);
        let outputs = ["=r"; 11].join(",");
        let read = AsmOperands::read(&format!("{outputs},10"), &results, &args)
            .expect("eleven outputs and a match on the last");
        let list: Vec<AsmOperand> = read.iter().copied().collect();
        assert_eq!(list[11].tied, Some(10));
        assert_eq!(read.tied_to(10), Some(args[0]));
    }
}
