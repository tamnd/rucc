/* row: 10.7 the mixed link */
/* allow */
/* A program that calls `fmod` without libm on the link line, which is a link nobody had made
   until SQLite did. The archive this suite links against carries `compiler_builtins`, which has
   a weak `fmod` in it, so the linker takes that one and pulls the unit holding it in. That unit
   also holds a reference to `rust_eh_personality`, because `compiler_builtins` is not built with
   panics turned off, and the reference has to resolve even though nothing here unwinds and
   nothing ever calls it. Every other case in this suite is small enough to need no unit of
   `compiler_builtins` at all, so this is the shape that says the archive is linkable rather than
   only linkable against the programs that happen to be here.

   The judgement is that there is nothing to judge. `fmod` reads no memory of the program's, and a
   report from this would mean the monitor is reporting on the runtime it is part of. */
double fmod(double x, double y);

int main(void) {
    double left = 7.5;
    double right = 2.0;
    return fmod(left, right) == 1.5 ? 0 : 1;
}
