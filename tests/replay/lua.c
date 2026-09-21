/* The Lua harness, which is OSS-Fuzz's fuzz_lua written out in C89 declarations. */
/* Upstream is `projects/lua/fuzz_lua.c` in google/oss-fuzz, which Lua's own tarball does not carry,
   so this is written here rather than taken. It is already C, so the only changes are declarations
   at the top of a block because that is how the C in this repository is written.

   What it does is load the input as a Lua chunk and then run it. Two things about that are worth
   reading before reading a result. The first is the mode argument `t`, which means the loader takes
   text and refuses a precompiled binary chunk, so this corpus is Lua source rather than bytecode
   and `lundump.c` is not what it exercises. The second is that `luaL_newstate` creates a state and
   nothing else: the standard libraries are never opened, so the chunk runs with no `os`, no `io`
   and no `string`, and what it can reach is the parser, the compiler, the virtual machine, the
   garbage collector and the table and string internals. That is the reason a corpus of executable
   inputs is safe to replay, and it is also why the corpus is as large as it is, since a fuzzer with
   nothing to call has to find its way into the interpreter by shape alone.

   The error path is upstream's and is kept because it is code too. A chunk that fails to load or
   raises while running goes through `msghandler`, which builds a traceback with `luaL_traceback`,
   and that walks the call stack and the debug information of every function on it. A malformed
   chunk that gets far enough to raise therefore ends up in `ldebug.c` rather than stopping at the
   parser, and the message it prints goes to stderr where the driver catches it.

   One deviation is not in this file at all and belongs here anyway: OSS-Fuzz builds this target
   against Lua's master branch, which is the 5.5 development line, and the libraries table here
   builds 5.4.8 out of the release tarball. So the corpus was collected against an interpreter a
   little newer than the one it is being replayed through. That costs coverage rather than
   soundness, since an input selected for a 5.5 code path may reach nothing special in 5.4.8, and
   the other direction does not happen. Using the release is the right call for everything else this
   repository does with Lua, so the note goes here instead. */
#define lua_c

#include "lprefix.h"

#include <signal.h>
#include <stdio.h>

#include "lauxlib.h"
#include "lua.h"

/* Upstream's name for the program, which is what an error message is prefixed with. */
#define PROGNAME "lua"

/* The state the signal handler reaches the interpreter through. */
static lua_State *globalL = NULL;

static const char *progname = PROGNAME;

/* Set as a debug hook by the signal handler, to stop the interpreter from inside itself. */
static void lstop(lua_State *L, lua_Debug *ar) {
    (void)ar;
    lua_sethook(L, NULL, 0, 0);
    luaL_error(L, "interrupted!");
}

/* The C signal handler. A signal cannot touch a Lua state, since nothing synchronises the two, so
   all it does is arrange for the next thing the interpreter does to be the stop above. */
static void laction(int i) {
    int flag = LUA_MASKCALL | LUA_MASKRET | LUA_MASKLINE | LUA_MASKCOUNT;

    signal(i, SIG_DFL);
    lua_sethook(globalL, lstop, flag, 1);
}

static void l_message(const char *pname, const char *msg) {
    if (pname) {
        fprintf(stderr, "%s: ", pname);
    }
    fprintf(stderr, "%s\n", msg);
}

/* Prints whatever is on the top of the stack if the status is not ok, and takes it off again. */
static int report(lua_State *L, int status) {
    if (status != LUA_OK) {
        const char *msg = lua_tostring(L, -1);

        l_message(progname, msg);
        lua_pop(L, 1);
    }
    return status;
}

/* The message handler every chunk runs under. It turns whatever was raised into a string and then
   appends a traceback, which is the part that walks the stack. */
static int msghandler(lua_State *L) {
    const char *msg = lua_tostring(L, 1);

    if (msg == NULL) {
        if (luaL_callmeta(L, 1, "__tostring") && lua_type(L, -1) == LUA_TSTRING) {
            return 1;
        }
        msg = lua_pushfstring(L, "(error object is a %s value)", luaL_typename(L, 1));
    }
    luaL_traceback(L, L, msg, 1);
    return 1;
}

/* `lua_pcall` with the message handler under the function and the signal handler installed. */
static int docall(lua_State *L, int narg, int nres) {
    int status;
    int base = lua_gettop(L) - narg;

    lua_pushcfunction(L, msghandler);
    lua_insert(L, base);
    globalL = L;
    signal(SIGINT, laction);
    status = lua_pcall(L, narg, nres, base);
    signal(SIGINT, SIG_DFL);
    lua_remove(L, base);
    return status;
}

static int dochunk(lua_State *L, int status) {
    if (status == LUA_OK) {
        status = docall(L, 0, 0);
    }
    return report(L, status);
}

int LLVMFuzzerTestOneInput(const unsigned char *data, unsigned long size) {
    lua_State *L = luaL_newstate();

    if (L == NULL) {
        return 0;
    }
    dochunk(L, luaL_loadbufferx(L, (const char *)data, (size_t)size, "test", "t"));
    lua_close(L);
    return 0;
}
