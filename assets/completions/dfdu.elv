
use builtin;
use str;

set edit:completion:arg-completer[dfdu] = {|@words|
    fn spaces {|n|
        builtin:repeat $n ' ' | str:join ''
    }
    fn cand {|text desc|
        edit:complex-candidate $text &display=$text' '(spaces (- 14 (wcswidth $text)))$desc
    }
    var command = 'dfdu'
    for word $words[1..-1] {
        if (str:has-prefix $word '-') {
            break
        }
        set command = $command';'$word
    }
    var completions = [
        &'dfdu'= {
            cand --resume-from 'Open the scan saved in FILE (a snapshot or an ncdu export) at once, and rescan its directory in the background, replacing it when done'
            cand -f 'Browse a scan exported to FILE (ncdu JSON, plain or zstd; `-` for stdin), read-only'
            cand --file 'Browse a scan exported to FILE (ncdu JSON, plain or zstd; `-` for stdin), read-only'
            cand --theme 'Colour theme: auto, dark, light, ansi (the terminal''s own 16 colours), mono, high-contrast, or NAME for the theme file NAME.toml in dfdu/themes in the configuration directory. auto asks the terminal for its background colour at start (up to 1 s if it does not answer) and picks dark or light, or ansi when the colour is unknown'
            cand --color 'Colours: auto (from the terminal and NO_COLOR), never, 16, 256 or truecolor. A level given here is used even when NO_COLOR is set'
            cand -x 'Stay on the filesystem of the scanned directory (the default; overrides --cross-mounts and --network given before it)'
            cand --one-file-system 'Stay on the filesystem of the scanned directory (the default; overrides --cross-mounts and --network given before it)'
            cand --cross-mounts 'Descend into other local filesystems (disks, tmpfs)'
            cand --network 'Descend into network filesystems (NFS, SMB, sshfs and other FUSE network mounts); with or without --cross-mounts'
            cand --no-rules 'Do not classify entries: no categories, no cleanup candidates'
            cand --disks 'Choose a disk to browse from the mounted filesystems'
            cand --read-only 'Never delete anything (the delete keys are refused)'
            cand --no-history 'Do not record the scan in the history, nor show what grew since the last one'
            cand --no-mouse 'Do not capture the mouse (click to select, double-click to open, wheel to scroll). While it is captured, most terminals select text with Shift held down'
            cand --debug-stats 'Start with the debug panel open, and print its numbers when the browser exits'
            cand -h 'Print help (see more with ''--help'')'
            cand --help 'Print help (see more with ''--help'')'
            cand -V 'Print version'
            cand --version 'Print version'
            cand scan 'Scan a directory and print a summary'
            cand disks 'List the mounted filesystems with their size and free space'
            cand report 'Scan a directory and list what the cleanup rules found in it: caches, build output, dependencies, with how safe each is to delete'
            cand clean 'Delete what `report` recommends (dry run unless --yes)'
            cand diff 'What grew: a directory now against its last recorded scan (or one from --since ago), or two saved scans'
            cand history 'List the recorded scans'
            cand export 'Scan a directory and write it in ncdu''s JSON format'
            cand import 'Read a scan in ncdu''s JSON format and print a summary (`dfdu -f FILE` browses it)'
            cand recover 'List what earlier sessions left unfinished (staged items, interrupted deletes), and finish, restore or dismiss them'
            cand man 'Write man pages (dfdu.1 and one per command) into DIR, for packagers'
            cand completions 'Print a shell completion script (bash, zsh, fish, elvish, powershell)'
            cand help 'Print this message or the help of the given subcommand(s)'
        }
        &'dfdu;scan'= {
            cand --format 'Output: table (for people), json (one document, schema/scan.v1.json), ndjson (one record per line, for large trees) or ncdu (ncdu''s JSON export)'
            cand --depth 'Levels of directories to print below the root [default: 1; every level for ndjson]'
            cand --top 'Entries to print per directory, largest first [default: 10; all for ndjson]'
            cand --threads 'Worker threads for the scanned filesystem. Mounts below it with their own limit (spinning disks: 2, network: 4) add that many'
            cand --measure-workers 'Threads measuring shared extents (btrfs, XFS); 0 reports sizes as du does'
            cand --stall-abandon 'Abandon directories stalled this many seconds'
            cand --stall-after 'Report a directory as stalled after one call on it took this many seconds (tests)'
            cand --backend 'Filesystem access: `native` (the fastest for this system) or `portable` (std::fs, the reference the native backends are tested against)'
            cand --apparent 'Show apparent sizes instead of disk usage'
            cand --like-du 'Count data shared between files (btrfs snapshots, reflinks) once per file, as du does, instead of once'
            cand -x 'Stay on the filesystem of the scanned directory (the default; overrides --cross-mounts and --network given before it)'
            cand --one-file-system 'Stay on the filesystem of the scanned directory (the default; overrides --cross-mounts and --network given before it)'
            cand --cross-mounts 'Descend into other local filesystems (disks, tmpfs)'
            cand --network 'Descend into network filesystems (NFS, SMB, sshfs and other FUSE network mounts); with or without --cross-mounts'
            cand --no-rules 'Do not classify entries: no categories, no cleanup candidates'
            cand --bytes 'Print only the root total in bytes (for scripts and tests)'
            cand --errors 'List every entry that could not be read, with the reason'
            cand --no-history 'Do not record the scan in the history (`dfdu diff`, `dfdu history`)'
            cand --debug-stats 'Print pipeline counters and applier timing (lock hold and wait) after the scan'
            cand -h 'Print help'
            cand --help 'Print help'
        }
        &'dfdu;disks'= {
            cand -h 'Print help'
            cand --help 'Print help'
        }
        &'dfdu;report'= {
            cand --min-size 'Only matches at least this big (`100M`, `1.5GiB`, `500 KB`)'
            cand --category 'Only these categories (comma separated): an id such as `dev.build`, or its first part (`dev`) for all of them'
            cand --older-than 'Only matches unchanged for this long (`30d`, `2w`, `12h`; a bare number is days)'
            cand --safety 'Only matches this safe or safer: safe, regenerable, review, manual or protected (all)'
            cand --fail-above 'Exit with code 4 when the matches listed, protected ones left out, add up to more than this (`5GiB`), for CI'
            cand --format 'Output: table or json (schema/report.v1.json)'
            cand --threads 'Worker threads'
            cand -x 'Stay on the filesystem of the scanned directory (the default; overrides --cross-mounts and --network given before it)'
            cand --one-file-system 'Stay on the filesystem of the scanned directory (the default; overrides --cross-mounts and --network given before it)'
            cand --cross-mounts 'Descend into other local filesystems (disks, tmpfs)'
            cand --network 'Descend into network filesystems (NFS, SMB, sshfs and other FUSE network mounts); with or without --cross-mounts'
            cand -h 'Print help'
            cand --help 'Print help'
        }
        &'dfdu;clean'= {
            cand --min-size 'Only matches at least this big (`100M`, `1.5GiB`, `500 KB`)'
            cand --category 'Only these categories (comma separated): an id such as `dev.build`, or its first part (`dev`) for all of them'
            cand --older-than 'Only matches unchanged for this long (`30d`, `2w`, `12h`; a bare number is days)'
            cand --safety 'Matches this safe or safer are considered: safe, regenerable or review. Manual and protected matches are never deleted'
            cand --mode 'trash (the default; frees no space until the trash is emptied), stage (moved aside at once, then deleted; what is left after a crash is offered by `dfdu recover`) or delete (permanently)'
            cand --format 'Output: table or json (schema/clean.v1.json)'
            cand --threads 'Worker threads'
            cand --all 'Every match that passes the filters, not only the recommended ones (recommended: `safe` above the rule''s minimum size, `regenerable` also unchanged for the rule''s minimum age, nothing changed in the last 7 days)'
            cand --yes 'Delete. Without it nothing is changed: the plan is printed'
            cand --allow-commands 'Run the owning tool''s cleanup command for matches cleaned up that way (`docker system prune`, `go clean -cache`). Without it they are skipped'
            cand --finish-staged 'First delete what earlier sessions left staged (`dfdu recover` lists it)'
            cand --restore-staged 'First put back what earlier sessions left staged'
            cand -x 'Stay on the filesystem of the scanned directory (the default; overrides --cross-mounts and --network given before it)'
            cand --one-file-system 'Stay on the filesystem of the scanned directory (the default; overrides --cross-mounts and --network given before it)'
            cand --cross-mounts 'Descend into other local filesystems (disks, tmpfs)'
            cand --network 'Descend into network filesystems (NFS, SMB, sshfs and other FUSE network mounts); with or without --cross-mounts'
            cand -h 'Print help'
            cand --help 'Print help'
        }
        &'dfdu;diff'= {
            cand --since 'Compare with the recorded scan closest to this long ago (`7d`, `2w`, `12h`) instead of the last one'
            cand --depth 'Levels below the root to list'
            cand --top 'Changes to list, largest first'
            cand --min-change 'Leave out changes smaller than this (`1M`, `500K`)'
            cand --format 'Output: table or json (schema/diff.v1.json)'
            cand --threads 'Worker threads'
            cand -x 'Stay on the filesystem of the scanned directory (the default; overrides --cross-mounts and --network given before it)'
            cand --one-file-system 'Stay on the filesystem of the scanned directory (the default; overrides --cross-mounts and --network given before it)'
            cand --cross-mounts 'Descend into other local filesystems (disks, tmpfs)'
            cand --network 'Descend into network filesystems (NFS, SMB, sshfs and other FUSE network mounts); with or without --cross-mounts'
            cand -h 'Print help'
            cand --help 'Print help'
        }
        &'dfdu;history'= {
            cand -h 'Print help'
            cand --help 'Print help'
        }
        &'dfdu;export'= {
            cand -o 'Where to write (`-` for stdout)'
            cand --output 'Where to write (`-` for stdout)'
            cand --threads 'Worker threads'
            cand --compress 'Compress with zstd (ncdu reads it with `-f`; dfdu recognises it on import)'
            cand --snapshot 'Write dfdu''s own snapshot format instead (lossless, checked, compressed; for `dfdu -f` and `--resume-from`, not for ncdu)'
            cand -x 'Stay on the filesystem of the scanned directory (the default; overrides --cross-mounts and --network given before it)'
            cand --one-file-system 'Stay on the filesystem of the scanned directory (the default; overrides --cross-mounts and --network given before it)'
            cand --cross-mounts 'Descend into other local filesystems (disks, tmpfs)'
            cand --network 'Descend into network filesystems (NFS, SMB, sshfs and other FUSE network mounts); with or without --cross-mounts'
            cand -h 'Print help'
            cand --help 'Print help'
        }
        &'dfdu;import'= {
            cand --depth 'Levels of directories to print below the root'
            cand --top 'Entries to print per directory, largest first'
            cand --apparent 'Show apparent sizes instead of disk usage'
            cand --bytes 'Print only the root total in bytes'
            cand -h 'Print help'
            cand --help 'Print help'
        }
        &'dfdu;recover'= {
            cand --finish 'Delete what is left of these staged items (numbers from the list)'
            cand --restore 'Put these staged items back at their paths (next to them as NAME.restored-N if something is there now), or take these items found in the trash back out of it'
            cand --dismiss 'Close these interrupted items: nothing is done on disk'
            cand --journals 'The journal directory (the default is dfdu''s state directory)'
            cand -h 'Print help'
            cand --help 'Print help'
        }
        &'dfdu;man'= {
            cand -h 'Print help'
            cand --help 'Print help'
        }
        &'dfdu;completions'= {
            cand -h 'Print help'
            cand --help 'Print help'
        }
        &'dfdu;help'= {
            cand scan 'Scan a directory and print a summary'
            cand disks 'List the mounted filesystems with their size and free space'
            cand report 'Scan a directory and list what the cleanup rules found in it: caches, build output, dependencies, with how safe each is to delete'
            cand clean 'Delete what `report` recommends (dry run unless --yes)'
            cand diff 'What grew: a directory now against its last recorded scan (or one from --since ago), or two saved scans'
            cand history 'List the recorded scans'
            cand export 'Scan a directory and write it in ncdu''s JSON format'
            cand import 'Read a scan in ncdu''s JSON format and print a summary (`dfdu -f FILE` browses it)'
            cand recover 'List what earlier sessions left unfinished (staged items, interrupted deletes), and finish, restore or dismiss them'
            cand man 'Write man pages (dfdu.1 and one per command) into DIR, for packagers'
            cand completions 'Print a shell completion script (bash, zsh, fish, elvish, powershell)'
            cand help 'Print this message or the help of the given subcommand(s)'
        }
        &'dfdu;help;scan'= {
        }
        &'dfdu;help;disks'= {
        }
        &'dfdu;help;report'= {
        }
        &'dfdu;help;clean'= {
        }
        &'dfdu;help;diff'= {
        }
        &'dfdu;help;history'= {
        }
        &'dfdu;help;export'= {
        }
        &'dfdu;help;import'= {
        }
        &'dfdu;help;recover'= {
        }
        &'dfdu;help;man'= {
        }
        &'dfdu;help;completions'= {
        }
        &'dfdu;help;help'= {
        }
    ]
    $completions[$command]
}
