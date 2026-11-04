# Changelog

Notable changes to dfdu, newest first. Generated from the commits by git-cliff and edited by hand.

## [0.5.2] - 2026-11-04

### Added

- **website:** offer the gui download for the visitor's platform

### Fixed

- **scan:** classify the macOS data volume through its firmlinks ([#27](https://github.com/jordond/dfdu/pull/27))

## [0.5.1] - 2026-11-04

### Added

- **gui:** ship the desktop app for Linux arm64: a .deb and an AppImage, which `install.sh --gui` and the GUI's updater pick for aarch64 ([#26](https://github.com/jordond/dfdu/pull/26))

## [0.5.0] - 2026-11-04

### Added

- Go to a folder and up past the scan root ([#22](https://github.com/jordond/dfdu/pull/22))
- List folders and mounts on a remote host and reuse the scan when going up ([#23](https://github.com/jordond/dfdu/pull/23))

## [0.4.0] - 2026-11-03

### Added

- Add dfdu update, update checks in the GUI, and installing the CLI from the GUI ([#17](https://github.com/jordond/dfdu/pull/17))
- **gui:** scan another computer over ssh from the start screen ([#19](https://github.com/jordond/dfdu/pull/19))

### Changed

- Run the AppImage as the agent only from inside its own AppDir
- Let SUDO_UID decide whom root acts for before PKEXEC_UID
- Never give back to the user what is staged
- Stage by file descriptor, checking the item again
- Make the journal in its directory as opened, not through a symlink
- Put a staged item back only if it is still the one staged, and as root stage only into root's directories
- Check install.sh --gui installs the latest release's GUI, which ships from 0.3.0
- Stack the GUI face's sheets at their natural height on narrow screens

## [0.3.0] - 2026-11-03

### Added

- Scan as an administrator from the GUI on Linux through pkexec
- **sys:** macOS privileged helper over SMAppService and XPC
- Scan as an administrator on macOS through a helper, with a setup window
- **sys:** ask an administrator's authorization for each scan as root on macOS
- **gui:** ask an administrator's authorization before each scan through the macOS helper
- Show the start screen again when the administrator's password dialog is cancelled
- Offer to install the macOS helper when SMAppService says it is not found
- **gui:** put Full disk access on the start screen and redo its setup window
- Install the GUI from the release with install.sh --gui
- Package the GUI for Linux as a .deb and an AppImage
- Package the GUI for Windows as an NSIS installer and a portable zip

### Changed

- Make sudo dfdu act for the user who ran sudo
- Answer a refused or failed helper start to the window as an error
- Sign and notarise the macOS GUI and CLI
- Fail the elevated agent's check in the root btrfs step if it would skip
- Version the GUI with the workspace: it ships on the CLI's tag
- Upload the GUI's installers to the CLI's release and publish its cask
- Give the window the app id app.dfdu, so Linux desktops match it to app.dfdu.desktop
- Build dfdu-gui for the Windows GUI subsystem, attaching to a shell's console

### Fixed

- **sys:** check the helper's signature before sending the link, and run on older macOS
- **sys:** answer the helper's client after the agent starts, and tighten the XPC boundary

## [0.2.0] - 2026-11-01

### Added

- Add the dfdu-gui crate: a window on the session driver
- Add details, marks, the delete dialog, undo and the row menu to the window
- Show the largest children's names in full in the details
- Add the Map view to the window, painted from the tile tree
- Add the Largest view to the window
- Add the Cleanup view and its native command pane to the window
- Add the Duplicates view to the window, with view cycling
- Add the command palette and the help to the window
- Show each view's selection, tab totals, theme files and row menus
- Show dfdu's mark as the window's icon
- Install from dfdu.app/install.sh: one POSIX sh script for macOS and Linux

### Changed

- --remote sends the agent to hosts without dfdu ([#5](https://github.com/jordond/dfdu/pull/5))
- Update parking_lot 0.13, sysinfo 0.40, rusqlite and unicode-width patches ([#9](https://github.com/jordond/dfdu/pull/9))
- Destroy btrfs subvolumes by id and rmdir only the directory emptied
- Draw marks, the filter and text inputs from ratatui-free state
- Run the TUI's loop on the session driver
- Apply user theme files in the window and list them in the View menu
- Open an export read-only from the start screen
- Type the screenshot keys one at a time, with Enter
- Close on Cmd-Q and Cmd-W, and say when Full Disk Access is missing
- Give the title bar the theme chosen
- Name every row, cell and title bar for assistive technology
- Leave the start screen's empty disk cells unselectable
- Draw the window in the Terminal design: fonts, sheets, toolbar, tabs, Files and details
- Give size bars a cool grey track in the light window
- Draw the Map in the Terminal design: controls row, gutters, category fills, raised parents
- Draw the start screen, the delete dialog and the windows in the Terminal design
- Fit the disk cards' sizes, quiet the start screen's View menu and keep keys in order
- Fold the middle breadcrumbs and cut the last one short when the path is long
- Draw Cleanup, Duplicates and Largest in the Terminal design
- Draw a safety glyph only in a font that has it
- Rule the review views' headers across the table and keep the summary line clear of its controls
- Leave out safety glyphs the bundled fonts lack, and drop the kit's unused parts
- Order the map's colourings as p steps through them, and name the root disks by what they are
- Name the disk cards by their volume labels: the Finder's, Explorer's, or udev's by-label
- Mark the suggested copies once their files are found when asked for before, instead of marking none
- List the disks as rows of one sheet on the start screen, the system volume folded into its data volume

### Fixed

- Stop a comparison of duplicates when its view closes or on quit

## [0.1.0] - 2026-10-30

### Added

- Add deletion and the terminal browser
- Support btrfs subvolumes, snapshots and reflinks
- Add macOS and Windows scan backends
- **engine:** resume lists changed directories again and keeps the loaded subdirectories they still have
- **store:** snapshot format 4 with the last full scan, a stepwise writer, and the scan cache store
- The browser opens the cached scan of PATH at once and saves complete scans; dfdu cache list|clear, --no-cache

### Changed

- Classify mounts, report unreadable entries, list disks
- Harden the scanner's synchronisation
- Limit work per mount and share the tree lock fairly
- Extend btrfs coverage
- Add marks, filter, palette, details, motion and mouse
- Add help and footer hints, polish and speed up filtering
- Treemap view, cleanup rules and classification
- Staging, native commands, cleanup view, import, export and reports
- Content rules, file types, duplicates, clone accounting and incremental resumes
- Remote scans over SSH, Windows USN and MFT backends, btrfs resumes
- Review fixes: leaf crates, docs, clean, rules, applier, engine, remote, TUI, backends, CLI
- Measure shared data during the scan and show APFS volume labels
- List view column layout and TUI bug fixes
- Sorted top rows while scanning, header state flags, treemap labels, half-page moves, copy and open, range and pattern marks, largest files view
- Tree mode and the marks pane
- Rescans wait for their parent's listing, so siblings in its unread blocks count
- Remote trees keep their host's paths (protocol version 6); duplicates only for this machine's trees
- Windows host paths store `/` as `\`, so one path compares and hashes as one
- Tab bar and scope line: the views by name, [ and ] to step through them, Esc back to Files
- One meaning per key: A marks suggested, R compares again, N runs the native command; views keep only navigation keys
- Review views: Largest, Duplicates and Cleanup share a summary bar, checkboxes, relative ages, shortened paths and a detail pane
- Filled tiles with header rows and gutters, colour by category, depth 1 peeks into the selected tile
- Empty states for Map, Cleanup, Duplicates and Largest; Duplicates compares only when asked
- Review views delete only what they ticked: d from a key or the palette, never every copy of a duplicate, no panic on an empty view
- Map colours by what is matched inside, however deep and once decided; dark tints with readable labels on dark themes
- Duplicates counts once per tree without building paths; Cleanup's tab is the summary bar's total, kept current
- Footers keep a view's own keys before views, help and quit; long roots shorten before the size shown gives way; empty-state boxes hold a wrapped keys line
- Sparse extent table; wide directories keep sibling names only where a rule can match ([#3](https://github.com/jordond/dfdu/pull/3))
- History rows walk stacks only directories
- Walk the loaded tree in steps, check kept mtimes and classes, flush FSEvents at the position
- Rescan a kept directory unless its parent's listing gives it the hits it was loaded with
- The watchdog spawns no workers once stopping
- Say what a resume guarantees before a delete
- No journal position when FSEvents cannot flush in time
- FSEvents position: a few seconds back instead of a flush
- Journal queries of a resume end early when the engine stops
- A resume checks the directories it keeps by their stat
- The resume's check stats directories by path on the workers
- A resume's rescans, grafts and re-homing go a step at a time
- Split the wire's chunks, the scanner's job control and the end of a sim run
- Build aarch64-apple-darwin releases on macos-26

### Fixed

- Keep the browser responsive during scans
- **engine:** a resume counts the loaded hard links with those its rescans find
- **engine:** resume journal query on a joinable, cancellable thread
- **tui:** save and record only once every command sent is applied
- **store:** use an existing cache directory as it is, or not at all
- Fix the docs off macOS, imports on Windows and the loom match

### Performance

- Speed up scanning and cut memory per entry
- Speed up the applier at ten million entries

### Security

- History, crash consistency, fuzzing, packaging and a security review
