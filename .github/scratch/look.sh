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

look() { # project root
    local p=$1 root=$2 lib=$top/target/libraries/$1
    stems=$(cd "$lib" && ls *-O2.s | sed 's/-O2\.s$//')
    say "$p: all -O2: $(run "$lib" $(set_with "$lib" none none))"
    head -c 3000 /tmp/look/out
    echo
    local culprits=""
    for s in $stems; do
        local r
        r=$(run "$lib" $(set_with "$lib" "$s" "$lib/$s-O0.s"))
        echo "$p: $s at -O0: $r"
        [ "$r" = good ] && culprits="$culprits $s"
    done
    say "$p: files that fix it alone:$culprits"
    for s in $culprits; do
        for toggle in RUCC_NO_SHARE='*' RUCC_KEEP_LOCALS='*' RUCC_KEEP_NAMES=1 RUCC_NO_PASS_LATE=1; do
            compile "$p" "$root" "$s" /tmp/one.s "$toggle" || { echo "$s $toggle: no compile"; continue; }
            local r
            r=$(run "$lib" $(set_with "$lib" "$s" /tmp/one.s))
            echo "$p: $s with $toggle: $r"
            if [ "$r" = good ] && [ "${toggle#*=}" = '*' ]; then
                local var=${toggle%%=*}
                for f in $(grep -oP '^\s*\.type\s+\K[^,]+(?=,\s*@function)' "$lib/$s-O2.s"); do
                    compile "$p" "$root" "$s" /tmp/one.s "$var=$f" || continue
                    r=$(run "$lib" $(set_with "$lib" "$s" /tmp/one.s))
                    if [ "$r" = good ]; then
                        say "$p: $s: $var=$f fixes it"
                        compile "$p" "$root" "$s" /tmp/fixed.s "$var=$f"
                        compile "$p" "$root" "$s" /tmp/broken.s
                        awk -v f="$f" '$0 ~ "^"f":" {on=1} on {print} on && /\.size/ {exit}' /tmp/broken.s >/tmp/b.s
                        awk -v f="$f" '$0 ~ "^"f":" {on=1} on {print} on && /\.size/ {exit}' /tmp/fixed.s >/tmp/f.s
                        echo "--- broken $f"; cat /tmp/b.s | head -1500
                        echo "--- fixed $f"; cat /tmp/f.s | head -1500
                    fi
                done
            fi
        done
    done
}

look brotli "$RUCC_BROTLI_SOURCE"
look lua "$RUCC_LUA_SOURCE"
