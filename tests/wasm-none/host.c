/* The test host of a wasm32-none program, for tests/rung0/run.sh (tamnd/rucc#3137). A wasm32-none
   program has no C library and no system interface, so it imports what it needs from a module
   that the host gives. This file is that module: rucc builds it for wasm32-wasip1 as a reactor,
   and `wasmtime run --preload env=host.wasm` links it as the import module `env` of the program.
   It gives two functions, one that writes a byte to standard output and one that ends the program
   with a status. tests/wasm-none/start.c imports them. */
#include <stdio.h>
#include <stdlib.h>

__attribute__((export_name("putchar"))) int host_putchar(int c) {
    return putchar(c);
}

__attribute__((export_name("exit"))) void host_exit(int status) {
    exit(status);
}
