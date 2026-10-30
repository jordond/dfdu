# Demo recordings

The GIFs in `assets/demos/` are recorded with [VHS](https://github.com/charmbracelet/vhs) from
the tapes in `tapes/`, against made-up data: `dfdu-demofs` (`crates/dfdu-demofs`) mounts a
fixture from `fixtures/` as the demo user's home inside a container, so every recording shows
the same files, sizes and ages, and nothing of the machine it is made on.

```sh
cargo xtask demo              # record every tape
cargo xtask demo browse dupes # only these
cargo xtask demo --list
cargo xtask demo --format mp4 --format webm   # videos instead of GIFs (dfdu.app plays them)
```

Needs Docker (privileged containers, for the FUSE mount). The binaries are built for Linux in
the `rust` image; `--bin-dir DIR` uses ones built already. Docker can be remote
(`DOCKER_HOST=ssh://host`) when `--bin-dir` is given, since nothing is bind-mounted then.

- `Dockerfile`: VHS, libfaketime and a `demo` user.
- `run.sh`: the container's entry point; builds the recording's root and records the tape.
- `env.sh`: sourced by `tapes/_setup.tape`; continues as the demo user, chrooted into that
  root, at the fixture's time.
- `fixtures/*.toml`: the trees served. The format, fill styles and how to make dfdu's rules
  match are in [`crates/dfdu-demofs/README.md`](../crates/dfdu-demofs/README.md).
  `cargo run -p dfdu-demofs -- check FIXTURE` prints what one adds up to.
- `tapes/NAME.tape`: one recording each. A tape writes `out/NAME.gif` (copied to
  `assets/demos/`; with `--format`, `run.sh` records a copy of the tape that writes those
  formats instead) and sources `tapes/_setup.tape` first. Comment lines set it up:

  | line | does |
  |---|---|
  | `# fixture: NAME` | serves `fixtures/NAME.toml` as the home (default `home`) |
  | `# mount: NAME PATH` | also serves `fixtures/NAME.toml` at PATH, a disk of its own (`disks`, `remote`) |
  | `# history: NAME AGE` | first records a scan of `~` served from `fixtures/NAME.toml`, AGE (`7d`) before the fixture's time, for `dfdu diff` and the browser's growth notes (`diff`) |

## The recording's root

The shell a tape types into runs in a root of its own, so the disk list (`dfdu disks`, the
`--disks` picker) shows made-up disks and none of the Docker host's. `run.sh` mounts at
`/newroot`:

- `fixtures/system.toml` with the tape's fixture at `home/demo`, as one dfdu-demofs mount: `/`
  is the tape fixture's disk. `system.toml` imports from the image only what the shell needs
  (bash, clear, dfdu, glibc, libfaketime, terminfo); a tape that types another command needs
  it added there.
- the fixtures named in the tape's `# mount:` lines, each a disk of its own.
- `proc` at `/proc`, `devtmpfs` at `/dev`, the container's `/dev/pts`, and a tmpfs at
  `/dev/shm` that holds dfdu's history (`DFDU_STATE_DIR`, kept from the `# history:` scan to
  the recording). The disk list leaves out kernel filesystems and what is mounted below
  them.

`env.sh` then `chroot`s into it as the demo user. VHS itself stays in the container's root.
