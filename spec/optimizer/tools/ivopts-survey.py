#!/usr/bin/env python3
"""The ivopts survey of tamnd/rucc#701, taken from GCC and from rucc over the same sources.

Section 28.3 of `spec/optimizer/28-induction-variables.md` bounds the search with GCC's three
parameters, and section 28.8 says the numbers worth having are how big the search actually gets and
how often the selection differs from the variables the loop came with. This answers both, for both
compilers, so that a bound can be argued about against a measurement rather than against GCC's
defaults.

    python3 spec/optimizer/tools/ivopts-survey.py \\
        --rucc target/release/rucc \\
        --gcc /opt/gcc-16.2.0/bin/gcc-16 \\
        --corpus ../rucc-corpus/programs

GCC is read from `-fdump-tree-ivopts-details`, which prints one block per loop it processes. rucc is
read from `-fopt-info-all`, whose notes are counted per function rather than per loop, so the per
loop tables are taken from the functions each compiler worked on exactly one loop in and the
coverage is printed rather than left implicit.

Two things make this a comparison of two passes rather than of two answers to one question. GCC runs
ivopts after vectorization, so a loop it sees may be a quarter as many memory accesses as the loop
rucc sees. And GCC counts uses of its own kinds, which do not line up with rucc's one for one. The
numbers that do compare directly are the size of the search and the size of the set chosen.
"""

import argparse
import json
import os
import re
import subprocess
import tempfile
from collections import Counter
from concurrent.futures import ProcessPoolExecutor

FUNC = re.compile(r"^;; Function (\S+) ")
LOOP = re.compile(r"^Processing loop (\d+) at ")
GROUP = re.compile(r"^Group (\d+):")
USE = re.compile(r"^  Use (\d+)\.(\d+):")
KIND = re.compile(r"^  Type:\t(REFERENCE ADDRESS|COMPARE|GENERIC)$")
CAND = re.compile(r"^Candidate (\d+):")
PICKED = re.compile(r"^Selected IV set for loop \d+ at .*, (\d+) IVs:")

NOTE = re.compile(r"^\S+: (\S+): (?:note|missed|optimized): (.*) \((\d+)\) \[ivopts\]$")

POPULATION = "loop with at least one induction variable use in it"
USE_ADDRESS = "address of a read or a write that moves by a fixed step"
USE_COMPARE = "comparison against something that moves by a fixed step"
USE_GENERIC = "other use of something that moves by a fixed step"
GROUPED = "address uses sharing one variable, apart in a constant offset"
PRICED = "group of uses every candidate for the loop is priced against"
CANDIDATE = "induction variable considered for the loop to keep"
CHOSEN = "induction variable chosen for the loop to keep"
KEPT = "loop whose own induction variables are the ones worth keeping"
CHANGED = "loop that would be cheaper with a different set of induction variables"
RETARGETED = "exit test asked of the pointer the loop walks, so the counter goes"

# Section 28.3, which is GCC's own defaults for these three.
IV_MAX_CONSIDERED_USES = 250
IV_CONSIDER_ALL_CANDIDATES_BOUND = 40
IV_ALWAYS_PRUNE_CAND_SET_BOUND = 10


def gcc_loops(gcc, path, work):
    """Every loop GCC's ivopts processed in one file, or None if it would not compile."""
    out = subprocess.run(
        [gcc, "-O2", "-S", "-w", "-std=c17", "-fdump-tree-ivopts-details",
         "-dumpdir", work + os.sep, "-o", os.devnull, path],
        capture_output=True, cwd=work,
    )
    if out.returncode != 0:
        return None
    dumps = [name for name in os.listdir(work) if name.endswith(".ivopts")]
    if not dumps:
        return []
    text = open(os.path.join(work, dumps[0]), errors="replace").read()
    for name in dumps:
        os.remove(os.path.join(work, name))

    loops, func, cur = [], "?", None
    for line in text.splitlines():
        found = FUNC.match(line)
        if found:
            func = found.group(1)
            continue
        if LOOP.match(line):
            cur = {"file": path, "func": func, "groups": 0, "cands": 0, "ivs": None, "kinds": {},
                   "widest": {}}
            loops.append(cur)
            continue
        if cur is None:
            continue
        found = GROUP.match(line)
        if found:
            cur["groups"] = max(cur["groups"], int(found.group(1)) + 1)
            continue
        found = USE.match(line)
        if found:
            group, nth = int(found.group(1)), int(found.group(2))
            cur["widest"][group] = max(cur["widest"].get(group, 0), nth + 1)
            continue
        found = KIND.match(line)
        if found:
            cur["kinds"][found.group(1)] = cur["kinds"].get(found.group(1), 0) + 1
            continue
        found = CAND.match(line)
        if found:
            cur["cands"] = max(cur["cands"], int(found.group(1)) + 1)
            continue
        found = PICKED.match(line)
        if found:
            cur["ivs"] = int(found.group(1))

    for cur in loops:
        cur["uses"] = sum(cur["widest"].values())
        cur["merged"] = sum(1 for wide in cur["widest"].values() if wide > 1)
        del cur["widest"]
    return loops


def rucc_funcs(rucc, path, work):
    """What rucc's ivopts notes say about one file, per function, or None if it would not build."""
    info = os.path.join(work, "info.txt")
    out = subprocess.run(
        [rucc, "-O2", "-S", "-w", "-o", os.devnull, "-fopt-info-all=" + info, path],
        capture_output=True, cwd=work,
    )
    if out.returncode != 0 or not os.path.exists(info):
        return None
    funcs = {}
    for line in open(info, errors="replace").read().splitlines():
        found = NOTE.match(line)
        if found:
            funcs.setdefault(found.group(1), Counter())[found.group(2)] += int(found.group(3))
    os.remove(info)
    return funcs


def one_file(job):
    rucc, gcc, path = job
    with tempfile.TemporaryDirectory() as work:
        return path, gcc_loops(gcc, path, work), rucc_funcs(rucc, path, work)


def sources(root):
    found = []
    for base, _, names in os.walk(root):
        for name in names:
            if name.endswith(".c"):
                found.append(os.path.join(base, name))
    found.sort()
    return found


def spread(name, counts, bound=None):
    """One distribution as a markdown table, with the bound it is being read against."""
    total = sum(counts.values())
    biggest = max(counts) if counts else 0
    against = "" if bound is None else f", against a bound of {bound}"
    print(f"\n{name}: {total} loops, the largest {biggest}{against}")
    print(f"\n| {name} | loops |")
    print("| --- | --- |")
    for key in sorted(counts):
        print(f"| {key} | {counts[key]} |")


def report(doc):
    loops = doc["gcc_loops"]
    alone = Counter((one["file"], one["func"]) for one in loops)
    single = [one for one in loops if alone[(one["file"], one["func"])] == 1]

    print(f"# ivopts over {doc['files']} sources\n")
    print(f"GCC would not compile {doc['gcc_failed']} of them and rucc would not compile "
          f"{doc['rucc_failed']}.\n")

    print("## GCC\n")
    print(f"ivopts processed {len(loops)} loops, {len(single)} of them in a function it processed "
          f"exactly one loop in.")
    uses = sum(one["uses"] for one in loops)
    groups = sum(one["groups"] for one in loops)
    merged = sum(one["merged"] for one in loops)
    print(f"\n{uses} uses fell into {groups} groups, of which {merged} hold more than one use.")
    kinds = Counter()
    for one in loops:
        for kind, count in one["kinds"].items():
            kinds[kind] += count
    print("They are " + ", ".join(f"{count} {kind.lower()}" for kind, count in kinds.most_common())
          + ".")
    spread("groups per loop", Counter(one["groups"] for one in loops), IV_MAX_CONSIDERED_USES)
    spread("candidates per loop", Counter(one["cands"] for one in loops),
           IV_CONSIDER_ALL_CANDIDATES_BOUND)
    spread("variables kept per loop", Counter(one["ivs"] for one in loops if one["ivs"]),
           IV_ALWAYS_PRUNE_CAND_SET_BOUND)

    funcs = [(path, func, Counter(counts)) for path, func, counts in doc["rucc_funcs"]]
    seen = sum(counts[POPULATION] for _, _, counts in funcs)
    one_loop = [counts for _, _, counts in funcs if counts[POPULATION] == 1]
    print("\n## rucc\n")
    print(f"ivopts processed {seen} loops across {len(funcs)} functions, {len(one_loop)} of which "
          f"hold exactly one of them.")
    total = Counter()
    for _, _, counts in funcs:
        total += counts
    print(f"\n{total[USE_ADDRESS] + total[USE_COMPARE] + total[USE_GENERIC]} uses fell into "
          f"{total[PRICED]} groups, of which {total[GROUPED]} hold more than one use.")
    print(f"They are {total[USE_ADDRESS]} addresses, {total[USE_COMPARE]} comparisons and "
          f"{total[USE_GENERIC]} of everything else.")
    spread("groups per loop", Counter(counts[PRICED] for counts in one_loop),
           IV_MAX_CONSIDERED_USES)
    spread("candidates per loop", Counter(counts[CANDIDATE] for counts in one_loop),
           IV_CONSIDER_ALL_CANDIDATES_BOUND)
    spread("variables kept per loop", Counter(counts[CHOSEN] for counts in one_loop),
           IV_ALWAYS_PRUNE_CAND_SET_BOUND)
    print(f"\nThe selection kept the loop's own variables in {total[KEPT]} loops and changed the "
          f"set in {total[CHANGED]}, and the exit test moved to a pointer in {total[RETARGETED]}.")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--rucc", required=True, help="a rucc built for the host")
    parser.add_argument("--gcc", required=True, help="a GCC 16 built for the host")
    parser.add_argument("--corpus", required=True, help="the programs directory of rucc-corpus")
    parser.add_argument("--jobs", type=int, default=os.cpu_count() or 1)
    parser.add_argument("--json", help="where to leave the raw survey, for a second look")
    args = parser.parse_args()

    # Each worker runs the compilers in a directory of its own, so that GCC's dump has somewhere
    # to land, which makes a relative path to a compiler mean the wrong thing.
    rucc, gcc = os.path.abspath(args.rucc), os.path.abspath(args.gcc)
    files = sources(args.corpus)
    loops, funcs = [], []
    gcc_failed = rucc_failed = 0
    jobs = [(rucc, gcc, path) for path in files]
    with ProcessPoolExecutor(max_workers=args.jobs) as pool:
        for path, gcc, rucc in pool.map(one_file, jobs, chunksize=8):
            if gcc is None:
                gcc_failed += 1
            else:
                loops.extend(gcc)
            if rucc is None:
                rucc_failed += 1
            else:
                funcs.extend((path, func, dict(counts)) for func, counts in rucc.items())

    doc = {"files": len(files), "gcc_failed": gcc_failed, "rucc_failed": rucc_failed,
           "gcc_loops": loops, "rucc_funcs": funcs}
    if args.json:
        json.dump(doc, open(args.json, "w"))
    report(doc)


if __name__ == "__main__":
    main()
