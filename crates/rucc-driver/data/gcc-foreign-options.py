"""Writes gcc-foreign-options.txt from gcc's own option files.

    python3 gcc-foreign-options.py GCC16/gcc/c-family/c.opt GCC16/gcc/common.opt \
        GCC14/gcc/c-family/c.opt GCC15/gcc/c-family/c.opt > gcc-foreign-options.txt

The first two are from gcc 16, the third from gcc 14 and the fourth from gcc 15. A line is an option of c.opt that
gcc takes for C++, Objective-C or Objective-C++ and not for C, and is not in common.opt. Only the
-f, -W, -std= and -nostdinc++ options are kept, which are the ones a build passes.
"""

import re
import sys

# Every language a gcc with the C, C++ and Objective-C front ends has, in the order gcc names
# them. LTO is always built. A gcc with the D or Fortran front end adds those to the names too.
ORDER = ["C", "C++", "LTO", "ObjC", "ObjC++"]
SKIP = {"Language", "Variable", "TargetVariable", "HeaderInclude", "SourceInclude", "Enum",
        "EnumValue", "TargetSave"}


def records(path):
    with open(path, encoding="utf-8") as f:
        text = f.read()
    for rec in re.split(r"\n\s*\n", text):
        lines = [line for line in rec.split("\n") if line and not line.startswith(";")]
        if len(lines) >= 2 and lines[0].strip() not in SKIP:
            yield lines[0].strip(), lines[1].strip()


def table(c_opt, common_opt):
    common = {name for name, _ in records(common_opt)}
    merged = {}
    for name, props in records(c_opt):
        langs, all_props = merged.setdefault(name, (set(), []))
        langs.update(word for word in props.split() if word in ORDER)
        all_props.append(props)
    out = {}
    for name, (langs, all_props) in merged.items():
        props = " ".join(all_props)
        words = props.split()
        if "C" in langs or not langs or name in common or "Common" in words:
            continue
        if not (name[0] in "fW" or name.startswith("std=") or name == "nostdinc++"):
            continue
        if "Ignore" in words:
            kind = "ignore"
        elif "WarnRemoved" in words:
            kind = "removed"
        elif any(word.startswith("Joined") for word in words):
            kind = "joined"
        else:
            kind = "exact"
        negative = "RejectNegative" not in words and name[0] in "fW"
        alias = re.search(r"Alias\(([^)]*)\)", props)
        alias = ",".join(part.strip() for part in alias.group(1).split(",")) if alias else "-"
        out[name] = ["/".join(lang for lang in ORDER if lang in langs), kind,
                     "neg" if negative else "-", alias]
    # An alias of an option that is not here, such as -fhandle-exceptions of -fexceptions, is
    # left out, since the option it stands for is one gcc takes for C.
    return {name: row for name, row in out.items()
            if row[3] == "-" or row[3].split(",")[0] in out}


def main():
    new = table(sys.argv[1], sys.argv[2])
    gcc14 = {name for name, _ in records(sys.argv[3])}
    gcc15 = {name for name, _ in records(sys.argv[4])}
    for name in sorted(new):
        since = "0" if name in gcc14 else "15" if name in gcc15 else "16"
        print("\t".join([name, *new[name], since]))


main()
