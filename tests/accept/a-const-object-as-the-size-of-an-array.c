/* accept: gnu */
/* reject: iso */
/* message: variably modified 'buf' at file scope */
/* gcc reads a `const` object of integer type as its value in the size of an array, so this is an
   array of four rather than a variable length array, which C does not allow at file scope. Linux
   7.2 relies on it. The strict dialects keep 6.6 as it is written. */

const int n = 4;
char buf[n];
typedef char four[sizeof buf == 4 ? 1 : -1];
