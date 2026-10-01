/* reject: all */
/* message: wrong number of arguments specified for 'copy' attribute */
/* The attribute names one declaration to take the attributes of, and gcc refuses it with none or
   with two. */

int x;
__attribute__((copy(x, x))) int d;
