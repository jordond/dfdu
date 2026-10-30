---
title: Command line
description: dfdu's commands for scripts and CI, with JSON output, size limits, snapshots and diffs.
---

```sh
dfdu scan ~                                 # print a summary
dfdu scan ~ --by-type                       # usage by file type
dfdu disks                                  # disks and free space, like df -h
dfdu dupes ~ --min-size 10MiB               # duplicate files
dfdu report ~/dev                           # what the cleanup rules found
dfdu report ~/dev --format json --fail-above 20GiB   # CI: exit 4 over the limit
dfdu clean ~/dev                            # dry run of the recommended deletes
dfdu clean ~/dev --yes                      # move them to the Trash
dfdu diff ~ --since 7d                      # what grew in the last week
dfdu export ~ --snapshot -o home.snap       # save a scan
dfdu -f home.snap                           # browse a saved scan or an ncdu export
```

`clean` changes nothing without `--yes`. JSON output is versioned; the schemas are in [`schema/`](https://github.com/jordond/dfdu/tree/main/schema). `dfdu --help` lists every option and exit code.

## Look

![dfdu scan printing a summary](/demos/scan-cli.gif)

## Clean

![dfdu report listing what the cleanup rules found](/demos/report.gif)

`dfdu report` lists what the [cleanup rules](/docs/rules/) found. With `--fail-above`, it exits with 4 when the recommended deletes add up to more than the limit, which makes it a CI check.

## Over time

![dfdu diff showing what grew](/demos/diff.gif)

Every scan is recorded, which is what `dfdu diff` compares against.
