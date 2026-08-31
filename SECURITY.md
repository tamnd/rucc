# Security policy

## Supported versions

The project is pre-1.0 and nothing is supported in the sense that word usually carries. Fixes go on the default branch. Once there is a 1.0, this section will say something more useful.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting on this repository, under Security, Report a vulnerability. That opens a private thread with the maintainers. Please do not open a public issue for something you believe is exploitable.

Expect an acknowledgement within a few days. If you have not heard anything in a week, feel free to nudge in a public issue without describing the problem.

## What counts

Compiling untrusted input is a security boundary. People run compilers on code they did not write, in CI, on build machines, against code from pull requests. So all of these are in scope:

- Memory unsafety in the compiler on any input, valid or not. The `unsafe` in this codebase lives in the arenas and the memory-mapped source handling, and a bug there is the most serious kind we can have.
- A hang or unbounded memory growth on a bounded input. A compiler that can be made to consume a build machine is a denial of service with extra steps.
- A miscompilation that turns correct source into unsafe object code. This is a correctness bug and a security bug at the same time, and `spec/02-the-goal.md` axis 1 treats it as the most serious class of defect in the project.
- Anything that causes the compiler to write outside the files it was asked to write, or to execute something it was not asked to execute.

A controlled internal compiler error is not a vulnerability. It is a bug, it should be filed as one, and the requirement in `spec/15-testing.md` section 15.6 is a clean diagnostic or a controlled ICE, never a panic inside `unsafe` and never a hang.

## What we do about it

Reports are triaged, reduced, fixed on a private branch, and released with an advisory that says what the problem was and what it affected. Reporters are credited unless they prefer not to be.
