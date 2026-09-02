/* accept: all */
/* warns: `#pragma once` in the main file */
/* The warning is about what the line usually means here, which is a header that ended up
   being compiled on its own. It is applied all the same, because a file that includes itself
   is the one case where the line does work in a main file, and without it this file is an
   include that never ends. gcc warns and applies it too. */

#pragma once

#include "a-pragma-once-in-the-main-file.c"

int read_once[1];
