#!/usr/bin/env bash
# Creates the btrfs fixture of docs/btrfs.md §4 on a loop-mounted image and prints its path.
# Needs root (mkfs, mount). Usage: btrfs-fixture.sh <image> <mountpoint> [owner]
# The owner (default: $SUDO_USER, else root) owns everything, so tests can run as that user.
set -euo pipefail

img=$1
mnt=$2
owner=${3:-${SUDO_USER:-root}}

if mountpoint -q "$mnt/mnt" 2>/dev/null; then umount "$mnt/mnt"; fi
if mountpoint -q "$mnt" 2>/dev/null; then umount "$mnt"; fi
rm -f "$img"
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
# 8 MiB in 512 KiB extents, then 2 MiB rewritten at an unaligned offset: the extents at either
# end of the rewrite stay allocated in full though only part of each is referenced ("bookend"
# extents). FIEMAP sees the referenced parts only; the extent tree, as root, sees all of it.
# Compression off for this file, so its data is in large extents, not 128 KiB units.
touch plain/bookend.bin
btrfs property set plain/bookend.bin compression no
head -c 8M /dev/urandom >plain/bookend.bin
sync
# Unaligned, so the extents at both ends of the rewrite stay partly referenced.
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
echo "$mnt"
