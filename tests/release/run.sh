#!/bin/sh
# The release test: what a user does first with a Linux archive, on a clean system.
#
# usage: tests/release/run.sh <unpacked archive> [<c-testsuite checkout>]
#
# It runs as root inside a container of one Linux system. It installs only the C library
# development package and binutils, and the tools this script itself needs, so that the archive
# has to bring everything else. Then it builds and runs hello world, a program with threads, a
# program that uses a shared library with thread-local storage, and rung 0 when a c-testsuite
# checkout is given. The default row is the one rucc picks on this machine, so nothing passes
# --target. Any step that fails, fails the run.

set -eu

[ $# -eq 1 ] || [ $# -eq 2 ] || { echo "usage: $0 <unpacked archive> [<c-testsuite>]" >&2; exit 2; }
here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
rucc=$(CDPATH='' cd -- "$1" && pwd)/rucc
suite=${2:-}

. /etc/os-release
echo "release test on $PRETTY_NAME, $(uname -m)"
case $ID in
ubuntu | debian)
	export DEBIAN_FRONTEND=noninteractive
	# The Debian 11 security pool on deb.debian.org gives 404 for packages that its index still
	# names, so Debian 11 gets its packages from archive.debian.org.
	if [ "${VERSION_CODENAME:-}" = bullseye ]; then
		printf '%s\n' 'deb http://archive.debian.org/debian bullseye main' \
			'deb http://archive.debian.org/debian-security bullseye-security main' >/etc/apt/sources.list
	fi
	apt-get update -qq
	apt-get install -y -qq libc6-dev binutils >/dev/null
	;;
rocky | fedora)
	dnf install -y -q glibc-devel binutils diffutils
	;;
alpine)
	apk add -q musl-dev binutils
	;;
arch)
	pacman -Sy --noconfirm --needed -q binutils diffutils >/dev/null
	;;
*)
	echo "release test: no package list for $ID" >&2
	exit 2
	;;
esac

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM
cd "$work"

"$rucc" --version
row=$("$rucc" -dumpmachine)
echo "the default row is $row"

cat >hello.c <<'C'
#include <stdio.h>
int main(void) { puts("hello"); return 0; }
C
"$rucc" hello.c -o hello
[ "$(./hello)" = hello ] || { echo "hello world printed the wrong thing" >&2; exit 1; }
echo "ok hello"

cat >threads.c <<'C'
#include <pthread.h>
#include <stdio.h>
static int total;
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static void *add(void *arg) {
	for (int i = 0; i < 1000; i++) {
		pthread_mutex_lock(&lock);
		total += *(int *)arg;
		pthread_mutex_unlock(&lock);
	}
	return 0;
}
int main(void) {
	pthread_t t[4];
	int one = 1;
	for (int i = 0; i < 4; i++) pthread_create(&t[i], 0, add, &one);
	for (int i = 0; i < 4; i++) pthread_join(t[i], 0);
	printf("%d\n", total);
	return 0;
}
C
"$rucc" -O2 -pthread threads.c -o threads
[ "$(./threads)" = 4000 ] || { echo "the threads program gave the wrong total" >&2; exit 1; }
echo "ok threads"

cat >tls.c <<'C'
__thread int counter = 40;
int bump(void) { return ++counter; }
C
cat >use.c <<'C'
#include <pthread.h>
#include <stdio.h>
int bump(void);
static void *twice(void *arg) {
	(void)arg;
	bump();
	return (void *)(long)bump();
}
int main(void) {
	pthread_t t;
	void *got;
	pthread_create(&t, 0, twice, 0);
	pthread_join(t, &got);
	printf("%d %ld\n", bump(), (long)got);
	return 0;
}
C
"$rucc" -O2 -fPIC -shared tls.c -o libtls.so
"$rucc" -O2 -pthread use.c -L. -ltls -Wl,-rpath,"$work" -o use
[ "$(./use)" = "41 42" ] || { echo "the shared library with TLS gave the wrong values" >&2; exit 1; }
echo "ok shared library with TLS"

if [ -n "$suite" ]; then
	"$here/../rung0/run.sh" "$rucc" "$row" "$suite"
fi
echo "release test on $PRETTY_NAME: ok"
