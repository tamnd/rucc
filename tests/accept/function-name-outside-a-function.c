/* accept: all */
/* warns: '__func__' is not defined outside of function scope */
/* Out here there is no function to name, and gcc hands back the empty string and warns rather
   than refusing the program, so a file that has one still builds. */

const char *nobody = __func__;
