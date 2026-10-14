# Builds the Lua interpreter for the checks in this directory. This file is sourced, not run.
#
#   build_lua OUT CC FLAGS...
#
# LUA is the directory of a Lua 5.4 release, the one with src/ in it. CC is a whole command, such
# as "rucc" or "gcc", and it compiles each file and links the interpreter. FLAGS go to each step.
# The interpreter is OUT/lua.
build_lua() {
    local out=$1 cc src name
    read -r -a cc <<<"$2"
    shift 2
    mkdir -p "$out/obj"
    for src in "$LUA"/src/*.c; do
        name=$(basename "$src" .c)
        case $name in luac) continue ;; esac
        "${cc[@]}" "$@" -DLUA_COMPAT_5_3 -DLUA_USE_LINUX -c "$src" -o "$out/obj/$name.o"
    done
    "${cc[@]}" "$@" -o "$out/lua" "$out"/obj/*.o -lm -ldl
}
