/* A workload against an instrumented Lua, with answers it has to get right. */
/* The third library run under the monitor end to end, and the first one that is a language runtime
   rather than a data structure. SQLite recycles its own storage through a lookaside allocator and
   zlib walks a handful of large buffers, but both of them hold a pointer in a field and use it as a
   pointer. Lua does neither. Every value it moves is a tagged union whose payload is sometimes a
   pointer and sometimes an integer of the same width, the collector reaches every live object by
   walking those unions, and every error unwinds through longjmp out of the middle of a C call
   stack the interpreter built. That is three separate things none of the earlier rows reach.

   The answers are arithmetic over a stream this program seeds for itself, so they are the same
   numbers against any build of the library and against no library at all. Nothing here depends on
   iteration order: the script uses numeric loops and ipairs, and the one place the C side walks a
   table with lua_next it sums the values rather than looking at their order.

   The headers are included rather than written out because lua_State is opaque but luaL_Reg,
   lua_Integer and the stack index macros are not, and they have to be the ones the library was
   compiled with. They come from the same tree the sources do. */
#include <stdio.h>
#include <string.h>

#include <lauxlib.h>
#include <lua.h>
#include <lualib.h>

/* Everything the script computes is kept under this, so the answers fit an integer of any width the
   build uses and do not depend on where Lua's own overflow happens. */
#define MODULUS 1000000007

/* The workload proper. Each phase returns one number and the whole thing returns their sum, so a
   single wrong answer anywhere moves the one value this program checks.

   The phases are chosen for the paths they take through the runtime rather than for what they
   compute: growing tables past every rehash the implementation does, building strings that have to
   be interned and then collected, suspending and resuming stacks that are not the one the C caller
   is on, dispatching through metatables, and throwing errors out of the middle of a call. */
static const char *WORKLOAD =
    "local M = 1000000007\n"
    "local state = 12345\n"
    "local function step()\n"
    "  state = (state * 1103515245 + 12345) % 2147483648\n"
    "  return state\n"
    "end\n"
    /* Two thousand tables, each grown a field at a time so the array part and the hash part are
       both rebuilt several times, and all of them held at once so the collector has to trace them
       rather than reclaiming each one before the next is made. */
    "local function tables()\n"
    "  local acc, held = 0, {}\n"
    "  for i = 1, 2000 do\n"
    "    local row = {}\n"
    "    for j = 1, 16 do row[j] = (step() + i * j) % M end\n"
    "    row.name = 'row' .. i\n"
    "    held[i] = row\n"
    "  end\n"
    "  for i = 1, #held do\n"
    "    local row = held[i]\n"
    "    for j = 1, #row do acc = (acc + row[j] * j) % M end\n"
    "    acc = (acc + #row.name) % M\n"
    "  end\n"
    "  return acc\n"
    "end\n"
    /* Strings long enough to be allocated rather than interned in the short string table, taken
       apart with the pattern matcher, which is the one part of the standard library that indexes a
       buffer with two pointers walking towards each other. */
    "local function strings()\n"
    "  local acc, parts = 0, {}\n"
    "  for i = 1, 400 do\n"
    "    parts[i] = string.format('%d:%s;', step() % 1000, string.rep('ab', i % 30 + 1))\n"
    "  end\n"
    "  local whole = table.concat(parts)\n"
    "  for number, letters in string.gmatch(whole, '(%d+):(%a+);') do\n"
    "    acc = (acc + tonumber(number) + #letters) % M\n"
    "  end\n"
    "  local swapped = string.gsub(whole, 'ab', 'ba')\n"
    "  for i = 1, #swapped, 997 do acc = (acc + string.byte(swapped, i)) % M end\n"
    "  return acc\n"
    "end\n"
    /* Coroutines, which are the reason the interpreter cannot keep its state in C locals: each one
       gets a stack of its own that the collector has to find and that resume and yield hand back
       and forth. */
    "local function threads()\n"
    "  local acc = 0\n"
    "  for round = 1, 50 do\n"
    "    local co = coroutine.create(function(seed)\n"
    "      local carried = seed\n"
    "      for k = 1, 40 do carried = coroutine.yield((carried * 31 + k) % M) end\n"
    "      return carried\n"
    "    end)\n"
    "    local ok, value = coroutine.resume(co, round)\n"
    "    while ok and coroutine.status(co) == 'suspended' do\n"
    "      acc = (acc + value) % M\n"
    "      ok, value = coroutine.resume(co, value)\n"
    "    end\n"
    "  end\n"
    "  return acc\n"
    "end\n"
    /* Metatables, so that reads and writes that look like plain indexing become calls the
       interpreter has to dispatch, and inheritance chains it has to walk. */
    "local function meta()\n"
    "  local base = {}\n"
    "  base.__index = function(_, key) return #tostring(key) * 7 end\n"
    "  base.__add = function(a, b) return (a.value + b.value) % M end\n"
    "  local function make(v) return setmetatable({value = v}, base) end\n"
    "  local acc = 0\n"
    "  for i = 1, 3000 do\n"
    "    local a, b = make(step() % M), make(step() % M)\n"
    "    acc = (acc + (a + b)) % M\n"
    "    acc = (acc + a['missing' .. (i % 10)]) % M\n"
    "  end\n"
    "  return acc\n"
    "end\n"
    /* Errors, which leave the interpreter through longjmp from wherever they were raised, past C
       frames that the unwinder does not run anything for. Half of these are raised by the runtime
       itself rather than by the script, so the throw happens inside the library. */
    "local function errors()\n"
    "  local acc = 0\n"
    "  for i = 1, 1500 do\n"
    "    local ok, why = pcall(function()\n"
    "      if i % 2 == 0 then error({code = i % 97}) end\n"
    "      local nothing = nil\n"
    "      return nothing.field\n"
    "    end)\n"
    "    if ok then acc = (acc + 1) % M\n"
    "    elseif type(why) == 'table' then acc = (acc + why.code) % M\n"
    "    else acc = (acc + #why) % M end\n"
    "  end\n"
    "  return acc\n"
    "end\n"
    /* A full collection between each phase and a heap large enough that the incremental collector
       runs several times inside one, so objects are traced in every colour it has. */
    "local total = 0\n"
    "for _, phase in ipairs({tables, strings, threads, meta, errors}) do\n"
    "  collectgarbage('collect')\n"
    "  total = (total + phase()) % M\n"
    "end\n"
    "collectgarbage('collect')\n"
    "return total\n";

/* The other half of the library's surface: the stack the host pushes and pops by hand, which is
   where an embedder's own mistakes usually are and where every value crosses between C and the
   interpreter. Sums rather than reads a sequence out of lua_next, so the answer does not depend on
   an order the manual says is not promised. */
static int through_the_stack(lua_State *state, long *answer) {
    long acc = 0;
    int i;

    lua_createtable(state, 500, 8);
    for (i = 1; i <= 500; i++) {
        lua_pushinteger(state, (lua_Integer)((i * 2654435761u) % MODULUS));
        lua_seti(state, -2, i);
    }
    for (i = 0; i < 8; i++) {
        char key[16];
        snprintf(key, sizeof key, "field%d", i);
        lua_pushstring(state, key);
        lua_pushinteger(state, (lua_Integer)(i * i + 1));
        lua_settable(state, -3);
    }

    lua_pushnil(state);
    while (lua_next(state, -2) != 0) {
        acc = (acc + (long)lua_tointeger(state, -1)) % MODULUS;
        lua_pop(state, 1);
    }

    /* A reference into the registry, which is the table the collector is told to treat as a root,
       and then the hole the release leaves in its free list. */
    {
        int handle = luaL_ref(state, LUA_REGISTRYINDEX);
        lua_rawgeti(state, LUA_REGISTRYINDEX, handle);
        if (lua_rawlen(state, -1) != 500) return 1;
        acc = (acc + (long)lua_rawlen(state, -1)) % MODULUS;
        lua_pop(state, 1);
        luaL_unref(state, LUA_REGISTRYINDEX, handle);
    }

    *answer = acc;
    return 0;
}

int main(void) {
    lua_State *state = luaL_newstate();
    long scripted;
    long embedded = 0;
    int bad = 0;

    if (!state) {
        printf("MISMATCH\n");
        return 1;
    }
    luaL_openlibs(state);

    if (luaL_loadstring(state, WORKLOAD) != LUA_OK) {
        printf("load failed: %s\n", lua_tostring(state, -1));
        lua_close(state);
        return 1;
    }
    if (lua_pcall(state, 0, 1, 0) != LUA_OK) {
        printf("run failed: %s\n", lua_tostring(state, -1));
        lua_close(state);
        return 1;
    }
    scripted = (long)lua_tointeger(state, -1);
    lua_pop(state, 1);

    if (through_the_stack(state, &embedded) != 0) bad = 1;
    lua_close(state);

    printf("scripted=%ld embedded=%ld\n", scripted, embedded);
    /* Arithmetic over a stream this program seeds, so these are the same two numbers against every
       build of the library. */
    if (scripted != 11401761L) bad = 1;
    if (embedded != 697466330L) bad = 1;
    printf("%s\n", bad ? "MISMATCH" : "all answers correct");
    return bad;
}
