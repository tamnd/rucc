/* accept: c23 gnu23 */
/* reject: c89 c99 c11 c17 gnu89 gnu99 gnu11 gnu17 */
/* `bool`, `true` and `false` became keywords in C23. Before that they were whatever
   `<stdbool.h>` made them, which is why this file does not include it. */

bool ready = true;
bool waiting = false;
