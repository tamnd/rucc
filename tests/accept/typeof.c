/* accept: c23 gnu */
/* reject: c89 c99 c11 c17 */
/* `typeof` was a GNU extension for thirty years before C23 took it, so the gnu dialects have
   had it all along and the iso ones get it in C23. */

typeof(1 + 1) counted;
