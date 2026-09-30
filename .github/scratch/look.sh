#!/bin/bash
# Scratch: find the file and the function a build goes wrong in.
set -u
top=$PWD
R=$top/target/release/rucc
say() { printf '=== %s\n' "$*"; }

flags() {
    case $1 in
    brotli) echo "-I $2 -I $2/c/include" ;;
    lua) echo "-I $2 -DLUA_USE_LINUX" ;;
    esac
}

src_of() { # project root stem
    if [ "$3" = driver ]; then echo "$top/tests/$1/a-real-workload.c"; else echo "$2/${3//-//}.c"; fi
}

compile() { # project root stem out [env...]
    local p=$1 root=$2 stem=$3 out=$4
    shift 4
    env "$@" "$R" -S --target=x86_64-unknown-linux-gnu -fsafety=detect -O2 -Zverify-each \
        $(flags "$p" "$root") -o "$out" "$(src_of "$p" "$root" "$stem")"
}

run() { # dir-of-p files...
    local lib=$1
    shift
    rm -rf /tmp/look && mkdir -p /tmp/look
    gcc -no-pie "$@" "$lib/safe-rt.a" -lpthread -lm -ldl -o /tmp/look/x >/tmp/look/link 2>&1 || { echo nolink; return; }
    (cd /tmp/look && RUCC_SAFETY_ON_ERROR=continue timeout 300 ./x) >/tmp/look/out 2>&1
    local st=$?
    if [ $st = 0 ] && grep -q "all answers correct" /tmp/look/out; then echo good; else echo "bad $st"; fi
}

set_with() { # lib stems-var swapstem replacement
    local lib=$1 swap=$2 with=$3 s
    for s in $stems; do
        if [ "$s" = "$swap" ]; then echo "$with"; else echo "$lib/$s-O2.s"; fi
    done
}


stems_of() { (cd "$top/target/libraries/$1" && ls *-O2.s | sed 's/-O2\.s$//'); }

narrow() { # project root stem function
    local p=$1 root=$2 s=$3 f=$4 lib=$top/target/libraries/$1
    stems=$(stems_of "$p")
    compile "$p" "$root" "$s" /tmp/one.s RUCC_REMAT_ONLY="$f:0:0" 2>/tmp/said
    cat /tmp/said
    local n
    n=$(grep -oP '\d+(?= locals in '"$f"')' /tmp/said | head -1)
    say "$p: $f has $n locals; none rebuilt: $(run "$lib" $(set_with "$lib" "$s" /tmp/one.s))"
    compile "$p" "$root" "$s" /tmp/one.s RUCC_REMAT_ONLY="$f:0:$n" 2>/dev/null
    say "$p: $f all rebuilt: $(run "$lib" $(set_with "$lib" "$s" /tmp/one.s))"
    local k first=""
    for k in $(seq 0 $((n - 1))); do
        compile "$p" "$root" "$s" /tmp/one.s RUCC_REMAT_ONLY="$f:$k:$((k + 1))" 2>/dev/null
        local r
        r=$(run "$lib" $(set_with "$lib" "$s" /tmp/one.s))
        echo "$p: $f only local $k: $r"
        if [ "$r" != good ] && [ -z "$first" ]; then first=$k; fi
    done
    for k in $(seq 1 "$n"); do
        compile "$p" "$root" "$s" /tmp/one.s RUCC_REMAT_ONLY="$f:0:$k" 2>/dev/null
        echo "$p: $f locals below $k: $(run "$lib" $(set_with "$lib" "$s" /tmp/one.s))"
    done
    if [ -n "$first" ]; then
        compile "$p" "$root" "$s" /tmp/broken.s RUCC_REMAT_ONLY="$f:$first:$((first + 1))" 2>/dev/null
        compile "$p" "$root" "$s" /tmp/fixed.s RUCC_REMAT_ONLY="$f:0:0" 2>/dev/null
        awk -v f="$f" '$0 ~ "^"f":" {on=1} on {print} on && /\.size/ {exit}' /tmp/broken.s >/tmp/b.s
        awk -v f="$f" '$0 ~ "^"f":" {on=1} on {print} on && /\.size/ {exit}' /tmp/fixed.s >/tmp/f.s
        echo "--- broken $f with local $first"; cat /tmp/b.s
        echo "--- fixed $f"; cat /tmp/f.s
        echo "--- end"
    fi
}


crash() { # project label files...
    local p=$1 label=$2
    shift 2
    local lib=$top/target/libraries/$p
    rm -rf /tmp/look && mkdir -p /tmp/look
    gcc -no-pie "$@" "$lib/safe-rt.a" -lpthread -lm -ldl -o /tmp/look/x || return
    for n in 1 2 3; do
        (cd /tmp/look && RUCC_SAFETY_ON_ERROR=continue timeout 300 ./x) >/tmp/look/out 2>&1
        echo "$p $label run $n: status $? $(grep -c 'all answers correct' /tmp/look/out)"
    done
    head -c 1500 /tmp/look/out; echo
    (cd /tmp/look && RUCC_SAFETY_ON_ERROR=continue timeout 300 gdb -q -batch -ex run -ex bt -ex 'x/12i $pc-30' -ex 'info registers' ./x 2>&1 | tail -60)
}

everything() { # project root dir env...
    local p=$1 root=$2 dir=$3
    shift 3
    mkdir -p "$dir"
    for s in $(stems_of "$p"); do
        compile "$p" "$root" "$s" "$dir/$s.s" "$@" 2>/dev/null
    done
}

sudo apt-get install -y -qq gdb >/dev/null 2>&1
lib=$top/target/libraries
crash brotli default $(for s in $(stems_of brotli); do echo "$lib/brotli/$s-O2.s"; done)
everything brotli "$RUCC_BROTLI_SOURCE" /tmp/bns RUCC_NO_SHARE='*'
crash brotli no-share /tmp/bns/*.s
everything brotli "$RUCC_BROTLI_SOURCE" /tmp/bkl RUCC_KEEP_LOCALS='*'
crash brotli keep-locals /tmp/bkl/*.s
crash lua default $(for s in $(stems_of lua); do echo "$lib/lua/$s-O2.s"; done)
everything lua "$RUCC_LUA_SOURCE" /tmp/lns RUCC_NO_SHARE='*'
crash lua no-share /tmp/lns/*.s
