/* accept: all */
/* The shapes the kernel writes: `__init` between the return type and the name, `__initdata` after
   the declarator, a table entry nothing refers to kept by `used`, and a `static` inside a
   function. */

static int __attribute__((section(".init.text"))) setup(void) { return 0; }
static int limit __attribute__((section(".init.data"))) = 4;
typedef int (*initcall_t)(void);
static initcall_t __initcall_setup __attribute__((used, section(".initcall6.init"))) = setup;

int reads(void) {
  static int once __attribute__((__section__(".data..once"))) = 1;
  return once + limit;
}
