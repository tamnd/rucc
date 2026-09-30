/* reject: all */
/* message: section attribute argument not a string constant */
/* The name of a section is a string, and gcc refuses anything else with this sentence. */

__attribute__((section(1))) int x;
