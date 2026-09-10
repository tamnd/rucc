/* What LD_PRELOAD puts in front of the library under test. An exported name is one the loader may
 * find a different definition of, and this is that definition. */

int interposed(void) { return 99; }
