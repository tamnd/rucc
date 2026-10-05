#!/bin/sh
# One program of the GCC torture execute tests at one optimization level, for tests/torture/run.sh,
# which sets the TORTURE_ variables and runs this file once for each pair. It prints one line, the
# name, the level and the result, and keeps the compiler's messages in a log file beside the
# program.
#
# usage: tests/torture/case.sh <name> <level>
#
# The name is the path of the source under the execute directory without `.c`, so `ieee/fp-cmp-1`
# and `builtins/abs-1` are names. The flags come from the place that GCC's own driver reads them
# from. The top directory is run by `execute.exp`, which reads the `dg-` directives in each source.
# `ieee` and `builtins` are run by `ieee.exp` and `builtins.exp`, which give flags of their own to
# each program and read a `.x` file beside it, if there is one.

set -eu

[ $# -eq 2 ] || { echo "usage: $0 <name> <level>" >&2; exit 2; }
name=$1
level=$2
source=$TORTURE_SUITE/$name.c
flat=$(printf '%s\n' "$name" | tr / _)-O$level
binary=$TORTURE_OUT/$flat.wasm
log=$TORTURE_OUT/$flat.log

say() {
	echo "$name -O$level $1"
	exit 0
}

# Each directive is on one line in this suite, so a line is the unit that is read. -w is what
# `execute.exp` and `c-torture-execute` both give to every program.
flags=-w
case $name in
ieee/*)
	flags="$flags -fno-inline"
	case " $TORTURE_FALSE " in
	*" signal "*) flags="$flags -DSIGNAL_SUPPRESS" ;;
	esac
	;;
builtins/*)
	flags="$flags -fno-tree-dse -fno-tree-loop-distribute-patterns -fno-tracer -fno-ipa-ra -fno-inline-functions"
	;;
esac

case $name in
ieee/* | builtins/*)
	x=$TORTURE_SUITE/$name.x
	if [ -f "$x" ]; then
		# The `.x` files that stop a program on every target this runner knows do it with
		# this one test. The other `return 1` lines are inside a test for a target by name,
		# and an indented `lappend` is inside one too.
		if grep -q '^if { ! \[check_effective_target_nonlocal_goto\] }' "$x"; then
			case " $TORTURE_FALSE " in
			*" nonlocal_goto "*) say 'skipped, needs nonlocal_goto' ;;
			esac
		fi
		set_flags=$(sed -n 's/^set additional_flags //p' "$x" | tr -d '"')
		if [ -n "$set_flags" ]; then
			flags="-w $set_flags"
		fi
		flags="$flags $(sed -n 's/^lappend additional_flags //p' "$x" | tr -d '"' | tr '\n' ' ')"
	fi
	;;
*)
	for target in $(sed -n 's/.*dg-require-effective-target[[:space:]]*\([a-z0-9_]*\).*/\1/p' "$source"); do
		case " $TORTURE_FALSE " in
		*" $target "*) say "skipped, needs $target" ;;
		esac
	done
	# The other `dg-skip-if` lines name a target by name, or an option set that has neither
	# -O0 nor -O2 in it.
	if grep -q 'dg-skip-if[[:space:]]*"[^"]*"[[:space:]]*{[[:space:]]*![[:space:]]*{[[:space:]]*i?86' "$source"; then
		say 'skipped, x86 only'
	fi
	size=$(sed -n 's/.*dg-require-stack-size[[:space:]]*"\([^"]*\)".*/\1/p' "$source")
	if [ -n "$size" ] && [ $(($size)) -gt "$TORTURE_STACK" ]; then
		say "skipped, needs a stack of $size bytes"
	fi
	# The flags are the first quoted string. A selector after them is taken only if it is
	# `{ target { ! signal } }` and the target has no signals, and every other selector in the
	# suite names a target by name.
	directives=$(grep 'dg-\(additional-\)\{0,1\}options' "$source" || true)
	while IFS= read -r line; do
		[ -n "$line" ] || continue
		rest=$(printf '%s\n' "$line" | sed 's/.*dg-\(additional-\)\{0,1\}options[[:space:]]*{\{0,1\}[[:space:]]*//')
		these=$(printf '%s\n' "$rest" | sed -n 's/^"\([^"]*\)".*/\1/p')
		selector=$(printf '%s\n' "$rest" | sed 's/^"[^"]*"[[:space:]]*//')
		case $selector in
		'{ target { ! signal } }'*)
			case " $TORTURE_FALSE " in
			*" signal "*) ;;
			*) continue ;;
			esac
			;;
		'{'*) continue ;;
		esac
		flags="$flags $these"
	done <<EOF
$directives
EOF
	;;
esac

# The level of the round comes first, and a level that the program names for itself wins over it,
# as it does in GCC's own runs.
sources=$source
case $name in
builtins/*) sources="$source $TORTURE_SUITE/$name-lib.c $TORTURE_SUITE/builtins/lib/main.c" ;;
esac
# shellcheck disable=SC2086
if ! $TORTURE_RUCC --target="$TORTURE_TRIPLE" -O"$level" $flags $sources -o "$binary" -lm $TORTURE_LINK >"$log" 2>&1; then
	say 'did not build'
fi
work=$TORTURE_OUT/$flat.d
mkdir -p "$work"
# shellcheck disable=SC2086
if ! (cd "$work" && ${RUNNER:-} "$binary") >"$log" 2>&1; then
	say 'exited nonzero'
fi
rm -rf "$binary" "$work"
say ok
