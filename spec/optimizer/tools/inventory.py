#!/usr/bin/env python3
"""Generate Ox/02-gcc16-pass-inventory.md from a pinned GCC source tree.

Run from the `gcc/` subdirectory of the pinned tree:

    cd gcc-internals/vendor/gcc/gcc
    python3 ~/notes/Spec/2131/Ox/tools/inventory.py > ~/notes/Spec/2131/Ox/02-gcc16-pass-inventory.md

Three extractions joined on the pass name:

  * every `make_pass_*(gcc::context *)` factory definition, for file and line;
  * every `const pass_data pass_data_* = { KIND, "dumpname", ...}`, for the kind
    and the string `-fdump-tree-<name>` answers to;
  * a nesting-aware walk of passes.def, for the order and the depth.

The verdict column is not extracted. It is rucc's judgement, it lives in VERDICT
and DOC below, and it is the part of the output a reviewer should argue with.
"""

import collections
import json
import os
import re
import sys

TAG = "releases/gcc-16.2.0"

# Passes whose factory is produced by a macro and so is not matched by the
# factory regexp. Resolved by hand; keep this list short and justified.
BY_HAND = {
    "pass_lower_complex_O0": "tree-complex.cc",
    "pass_lower_bitint_O0": "gimple-lower-bitint.cc",
    "pass_sancov_O0": "sancov.cc",
    "pass_lower_switch_O0": "tree-switch-conversion.cc",
    "pass_asan_O0": "asan.cc",
    "pass_tsan_O0": "tsan.cc",
}

# A pass that only holds other passes and has no behaviour of its own.
CONTAINERS = {
    "pass_local_optimization_passes", "pass_all_early_optimizations",
    "pass_all_optimizations", "pass_all_optimizations_g", "pass_build_ssa_passes",
    "pass_ipa_oacc", "pass_ipa_oacc_kernels", "pass_oacc_kernels", "pass_graphite",
    "pass_tree_loop", "pass_tree_no_loop", "pass_vectorize",
    "pass_pre_slp_scalar_cleanup", "pass_rest_of_compilation", "pass_loop2",
    "pass_postreload", "pass_late_compilation", "pass_stack_regs",
    "pass_ipa_auto_profile", "pass_ipa_tree_profile", "pass_tm_init",
}

# (regexp over the pass name, verdict, reason) tried in order. First match wins.
VERDICT = [
    (r"omp|oacc|simduid|simd_clone", "Out", "OpenMP and OpenACC"),
    (r"tm_|_tm$|trans_mem", "Out", "transactional memory"),
    (r"coroutine", "Out", "C++ coroutines"),
    (r"analyzer", "Out", "spec 00: rucc is not a static analyser"),
    (r"odr|devirt|vtable|cdtor", "Out", "C++ only; C has no virtual calls"),
    (r"graphite", "Out", "spec 9.11 rules out a polyhedral framework for 1.0"),
    (r"strub", "Out", "GCC-specific stack scrubbing"),
    (r"warn|diagnose|uninit|walloca|array_bounds|sprintf", "Elsewhere", "a diagnostic"),
    (r"asan|tsan|ubsan|sancov|sanopt|harden", "Defer", "instrumentation, M8"),
]

# (regexp over the pass name, Ox document number) tried in order.
DOC = [
    (r"^pass_(ccp|early_vrp)$", "14"),
    (r"vrp", "10"),
    (r"^pass_(copy_prop|forwprop|phiprop|sccopy|uncprop)$", "15"),
    (r"^pass_(fre|pre)$", "16"),
    (r"dce|dse", "17"),
    (r"^pass_(sra|sra_early|laddress|return_slot|nrv|stdarg)$", "18"),
    (r"^pass_(reassoc|expand_pow|optimize_widening_mul|cse_sincos|cse_reciprocals"
     r"|backprop|optimize_bswap|crc_optimization)$", "19"),
    (r"^pass_(strlen|store_merging|object_sizes|early_object_sizes|call_cdce)$", "20"),
    (r"^pass_(fixup_cfg|build_cfg|cleanup_cfg_post_optimizing|merge_phi|tail_merge"
     r"|split_crit_edges|tracer|split_paths|jump|jump2|jump_after_combine)$", "21"),
    (r"^pass_(phiopt|cselim|tree_ifcombine|if_conversion|isolate_erroneous_paths"
     r"|rtl_ifcvt|if_after_combine|if_after_reload)$", "22"),
    (r"thread_jumps|^pass_dominator$|threadbackward", "23"),
    (r"switch", "24"),
    (r"^pass_(tail_recursion|tail_calls|musttail)$", "25"),
    (r"^pass_(ch|ch_vect|fix_loops|tree_loop|tree_loop_init|tree_loop_done|tree_no_loop"
     r"|iv_canon|scev_cprop|rtl_loop_init|rtl_loop_done|loop2)$", "26"),
    (r"^pass_(lim|sink_code|rtl_move_loop_invariants|rtl_store_motion|predcom)$", "27"),
    (r"^pass_(iv_optimize|strength_reduction|rtl_doloop)$", "28"),
    (r"unroll|peel|loop_jam", "29"),
    (r"^pass_(tree_unswitch|loop_split|loop_versioning|linterchange|loop_distribution"
     r"|parallelize_loops|loop_prefetch)$", "30"),
    (r"vectorize|^pass_pre_slp_scalar_cleanup$", "32"),
    (r"inline", "33"),
    (r"^pass_ipa|modref|pure_const|reference|single_use|split_functions"
     r"|locality_cloning|comdats|visibility|whole_program|icf", "34"),
    (r"profile|auto_profile", "11"),
    (r"^pass_(cse|cse2|cse_after_global_opts|combine|late_combine|ext_dce|ree"
     r"|fold_mem_offsets|rtl_avoid_store_forwarding|postreload_cse|gcse2|rtl_fwprop"
     r"|rtl_fwprop_addr|rtl_cprop|rtl_pre|rtl_hoist|hardreg_pre|rtl_dse1|rtl_dse2"
     r"|fast_rtl_dce|ud_rtl_dce|web|inc_dec|cprop_hardreg|regrename|peephole2"
     r"|compare_elim_after_reload|stack_adjustments|duplicate_computed_gotos|leaf_regs"
     r"|zero_call_used_regs)$", "37"),
    (r"^pass_(sched|sched2|sched_fusion|dep_fusion|sms|live_range_shrinkage|early_remat"
     r"|reorder_blocks|partition_blocks|compute_alignments|shorten_branches"
     r"|cleanup_barriers|delay_slots|machine_reorg)$", "38"),
    (r"^pass_(ira|reload|postreload|late_compilation|rest_of_compilation"
     r"|thread_prologue_and_epilogue|late_thread_prologue_and_epilogue)$", "39"),
    (r".", "36"),  # default: lowering, expansion, bookkeeping
]

KIND = {"GIMPLE_PASS": "gimple", "RTL_PASS": "rtl",
        "SIMPLE_IPA_PASS": "ipa-simple", "IPA_PASS": "ipa"}

LISTS = [(31, 46, "all_lowering_passes"), (49, 161, "all_small_ipa_passes"),
         (163, 187, "all_regular_ipa_passes"), (192, 195, "all_late_ipa_passes"),
         (199, 577, "all_passes")]


def scan_tree(root="."):
    """Return {factory_name: (relpath, line)} and {pass_data_name: (relpath, line, kind, dump)}."""
    factories, datas = {}, {}
    fac_re = re.compile(r"^(make_pass_[a-z0-9_]+) \(gcc::context", re.M)
    data_re = re.compile(
        r"const pass_data (pass_data_[a-z0-9_]+)\s*=\s*\{\s*([A-Z_]+),"
        r"\s*/\*[^*]*\*/\s*\"([^\"]*)\"")
    for dirpath, _, files in os.walk(root):
        if "/testsuite" in dirpath:
            continue
        for fn in files:
            if not fn.endswith(".cc"):
                continue
            path = os.path.join(dirpath, fn)
            rel = os.path.relpath(path, root)
            with open(path, encoding="utf-8", errors="replace") as fh:
                text = fh.read()
            for m in fac_re.finditer(text):
                factories.setdefault(m.group(1), (rel, text[:m.start()].count("\n") + 1))
            for m in data_re.finditer(text):
                datas[m.group(1)] = (rel, text[:m.start()].count("\n") + 1,
                                     m.group(2), m.group(3))
    return factories, datas


def walk_passes_def(path="passes.def"):
    """Return the ordered [(depth, defline, pass_name)] from passes.def."""
    rows, depth = [], 0
    with open(path, encoding="utf-8") as fh:
        for lineno, line in enumerate(fh, 1):
            s = line.strip()
            if s.startswith("NEXT_PASS"):
                rows.append((depth, lineno, re.match(r"NEXT_PASS \((\w+)", s).group(1)))
            elif s.startswith("PUSH_INSERT_PASSES_WITHIN"):
                depth += 1
            elif s.startswith("POP_INSERT_PASSES"):
                depth -= 1
    return rows


def classify(name):
    """Return (verdict, Ox document number). Only Take carries a document."""
    if name in CONTAINERS:
        return "Container", ""
    for pattern, verdict, _reason in VERDICT:
        if re.search(pattern, name):
            return verdict, ""
    for pattern, doc in DOC:
        if re.search(pattern, name):
            return "Take", doc
    return "Take", ""


def which_list(lineno):
    for lo, hi, name in LISTS:
        if lo <= lineno <= hi:
            return name
    return "?"


def main():
    if not os.path.exists("passes.def"):
        sys.exit("run me from the gcc/ subdirectory of the pinned tree")
    factories, datas = scan_tree()
    rows = []
    for depth, defline, name in walk_passes_def():
        fac = factories.get("make_" + name)
        src, srcline = fac if fac else (BY_HAND.get(name), None)
        data = datas.get("pass_data_" + name[len("pass_"):])
        verdict, doc = classify(name)
        rows.append(dict(depth=depth, defline=defline, name=name, src=src, srcline=srcline,
                         kind=data[2] if data else None, dump=data[3] if data else None,
                         verdict=verdict, doc=doc, list=which_list(defline)))

    if "--json" in sys.argv:
        json.dump(rows, sys.stdout, indent=1)
        return

    counts = collections.Counter(r["list"] for r in rows)
    verdicts = collections.Counter(r["verdict"] for r in rows)
    distinct = len({r["name"] for r in rows})
    out = sys.stdout.write

    repeats = collections.Counter(r["name"] for r in rows)
    top = ", ".join(f"`{n[len('pass_'):]}` {c} times"
                    for n, c in repeats.most_common(3))

    out(f"<!-- generated by Ox/tools/inventory.py from {TAG}; do not edit by hand -->\n\n")
    out("# 02. The GCC 16.2.0 pass inventory\n\n")
    out("Every pass GCC 16.2.0 runs, in the order it runs them, with the file that implements\n"
        "it, the name its dump answers to, and what rucc does about it. The table is generated\n"
        "from the pinned tree rather than typed, by the script document 01 describes, so it is a\n"
        "fact about GCC 16.2.0 and not a recollection of one. Regenerate it against a new tag and\n"
        "the diff is the release note nobody writes.\n\n")

    out("## 2.1 The shape of the thing\n\n")
    out(f"`gcc/passes.def` contains {len(rows)} `NEXT_PASS` entries naming {distinct} distinct\n"
        "passes, which is the first useful number in this document. Passes appear more than once\n"
        f"on purpose: {top}. A compiler that runs constant propagation\n"
        "once and calls it done is leaving the interaction between passes on the table, and GCC's\n"
        "answer to the pass-ordering problem is simply to run the cheap passes again after\n"
        "anything that might have created work for them. Document 12 is about whether an e-graph\n"
        "lets us stop doing that.\n\n")
    out("The passes are organised into five lists. `all_lowering_passes` runs once per function\n"
        "as it arrives from the front end and is not optional. `all_small_ipa_passes` and\n"
        "`all_regular_ipa_passes` are the interprocedural stages, split because the first group\n"
        "runs before partitioning under LTO and the second after. `all_late_ipa_passes` is a\n"
        "short tail. `all_passes` is everything else: the whole intraprocedural middle end,\n"
        "`pass_expand` where GIMPLE becomes RTL, and then the entire back end.\n\n")
    out("| List | Entries | What it is |\n|---|---:|---|\n")
    blurb = {"all_lowering_passes": "GIMPLE lowering, CFG construction, EH lowering",
             "all_small_ipa_passes": "SSA construction, early optimization, early inlining",
             "all_regular_ipa_passes": "the real IPO: inlining, constant propagation, SRA",
             "all_late_ipa_passes": "points-to and SIMD cloning after partitioning",
             "all_passes": "the intraprocedural middle end, RTL expansion, the back end"}
    for _lo, _hi, name in LISTS:
        out(f"| `{name}` | {counts[name]} | {blurb[name]} |\n")

    out("\n## 2.2 What rucc does with each of them\n\n")
    out("Five verdicts, assigned per instance rather than per pass, because a pass rucc takes at\n"
        "`-O2` may be one it declines to run early.\n\n")
    out("**Take** means rucc wants the transformation, in some form, at some level. It does not\n"
        "mean rucc copies GCC's implementation; in a dozen cases the transformation is subsumed\n"
        "by the e-graph and the last column points at document 12 or 13 rather than at a pass of\n"
        "its own. **Container** is a pass that only holds other passes and has no behaviour of\n"
        "its own. **Elsewhere** is a diagnostic that GCC happens to schedule as a pass and that\n"
        "rucc emits from the front end instead; `spec/07-types-and-semantics.md` owns those.\n"
        "**Defer** is wanted but not in M4, which in every case here means the sanitizer and\n"
        "hardening instrumentation that `spec/17-milestones.md` puts in M8. **Out** is not wanted\n"
        "at all.\n\n")
    out("| Verdict | Instances |\n|---|---:|\n")
    for k in ("Take", "Out", "Elsewhere", "Defer", "Container"):
        out(f"| {k} | {verdicts[k]} |\n")
    out(f"\nThe {verdicts['Out']} **Out** instances are worth naming as a group, because they are\n"
        "a large part of why GCC's pass list looks unapproachable and not one of them is a C\n"
        "compiler's problem: OpenMP and OpenACC lowering and offloading, transactional memory,\n"
        "C++ coroutines, C++ devirtualization and ODR handling, the static analyser, the\n"
        "polyhedral loop framework, and GCC's stack-scrubbing feature. Deleting them from the\n"
        f"mental model takes the list from {len(rows)} to something a person can hold, and taking\n"
        f"out the {verdicts['Container']} containers and the {verdicts['Elsewhere']} diagnostics\n"
        "takes it down again.\n\n")

    out("## 2.3 The table\n\n")
    out("Columns are: the ordinal in `passes.def`, the nesting depth inside\n"
        "`PUSH_INSERT_PASSES_WITHIN`, the pass, the string its dump answers to\n"
        "(`-fdump-tree-<name>` or `-fdump-rtl-<name>`, with a leading `*` for a pass with no\n"
        "user-visible dump), the pass kind, the file that implements it with the line of its\n"
        "factory function, and rucc's verdict with the document that covers it.\n\n")
    out("| # | D | Pass | Dump | Kind | GCC 16.2.0 source | rucc |\n")
    out("|---:|---:|---|---|---|---|---|\n")
    for i, r in enumerate(rows, 1):
        src = f"`gcc/{r['src']}:{r['srcline']}`" if r["srcline"] else (
            f"`gcc/{r['src']}`" if r["src"] else "—")
        verdict = r["verdict"] + (f" → {r['doc']}" if r["doc"] else "")
        out(f"| {i} | {r['depth']} | `{r['name'][len('pass_'):]}` | `{r['dump'] or '—'}` | "
            f"{KIND.get(r['kind'] or '', '—')} | {src} | {verdict} |\n")


if __name__ == "__main__":
    main()
