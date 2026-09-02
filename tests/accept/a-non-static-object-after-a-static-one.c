/* reject: all */
/* message: non-static declaration of 'j' follows static declaration */
/* The other half of `a-static-function-defined-without-the-keyword.c`. An object at file scope
   with no storage class has external linkage of its own rather than taking what was there, so
   this pair is two declarations that disagree about which linkage the name has. gcc refuses it
   in the same words. */

static int j;

int j;
