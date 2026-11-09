/* accept: all */
/* Two pointers to the same width with a different sign are a warning and not an error, because
   they point at the same bytes. gcc calls it -Wpointer-sign and keeps it off until -Wall or
   -pedantic asks, and so does this compiler, so with no flags the case is silent.
   crates/rucc/tests/kernel_flags.rs has the warning under the flags that turn it on. */

char *narrow(unsigned char *bytes)
{
    char *text = bytes;
    return text;
}
