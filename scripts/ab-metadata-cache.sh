#!/usr/bin/env bash
#
# Run one metadata-cache configuration end to end against the S3 test image.
#
#   ab-metadata-cache.sh TAG BINARY CHUNK_BYTES FETCH_BYTES
#
# Two walks per run. The first is cold: empty cache, everything comes from S3,
# and it builds the metadata tier. Then SIGUSR1 flips the reader into content
# phase, the kernel page cache is dropped, and the second walk re-reads the same
# metadata. Whether that second walk touches S3 at all is the measurement: it is
# what "the metadata cache held the whole working set" means operationally.
set -euo pipefail

TAG=${1:?tag}; BIN=${2:?binary}; CHUNK=${3:?chunk bytes}; FETCH=${4:?fetch bytes}

IMAGE="s3://digitalcorpora/corpora/drives/nps-2009-domexusers/nps-2009-domexusers.E01"
OFFSET=63                      # NTFS start sector, from mmls
SOCK=/tmp/dn.sock              # must stay short: longer paths exceed SUN_LEN
CACHE_DIR=/tmp/dn-cache
OUT="results/$TAG"
export AWS_REGION=us-east-1

: "${FLS:?run: export FLS=\$(nix-shell tools.nix --run 'which fls')}"
: "${NBDC:?run: export NBDC=\$(nix-shell tools.nix --run 'which nbd-client')}"

mkdir -p "$OUT"
rm -rf "${CACHE_DIR:?}"; mkdir -p "$CACHE_DIR"
rm -f "$SOCK"

"$BIN" "$IMAGE" --unix "$SOCK" \
  --metadata-cache \
  --cache-dir "$CACHE_DIR" \
  --cache-chunk-size "$CHUNK" \
  --cache-fetch-size "$FETCH" \
  --cache-trace-log "$OUT/trace.jsonl" \
  > "$OUT/server.log" 2>&1 &
SERVER=$!
trap 'sudo "$NBDC" -d /dev/nbd0 2>/dev/null || true; kill $SERVER 2>/dev/null || true' EXIT

# The socket is bound before the image is opened, so waiting on the socket file
# races the ~20 s E01 open. Wait for the server to say it is listening.
for _ in $(seq 1 300); do
  grep -q "listening on unix:" "$OUT/server.log" && break
  kill -0 "$SERVER" 2>/dev/null || { echo "server died during open" >&2; exit 1; }
  sleep 1
done
grep -q "listening on unix:" "$OUT/server.log" || { echo "server never listened" >&2; exit 1; }

sudo "$NBDC" -u -N "" "$SOCK" /dev/nbd0
drop_caches() { sync; echo 3 | sudo tee /proc/sys/vm/drop_caches > /dev/null; }

drop_caches
T0=$(date +%s.%N)
sudo "$FLS" -r -o "$OFFSET" /dev/nbd0 > "$OUT/walk1.txt" 2>"$OUT/walk1.err" || true
T1=$(date +%s.%N)

FLIP=$(date -u +%Y-%m-%dT%H:%M:%S.%NZ)
kill -USR1 "$SERVER"
sleep 2

drop_caches
T2=$(date +%s.%N)
sudo "$FLS" -r -o "$OFFSET" /dev/nbd0 > "$OUT/walk2.txt" 2>"$OUT/walk2.err" || true
T3=$(date +%s.%N)

# Bounded content hash: proves the coalescing path returns the same bytes on both
# builds. 512 MiB, one GiB in, to land in file data rather than the metadata the
# walk already pulled.
#
# It runs LAST, after both measured walks, and its start is timestamped. Run
# during the metadata phase it would have put 512 MiB of pure file content into
# the metadata tier and destroyed the footprint measurement; run between the flip
# and the re-walk it would have inflated the post-flip S3 count that IS the
# retention result. So it goes at the end, and the analyzer ignores everything
# at or after HASH_TS.
HASH_TS=$(date -u +%Y-%m-%dT%H:%M:%S.%NZ)
sudo dd if=/dev/nbd0 bs=4M count=536870912 skip=1073741824 \
  iflag=skip_bytes,count_bytes status=none | sha256sum > "$OUT/content.sha256"

sudo "$NBDC" -d /dev/nbd0
kill "$SERVER" 2>/dev/null || true
wait "$SERVER" 2>/dev/null || true
trap - EXIT

# bc is not present in this environment; awk does the float subtraction.
elapsed() { awk "BEGIN{printf \"%.3f\", $2 - $1}"; }

cat > "$OUT/timing.json" <<EOF
{"tag":"$TAG","binary":"$BIN","chunk":$CHUNK,"fetch":$FETCH,
 "walk1_s":$(elapsed "$T0" "$T1"),"walk2_s":$(elapsed "$T2" "$T3"),
 "flip_ts":"$FLIP","hash_ts":"$HASH_TS",
 "walk1_lines":$(wc -l < "$OUT/walk1.txt"),
 "walk2_lines":$(wc -l < "$OUT/walk2.txt")}
EOF
echo "== $TAG: cold $(elapsed "$T0" "$T1")s, re-walk $(elapsed "$T2" "$T3")s"
