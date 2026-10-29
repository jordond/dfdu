# dfdu-demofs

A FUSE filesystem that serves a made-up home directory, so the demo recordings show the same
files, sizes and ages on every run and nothing from the machine they are made on. dfdu itself
is not changed: the real binary scans, deletes and finds duplicates on this filesystem as it
would on a disk.

- Sizes cost nothing. A file's length and allocated bytes are whatever the fixture says, so a
  1 TB disk with 86 GiB of projects fits in a few MB of memory.
- File bytes are generated from a seed when read. Files with the same `content` read the same
  bytes, so `dfdu dupes` and the duplicates view find them.
- The tree can be changed: delete, rename, the Trash, staging and small writes all work, in
  memory. Remounting starts again from the fixture.
- Directories can answer late (`delay`) to make a live scan visible, or not at all (`error`)
  for the `!` view.

It is not published and builds on every platform, but mounting needs Linux. The recordings run
in Docker (`demo/`), so macOS works through Docker.

## Recording the demos

Most of the time you only need the runner, which builds everything and records each tape in its
own container:

```sh
cargo xtask demo              # every tape in demo/tapes, into assets/demos/
cargo xtask demo browse dupes # only these
cargo xtask demo --list
```

See [`demo/README.md`](../../demo/README.md) for the container, the tapes and remote Docker.

## The command

```sh
dfdu-demofs check FIXTURE              # build it and print entries, apparent size, allocated bytes
dfdu-demofs now FIXTURE [--minus 7d]   # the fixture's time as FAKETIME takes it: @2026-10-01 09:00:00
dfdu-demofs mount FIXTURE DIR [--ready FILE]   # serve FIXTURE at DIR until unmounted (Linux)
dfdu-demofs mount FIXTURE DIR --root BASE --at home/demo   # FIXTURE inside BASE's tree
```

`check` runs anywhere and is the quick way to try a fixture while writing it:

```sh
cargo run -p dfdu-demofs -- check demo/fixtures/home.toml
```

`mount` needs `/dev/fuse` and root (or `fusermount3`). The mount is open to every user
(`allow_other`), shows up in the mount table as `fuse.dfdu-demofs` (which dfdu treats as a
local disk), and stays in the foreground until `umount DIR`. `--ready` creates a file once the
mount answers, for scripts to wait on.

To poke at a fixture by hand on Linux, or inside a privileged container:

```sh
mkdir -p /tmp/home
dfdu-demofs mount demo/fixtures/home.toml /tmp/home --ready /tmp/ready &
until [ -e /tmp/ready ]; do sleep 0.1; done
FAKETIME="$(dfdu-demofs now demo/fixtures/home.toml)" \
  LD_PRELOAD=/usr/lib/x86_64-linux-gnu/faketime/libfaketime.so.1 TZ=UTC \
  dfdu /tmp/home
umount /tmp/home
```

Without libfaketime the scan still works, but ages count from the real date rather than the
fixture's `now`.

With `--root BASE --at PATH` the mount serves BASE's tree with FIXTURE's at PATH in it, as one
filesystem. The disk (`statfs`, the mount's source) and the clock are FIXTURE's. The recordings
use this to give the shell a root of its own (`demo/fixtures/system.toml`, with the home at
`home/demo`), so that `/` is the fixture's disk.

## Fixtures

A fixture is a TOML file. Paths are relative to the mount point, and parent directories are
made as needed.

```toml
now = "2026-10-01T09:00:00Z"   # when the recording runs; every age counts back from it
age = "45d"                    # age of entries that do not set one (default 30d)
uid = 1000                     # owner of the fixture's entries (default 1000)
gid = 1000
import = ["/usr/bin/bash"]     # real files copied in at the same path (below)

[disk]                         # what statfs and the mount table show
total = "1TB"
free = "212GB"                 # free space with the fixture's files on it; deleting adds to it
device = "/dev/nvme0n1p2"      # the mount's source

[[entry]]
path = "Downloads/ubuntu-24.04.1-desktop-amd64.iso"
size = "5.8GiB"
age = "200d"

[[entry]]
path = "dev/web-shop/node_modules"
age = "60d"
fill = { files = 2400, size = "1.8GiB", style = "npm", seed = 1 }
```

### Entry keys

| key | meaning |
|---|---|
| `path` | where the entry goes, relative to the root. Required. |
| `kind` | `file`, `dir` or `symlink`. Defaults to `dir` with `fill`, `symlink` with `target`, else `file`. A `dir` entry for a directory an earlier entry already made just sets its attributes. |
| `size` | a file's length: bytes, or text such as `1.4GiB`, `500MB`, `412KiB` (`KB`/`MB`/`GB`/`TB` are powers of 1000, `KiB`/`MiB`/`GiB`/`TiB` of 1024). |
| `alloc` | bytes allocated on disk, when not the size rounded up to 4 KiB. Smaller than `size` makes a sparse file (`size = "6GiB"`, `alloc = "2.2GiB"`). |
| `age` | how long before `now` it was modified: `30s`, `45m`, `6h`, `120d`, `3w`, `2y`. |
| `content` | an id for the file's bytes. Files with the same `content` and size read the same, so they are duplicates. Without it every file's bytes are its own. |
| `text` | the file's bytes, literally. Use it for marker files rules look at (`package.json`, `Cargo.toml`, `pyvenv.cfg`, `CACHEDIR.TAG`). Use a basic string (`"..."`) for `\n` escapes. |
| `target` | a symlink's target. A target that does not exist makes a broken link. |
| `link` | the path of an earlier file entry: this entry is a hard link to it. |
| `mode` | permission bits, e.g. `0o600`. |
| `error` | `denied`: a directory that cannot be listed (`EACCES`). `io`: a file that cannot be stat'ed (`EIO`). Both show in dfdu's `!` list. |
| `delay` | a directory's listing opens this late (`300ms`, `2s`), so the live scan has something to show. |
| `fill` | bulk content for a directory, below. |

Unknown keys are errors, as are paths through a file, a path given twice, a `link` to a path
that is not an earlier file, sizes or durations that overflow, and dates that do not exist.

### Extending another fixture

A fixture can start from another and say only what differs, as `home-week-ago.toml` does for
the history the `diff` recording compares with:

```toml
extends = "home.toml"                  # relative to this file
remove = ["dev/web-shop/.next"]        # inherited entries to leave out, with all below them

[[entry]]                              # the same path as an inherited entry: replaces it
path = "dev/indexer/target/debug"
fill = { files = 900, size = "3.1GiB", style = "cargo", seed = 4 }
```

Keys it does not set (`now`, `age`, `uid`, `gid`, `[disk]`) are the other fixture's, imports
add to the other's, and an entry replaces the inherited one with its `path` whole, in its
place, so listings keep their order. Other entries are added after the inherited ones. A
`remove` that matches nothing is an error, as are fixtures that extend one another in a loop.

### Imports

`import` lists real paths, read when the fixture is built and served at the same path below
the fixture's root: files with their bytes and permission bits, symlinks as they are (not
followed), directories with everything below them. They are owned by the fixture's `uid` and
get its default age. A symlink's target is not imported with it; list both. Imports come
before the entries, so an entry can set the attributes of an imported directory. This is for
a root a shell can run in (`demo/fixtures/system.toml`); a missing path is an error, so `check`
of such a fixture fails outside the recording image.

### Fill

`fill = { files, size, style, seed, spread }` adds `files` files to the entry's directory,
adding up to exactly `size`. Sizes are skewed the way real trees are: a few large files and
many small ones. The same fill always makes the same files, and `seed` gives another set.
File ages run from the directory's age back by up to `spread` (default `30d`).

| `style` | looks like |
|---|---|
| `generic` (default) | `file-0001.dat` |
| `npm` | packages of a `node_modules`: `react/package.json`, `react/dist/index.js`, ... |
| `cargo` | a Cargo `target/<profile>`: `deps/libserde-<hash>.rlib`, `.rmeta`, `incremental/...` |
| `python` | a `site-packages`: `numpy/__init__.py`, `numpy/_core.cpython-313-....so`, `*.dist-info/` |
| `photos` | a camera roll: `IMG_4000.HEIC`, every seventh a `.MOV` |
| `cache` | content-addressed blobs: `3f/a9c2...` |

### Making dfdu's rules match

The cleanup rules look at names, ages, sizes and marker files, so a fixture has to provide
them:

- `node_modules` needs a `package.json` beside it, and a lockfile to be rated "regenerable"
  rather than "review". The age must be over 30 days and the size over 50 MiB.
- A Cargo `target/` needs `Cargo.toml` beside it and one of `CACHEDIR.TAG`, `debug` or
  `release` inside. It must be over 14 days old.
- A `.venv` needs `pyvenv.cfg` inside.
- Caches are matched by place: `.cache/pip`, `.npm/_cacache`, `.cargo/registry`,
  `.rustup/toolchains/*`, `.local/share/containers/storage` and so on.

`crates/dfdu-rules/src/builtin.toml` has every rule. To see what a fixture matches, run
`dfdu report ~` in the container.

## Determinism

The same fixture always builds the same tree: names, sizes, ages, inode numbers and bytes come
only from the fixture. Together with libfaketime set to the fixture's `now` (the recording
image does this), a recording shows the same numbers and dates every time it is made. Changes
made while mounted are stamped with `now` plus the time since mounting, and owned by the
user who made them.

## Tests

```sh
cargo test -p dfdu-demofs                      # fixture parsing and the tree, any platform
cargo xtask linux --privileged -- cargo test -p dfdu-demofs   # also mounts it (Linux, root)
```

`tests/mount.rs` mounts a fixture and checks what `stat`, `read`, `rm -r`, `rename` and
`statvfs` see through the kernel. It skips without `/dev/fuse` and root, unless
`DFDU_FUSE_REQUIRED` is set, as in CI's FUSE job.

## Limits

- An unlinked file is gone at once, even if it is still open.
- Writes are kept in memory and refused for generated files over 64 MiB, which is enough for
  the Trash's `.trashinfo` files and dfdu's staging.
- No extended attributes, locks or special files.
