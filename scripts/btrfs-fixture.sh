#!/usr/bin/env bash
# Creates the btrfs test fixture on a loop-mounted image and prints its path;
# also a small filesystem with the DUP data profile at <mountpoint>-dup.
# Needs root (mkfs, mount). Usage: btrfs-fixture.sh <image> <mountpoint> [owner]
# The owner (default: $SUDO_USER, else root) owns everything, so tests can run as that user.
set -euo pipefail

img=$1
mnt=$2
owner=${3:-${SUDO_USER:-root}}

for m in "$mnt/mnt" "$mnt" "$mnt-dup"; do
    if mountpoint -q "$m" 2>/dev/null; then umount "$m"; fi
done
rm -f "$img" "$img-dup"
truncate -s 2G "$img"
mkfs.btrfs -q -f "$img"
mkdir -p "$mnt"
mount -o loop,compress-force=zstd "$img" "$mnt"

cd "$mnt"
mkdir plain
head -c 16M /dev/urandom >plain/random.bin
# 384 compressed extents of 128 KiB: more than one FIEMAP batch.
yes "the quick brown fox jumps over the lazy dog" | head -c 48M >plain/text.txt || true
cp --reflink=always plain/random.bin plain/reflink.bin
# Overwrite 4 MiB in the middle of the copy: the copy then shares only part of the original.
dd if=/dev/urandom of=plain/reflink.bin bs=1M count=4 seek=4 conv=notrunc status=none
echo tiny >plain/tiny.txt
fallocate -l 4M plain/prealloc.bin
truncate -s 1G plain/sparse.bin
mkdir plain/dir
cp --reflink=always plain/random.bin plain/dir/copy.bin
# Shared compressed extents: FIEMAP gives each the uncompressed length from its physical start,
# so their ranges overlap the extents stored after them.
cp --reflink=always plain/text.txt plain/dir/text-copy.txt
# 8 MiB in 512 KiB extents, then 2 MiB rewritten at an unaligned offset: the extents at either
# end of the rewrite stay allocated in full though only part of each is referenced ("bookend"
# extents). FIEMAP sees the referenced parts only; the extent tree, as root, sees all of it.
# Compression off for this file, so its data is in large extents, not 128 KiB units.
touch plain/bookend.bin
btrfs property set plain/bookend.bin compression no
head -c 8M /dev/urandom >plain/bookend.bin
sync
dd if=/dev/urandom of=plain/bookend.bin bs=256K count=8 seek=13 conv=notrunc status=none

btrfs -q subvolume create sub
head -c 8M /dev/urandom >sub/a.bin
mkdir sub/d
head -c 2M /dev/urandom >sub/d/b.bin
btrfs -q subvolume create sub/nested
head -c 2M /dev/urandom >sub/nested/n.bin
# Everything belongs to the owner, including what the read-only snapshot captures.
mkdir mnt
chown -R "$owner" "$mnt"
btrfs -q subvolume snapshot -r sub snap-ro
btrfs -q subvolume snapshot sub snap-rw
# Data that only the read-write snapshot has.
head -c 1M /dev/urandom >snap-rw/only-here.bin
chown "$owner" snap-rw/only-here.bin

# The subvolume mounted a second time inside the tree: a real mount boundary.
mount -o loop,subvol=sub "$img" "$mnt/mnt"
sync
btrfs filesystem sync "$mnt"
# Quota groups, for the per-subvolume numbers shown as root.
btrfs quota enable "$mnt"
btrfs quota rescan -w "$mnt" >/dev/null

# A second, small filesystem with the DUP data profile (every byte stored twice), next to the
# fixture at "$mnt-dup", for the header that states the raw multiplier.
truncate -s 256M "$img-dup"
mkfs.btrfs -q -f -d dup -m dup "$img-dup"
mkdir -p "$mnt-dup"
mount -o loop "$img-dup" "$mnt-dup"
head -c 1M /dev/urandom >"$mnt-dup/data.bin"
chown -R "$owner" "$mnt-dup"
sync
echo "$mnt"
