/* accept: all */
/* Every attribute here asks gcc either to hold back a warning this compiler does not give, or to
   leave out a transformation or an instrumentation this compiler never does. Taking them in
   silence is the whole of what they ask for, and `__has_attribute` answers yes for each, so a
   header that asks and then writes one has to get this far without a word. The standard's own
   spellings of two of them are read under C23 alone, which is the dialect that has them. */

#if __STDC_VERSION__ >= 202311L
#define FALLTHROUGH [[fallthrough]]
#define MAYBE_UNUSED [[maybe_unused]]
#else
#define FALLTHROUGH __attribute__((fallthrough))
#define MAYBE_UNUSED __attribute__((unused))
#endif

void use(char *);
int sink(int);

__attribute__((noclone, externally_visible, no_profile_instrument_function)) int counted(int x);
int counted(int x) { return x + 1; }

static int spare __attribute__((__unused__));
MAYBE_UNUSED static int also_spare;
static void helper(void) __attribute__((unused));
static void helper(void) {}

struct __attribute__((unused)) record {
  char tag[4] __attribute__((__nonstring__));
  int n __attribute__((unused));
};

int step(int state, int unused_arg __attribute__((unused))) {
  char scratch[16] __attribute__((uninitialized));
  switch (state) {
  case 0:
    state++;
    __attribute__((__fallthrough__));
  case 1:
    state++;
    FALLTHROUGH;
  case 2:
    state++;
    break;
  default:
    break;
  }
  use(scratch);
done:
  __attribute__((unused));
  return sink(state);
}
