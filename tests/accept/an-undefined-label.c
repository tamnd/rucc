/* reject: all */
/* message: label 'away' used but not defined */
/* A label is looked for over the whole function, so this is reported at the end of the
   definition rather than where the jump was written. */

void f(void) {
  goto away;
}
