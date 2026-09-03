# Running the generated code

Every layer in document 15 is about finding a bug before it reaches a user. This document is about the one question none of the other layers answer, which is whether the program the compiler produced does what the program the user wrote said. A compiler that parses a corpus, lowers it, verifies its own IR and prints machine instructions has proved a great deal about itself and nothing at all about the executable, because nothing so far has run one.

That gap is not hypothetical. The harness in `tamnd/rucc-compat` today gets five corpora through the front end, the lowering and the verifier, and round trips the IR, and the number it reports is a number about compiling rather than about running. This document specifies the half that is missing, in enough detail that the harness can be built against it and that a result from it can be read six months later by somebody who was not there.

## 20.1 What an execution test is

Four parts, and all four have to be written down for a result to mean anything.

A **case** is one program, from a corpus, with the flags it is compiled under and the input it is given. A case is identified by corpus, unit and path, which is the same identity `rucc-compat check` already uses, so the two commands can name the same case and their results can be read side by side.

A **build** turns the case into an executable, by one of the paths in section 20.4. A build that fails is a result, not an error: it is reported as a build failure with the compiler's diagnostic attached, and it is a different outcome from a program that built and then gave a wrong answer.

A **run** executes it under the limits in section 20.6 and captures the exit status, the standard output, the standard error and whether it died on a signal.

An **oracle** says whether that run was right. Section 20.3 says which oracle applies to which corpus, and the choice is a property of the corpus rather than of the case, because a corpus that mixes oracles is a corpus where a passing number cannot be interpreted.

## 20.2 Generating is not running

It is worth being explicit about why the check the harness already does is not enough, since the two look similar from a distance.

The front end check proves that a program was accepted, that it lowered to IR the verifier accepts, and that the IR printer and the IR parser agree. Every one of those can hold while the generated code is wrong. A lowering rule that is proved correct in isolation still has to be selected for the right term, given the right operands, allocated registers that do not overwrite each other, and assembled into the bytes the manual describes. Between the last thing the verifier sees and the first thing the processor sees there are four passes and an encoder, and the only test that covers all of them at once is running the program.

The reverse is also true and is the reason both exist. An execution test that fails tells you the program is wrong and nothing about where, and a compiler with only execution tests is a compiler debugged by bisection. The layers are complementary and this document does not replace any of them.

## 20.3 The oracles

Three, in decreasing order of how much they can be trusted, and every corpus names the one it uses in its manifest.

**Self checking.** The program decides for itself and says so through its exit status. This is what the GCC torture execution suite is: each program computes something, compares it against what it should be, and calls `abort` when it does not match, so an exit status of zero is a pass and anything else is a failure. This oracle is the best of the three because it needs no reference compiler and no recorded expectation, and because the check was written by the person who found the bug the program is about. The c-testsuite is the same shape with a recorded expected output alongside.

**Reference differential.** The program is built twice, once by rucc and once by the reference compiler, and both are run on the same input. A pass is the same exit status and the same standard output. This is the oracle for real projects, which mostly print rather than check. The reference is gcc 16, section 20.5, and standard error is captured and reported but is not part of the comparison, because two compilers may legitimately produce programs that warn differently at run time through library messages we do not control.

**Recorded expectation.** The corpus ships what the program is supposed to print, and the run has to match it. This is the c-testsuite's `.expected_output` files and it needs neither a reference compiler nor a self check, which is what makes it the one oracle available when the reference compiler cannot build the case at all.

A case whose oracle is unavailable is reported as not compared rather than as a pass. That is the same rule the preprocessor differential already follows and it exists because a harness that quietly counts an unmeasurable case as a success is a harness that reports a number nobody should act on.

## 20.4 The build paths

There are three ways to get from a C file to an executable, they land in this order, and the harness runs every one that is available on the machine.

**Through assembly.** `rucc -S` writes assembly text, the system assembler turns it into an object, and the system linker links it. This is the first path that can work and it is deliberately the first one built, because it depends on `-S` and on nothing else: no encoder, no object writer, no relocations. It is also the path where a failure is easiest to read, since the intermediate artifact is text a person can inspect and hand to `gcc -c` on its own.

**Through our own object.** `rucc -c` writes an ELF object with our own encoder and our own writer, and the system linker links it. This is the path that exercises the encoder and the relocations, and it is the one that has to work for the milestone to be over.

**Through the driver.** `rucc case.c -o case` does the whole thing, which is the second path plus finding a linker and passing it the right arguments. This is what a user runs and it is the one the exit criterion of M3 is written in terms of.

The paths are not alternatives to choose between. Once two of them exist, every case runs through both, and a case that passes on one and fails on the other is a bug in the one that fails, located by construction. That is how the encoder gets checked against the assembly printer without anybody writing a separate encoder differential: the same instruction description produces both, so a disagreement between them is a disagreement inside one description and the two paths make it visible as a wrong answer rather than as a byte diff nobody reads.

## 20.5 The reference

gcc 16, built from source where the distribution does not ship it, which is the arrangement `tamnd/rucc#84` set up and which the preprocessor differential already uses. The harness takes a `--reference` path and reports which compiler it actually used in every result it writes, because a number measured against gcc 13 and a number measured against gcc 16 are different numbers and a result that does not say which one it is cannot be compared against last month's.

Both compilers are given the same flags, the same include paths and the same language level, and the harness asks the reference for the last three rather than assuming them, exactly as the preprocessor differential does. A difference that comes from the two compilers being handed different headers is not a difference in the generated code and finding one of those costs a day.

The reference is also the arbiter of what is a valid case. A program the reference compiler refuses is skipped rather than excluded, and skipped cases are counted separately and reported, because a corpus written in 2005 contains programs that no compiler released this decade accepts and treating those as our failures makes our number a fiction.

## 20.6 Running a program safely

The harness runs code from a corpus, and a corpus of compiler test programs is full of programs that loop forever when they are miscompiled. So every run is bounded.

A **timeout**, per case, defaulting to ten seconds and overridable per corpus. A timeout is its own outcome. It is not a failure of the same kind as a wrong answer, because a program that would have printed the right thing eventually is a performance bug and a program that printed the wrong thing is a miscompilation, and the report keeps them apart.

A **memory limit**, so that a miscompiled allocation loop does not take the machine down with it. On a machine without a way to set one the harness says so rather than pretending it set one.

A **fresh temporary directory per case**, which is also the working directory of the run, so that a program writing a file does not see another case's file and a run leaves nothing behind.

**No network**, which is not enforced by the harness and is a property of where it runs. This is written down so that the day somebody adds a corpus whose programs open sockets, the decision is visible.

**A signal is not an exit status.** A program killed by `SIGSEGV` and a program that returned 139 are different events and are reported differently, because the first is nearly always a miscompilation and the second is nearly always the test.

The output of a run is captured in full and truncated only in the report, never in the comparison. A difference in the ten thousandth line of output is still a difference.

## 20.7 Cross execution

Two of the three targets in document 02 are not the machine the harness runs on, and running the generated code is exactly as important there. The mechanism is QEMU user mode emulation, the same one document 15.7 already puts in the CI matrix for aarch64 and riscv64, and the only thing it changes about this document is that the run command has a prefix and the timeout is longer.

It is out of scope until there is a second target, which is M6. Naming it here is what keeps the harness from being written in a way that assumes the program it built can be executed by running it.

## 20.8 The exclusion list

The same discipline the check command already has, and for the same reason. Every case that does not pass yet is an entry in the corpus manifest, every entry names the issue that will take it off the list, and a manifest with an entry that has no issue does not load.

The list is checked for going stale on every whole run. A case that starts passing while its entry is still there fails the run, and so does an entry naming a case the corpus does not have. That is what stops an exclusion list from becoming the place a regression goes to be quiet.

An execution exclusion says which of the four outcomes it covers, because "this case does not build" and "this case builds and prints the wrong thing" are different admissions and the second is much worse. A case excluded for a build failure that starts failing at run time instead has not started passing and has not stayed the same, and the harness says so.

## 20.9 What full lowering coverage means

Two different counts get called coverage and they are worth separating, because one of them is structural and the other is empirical.

**Every IR opcode has at least one lowering rule for the target.** This is a property of the rule set and the instruction description, it is checked when the compiler is built, and the answer is a list of opcodes with nothing to lower them. Document 10 asks for this test and document 15.8 lists the count as a measured number, and it costs nothing at run time because nothing has to be executed to find it out. An opcode with no rule is a program that cannot be compiled, discovered at build time rather than by a user.

**Every lowering rule fires at least once over the corpus.** This is a property of the corpus, not of the compiler, and it is the number this document is about. A rule that is written, proved and never selected is a rule that has never run, and a proof about a rule that has never run has never been checked against a real machine. Rules that never fire are also where dead entries in the rule set accumulate, since nothing else would ever notice them.

Measuring the second needs the compiler's help. `-Zrule-coverage=FILE` writes the identity of every rule that fired during a compilation, and the harness unions those files over the corpus and reports the rules that appear in none of them. The identity is the rule's file and line, which is stable across a build and readable in a report, rather than an index into a generated table that changes when a rule is added above it.

The gate on that number is staged deliberately. First it is reported, so that the number exists and its movement is visible. Then a threshold, so it cannot fall. Only then is it a hundred percent with a checked-in list of the rules the corpus does not reach and the reason each one is unreachable, which is the same shape as every other exclusion list here and is subject to the same staleness check. Going to a hundred percent before the corpus is large enough would be an invitation to write a test that exists to make a number go up, and a test written for a number is a test that tests the number.

## 20.10 What it reports

One file per corpus under `results/`, written by the harness, holding the counts by outcome, the list of failures with enough detail to reproduce one by hand, and the identity of both compilers. A result is about the machine that produced it, so the machine is named in it and CI keeps its own as an artifact rather than committing it.

The outcomes are these and no others: passed, wrong answer, crashed, timed out, did not build, not compared, skipped, excluded. A summary that collapses them into passed and failed is a summary that hides the difference between a compiler that is wrong and a compiler that is incomplete.

## 20.11 Where it runs

Per commit, on the small corpora, which is the c-testsuite and the chibicc programs, at whatever optimization levels exist. These are fast enough that the gate is not felt and they cover enough that a wrong answer usually shows up in them first.

Nightly, on the torture suite and on the real projects, at every optimization level. The torture suite is the largest and it is also the one whose value per minute is highest, since every one of its programs is a bug report that was once a wrong answer from a released compiler.

On the real machines rather than only in a container, because the ABI and the linker are the parts most likely to be different on somebody else's machine, and a Windows result and a Linux result are different results.

## 20.12 What this is not

Not a benchmark. Nothing here measures how fast the generated code is, and a case that passes slowly passes. Document 16 owns that question and mixing the two would make both harder to read.

Not a fuzzer. Every case here was written by a person for a reason, and the randomized half of the correctness story is document 15.4.

Not a substitute for the verifier. A rule that is exercised by a thousand programs and proved by nobody is still an unproved rule, and the order of precedence in this project is that a rule is proved first and run second.
