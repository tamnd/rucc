/* The half of the library this compiler builds. Design: spec/11-asm-objects-debug.md 11.3.
 *
 * Every name in here is one of the cases from tamnd/rucc#760, which are the things that only go
 * wrong once the object is in a shared library and that nothing else in the suite can see. */

/* Defined in another library, so the address of it is one only the loader knows. */
extern int other_var;

/* Defined in the other file of this library, and not reachable from outside it. */
int hidden_helper(void) __attribute__((visibility("hidden")));

/* Exported and written by the executable, which takes its own copy of it and leaves the library
 * reading a name whose address is now somewhere else. */
int shared_var = 1;

int exported(void) { return 7; }

int reads_shared(void) { return shared_var; }

int reads_other(void) { return other_var; }

int calls_hidden(void) { return hidden_helper(); }

/* Exported and not replaceable, so a call from inside the library binds to this one. */
__attribute__((visibility("protected"))) int protected_answer(void) { return 1; }

int interposed(void) { return 1; }

int calls_interposed(void) { return interposed(); }
