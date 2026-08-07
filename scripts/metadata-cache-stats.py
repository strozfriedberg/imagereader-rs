#!/usr/bin/env python3
"""Summarize one metadata-cache A/B run.

Two different questions, two different sources:

  How big did the metadata tier get?  On the new build that is the number of
  distinct blocks logged as md_insert.  On the old build there are no such
  records and there do not need to be: in its metadata phase every block of
  every fetch group entered the metadata tier, so the footprint is exactly the
  union of the fetched S3 ranges, cut into blocks.  Both are computed here and
  the old-build figure is reported as the union.

  Did the metadata cache actually hold the working set?  Count S3 fetches after
  the SIGUSR1 flip.  A re-walk that touches S3 is a re-walk the metadata tier
  failed to serve.

Usage: metadata-cache-stats.py results/TAG [results/TAG ...]
"""

import json
import sys
from pathlib import Path

MIB = 1024 * 1024


def load(path):
    recs = []
    for line in path.open():
        i = line.find("{")
        if i < 0:
            continue
        try:
            recs.append(json.loads(line[i:]))
        except json.JSONDecodeError:
            pass
    return recs


def blocks_of(beg, end, chunk):
    """The aligned cache blocks a fetched range [beg, end) fills."""
    first = (beg // chunk) * chunk
    return range(first, end, chunk)


def summarize(run_dir):
    run = Path(run_dir)
    timing = json.loads((run / "timing.json").read_text())
    chunk = timing["chunk"]
    flip = timing["flip_ts"]
    # The content hash runs after both walks; everything from there on is a
    # deliberate 512 MiB sequential read and belongs to neither measurement.
    hash_ts = timing.get("hash_ts", "9999")
    recs = load(run / "trace.jsonl")

    # Records carry an RFC3339 "ts"; the flip and hash timestamps are written in
    # the same format, so string comparison orders them correctly.
    pre = [r for r in recs if r.get("ts", "") < flip]
    post = [r for r in recs if flip <= r.get("ts", "") < hash_ts]

    pre_s3 = [r for r in pre if r.get("kind") == "s3"]
    post_s3 = [r for r in post if r.get("kind") == "s3"]

    fetched_blocks = set()
    for r in pre_s3:
        for b in blocks_of(r["beg"], r["end"], chunk):
            fetched_blocks.add((r["segment"], b))

    md_blocks = {
        (r["segment"], r["block"]) for r in pre if r.get("kind") == "md_insert"
    }
    promoted = sum(
        1 for r in pre if r.get("kind") == "md_insert" and r.get("promoted")
    )
    traced = bool(md_blocks)

    # No md_insert records means the old build, where the metadata tier received
    # every fetched block.
    md_footprint = (len(md_blocks) if traced else len(fetched_blocks)) * chunk

    print(f"== {timing['tag']}  chunk={chunk // 1024}K fetch={timing['fetch'] // MIB}M")
    print(f"   cold walk        {timing['walk1_s']:>8.1f} s   ({timing['walk1_lines']} lines)")
    print(f"   re-walk          {timing['walk2_s']:>8.1f} s   ({timing['walk2_lines']} lines)")
    print(f"   S3 GETs (walk 1) {len(pre_s3):>8}")
    print(f"   S3 bytes         {sum(r['bytes'] for r in pre_s3) / MIB:>8.1f} MiB")
    print(f"   content tier     {len(fetched_blocks) * chunk / MIB:>8.1f} MiB (union of fetches)")
    print(f"   METADATA TIER    {md_footprint / MIB:>8.1f} MiB"
          f"   [{'measured' if traced else 'inferred: == fetch union'}]")
    if traced:
        print(f"     of which promoted {promoted} inserts")
    print(f"   RETENTION: S3 GETs after flip {len(post_s3)}"
          f"  ({sum(r['bytes'] for r in post_s3) / MIB:.1f} MiB)")
    if len(post_s3) == 0:
        print("     -> metadata tier served the entire re-walk")
    print()


if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    for d in sys.argv[1:]:
        summarize(d)
