#!/usr/bin/env python3
"""Convert a `{nodes:[{id,..}], edges:[{from,to,type?}]}` dump to nebula's JSON.

Usage:  python3 fromto-to-nebula.py IN.json OUT.json [--group FIELD]

Edges are grouped by `--group` (default `type`) into separately colored, toggleable
edge sets. Every scalar node field is carried through as an attribute, so
--color-attr / --filter / the inspect panel work. Field names the panel treats as
a title (`type`/`name`/`label`/`ty`) are renamed and long ones truncated, so the
node id stays the panel title and the panel stays narrow.
"""
import json
import sys

# Reserved by nebula's inspect panel as the panel title; rename so the id wins.
TITLE_KEYS = {"ty": "ty_", "type": "node_type", "name": "desc", "label": "text"}
MAX_ATTR = 100
PALETTE = [[90, 150, 255], [255, 150, 60], [120, 215, 130],
           [220, 110, 200], [240, 220, 90], [150, 150, 150]]


def main():
    argv = sys.argv[1:]
    group = "type"
    if "--group" in argv:
        i = argv.index("--group")
        group = argv[i + 1]
        del argv[i:i + 2]
    if len(argv) != 2:
        sys.exit("usage: fromto-to-nebula.py IN.json OUT.json [--group FIELD]")
    src, dst = argv
    doc = json.load(open(src))

    out_nodes = []
    for n in doc["nodes"]:
        nn = {"id": n["id"]}
        for k, v in n.items():
            if k == "id" or isinstance(v, (list, dict)):
                continue
            if isinstance(v, str) and len(v) > MAX_ATTR:
                v = v[:MAX_ATTR - 1] + "…"
            nn[TITLE_KEYS.get(k, k)] = v
        out_nodes.append(nn)

    sets = {}
    for e in doc["edges"]:
        sets.setdefault(e.get(group, "edges"), []).append([e["from"], e["to"]])
    edge_types = [{"name": name, "color": PALETTE[i % len(PALETTE)], "edges": es}
                  for i, (name, es) in enumerate(sorted(sets.items()))]

    json.dump({"nodes": out_nodes, "edge_types": edge_types}, open(dst, "w"))
    print(f"nodes {len(out_nodes)}  "
          + "  ".join(f"{t['name']} {len(t['edges'])}" for t in edge_types))
    print("wrote", dst)


if __name__ == "__main__":
    main()
