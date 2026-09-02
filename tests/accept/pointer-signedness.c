/* accept: all */
/* warns: pointer targets in initialization of 'char *' from 'unsigned char *' differ in signedness */
/* Two pointers to the same width with a different sign are a warning and not an error, because
   they point at the same bytes. gcc calls it -Wpointer-sign and keeps it off until -Wall or
   -pedantic asks; there is no switch to name here yet, so it is always on. */

char *narrow(unsigned char *bytes)
{
    char *text = bytes;
    return text;
}
