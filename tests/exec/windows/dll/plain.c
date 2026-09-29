/* A DLL with no .def file and no dllexport, so the linker exports every global it defines. */

#include <string.h>

int twice(int x) { return 2 * x; }

/* A frame bigger than a page, so this DLL calls the stack probe and links it in. The probe is ours
 * and not the program's, so it must not show up in the export table next to twice. */
int big(int x)
{
    volatile char buf[8192];
    memset((char *)buf, x, sizeof buf);
    return buf[0] + buf[sizeof buf - 1];
}
