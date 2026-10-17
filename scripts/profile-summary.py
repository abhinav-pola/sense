#!/usr/bin/env python3
"""Inclusive sample counts per function from a samply profile.

usage: profile-summary.py <profile.json.gz> [regex] [limit]
Needs the profile recorded with --unstable-presymbolicate (the .syms.json beside it).
"""
import bisect, collections, gzip, json, re, sys

path = sys.argv[1]
pattern = re.compile(sys.argv[2]) if len(sys.argv) > 2 else None
limit = int(sys.argv[3]) if len(sys.argv) > 3 else 40
profile = json.load(gzip.open(path))
syms = json.load(open(path.removesuffix(".gz") + ".syms.json"))
strings = syms["string_table"]
tables = {
    lib["debug_name"]: sorted((e["rva"], e["size"], strings[e["symbol"]]) for e in lib["symbol_table"])
    for lib in syms["data"]
}
libs = profile["libs"]
for thread in profile["threads"]:
    if not thread.get("isMainThread"):
        continue
    names, frames, funcs = thread["stringArray"], thread["frameTable"], thread["funcTable"]
    stacks, resources = thread["stackTable"], thread["resourceTable"]

    def name_of(frame):
        func = frames["func"][frame]
        name = names[funcs["name"][func]]
        resource = funcs["resource"][func]
        if not name.startswith("0x") or resource is None or resource < 0:
            return name
        lib = libs[resources["lib"][resource]]["debugName"]
        entries, addr = tables.get(lib, []), int(name, 16)
        i = bisect.bisect_right(entries, (addr, 1 << 62, "")) - 1
        if i >= 0 and entries[i][0] <= addr < entries[i][0] + max(entries[i][1], 1):
            return entries[i][2]
        return f"{lib}:{name}"

    counts = collections.Counter()
    for stack in thread["samples"]["stack"]:
        seen = set()
        while stack is not None:
            name = name_of(stacks["frame"][stack])
            if name not in seen:
                counts[name] += 1
                seen.add(name)
            stack = stacks["prefix"][stack]
    interval = profile["meta"]["interval"]
    print(f"{thread['name']}: {len(thread['samples']['stack'])} samples x {interval} ms")
    shown = 0
    for name, count in counts.most_common():
        if pattern and not pattern.search(name):
            continue
        print(f"{count * interval:8.1f} ms  {name[:140]}")
        shown += 1
        if shown >= limit:
            break
