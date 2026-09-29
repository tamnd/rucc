/* A DLL whose exports come from listed.def, and whose constructor runs when it is loaded. */

int counter = 1;

int add(int a, int b) { return a + b; }

int unlisted(void) { return 7; }

__attribute__((constructor)) static void start(void) { counter = 40; }
