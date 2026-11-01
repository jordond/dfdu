/** The landing page's tabs, rows and details. */

import { existsSync } from 'node:fs'
import { join } from 'node:path'
import { BREW, INSTALL_SH } from '../site'

/** A colour from the TUI palette (src/styles/landing.css). */
export type Tone =
  | 'fg' | 'dim' | 'header' | 'key' | 'dir' | 'ok' | 'warn' | 'err'
  | 'safe' | 'review' | 'keep' | 'cache' | 'build' | 'vm' | 'tool' | 'media' | 'download' | 'container' | 'other'

export interface Line {
  t: string
  tone: Tone
  bold: boolean
}

export interface Detail {
  title: string
  sub: string
  lines: Line[]
  cmd?: string
  gif?: string
  gifAlt?: string
  /** A still frame of the recording, shown while it is paused. */
  still?: string
  /** The recording as video, when `cargo xtask demo --format` made one; played before the GIF. */
  videos: { src: string; type: string }[]
}

export interface Cell {
  t: string
  tone: Tone
  bold: boolean
  align: 'left' | 'right' | 'center'
}

export type Row =
  | { kind: 'section'; label: string; note: string; count: string; first: boolean }
  | { kind: 'item'; label: string; tone: Tone; tree: string; cells: Cell[]; detail: Detail }

export interface Tab {
  id: 'about' | 'install' | 'cli'
  name: string
  band: string
  bandRight: string
  /** Shown after the active tab's name, as the TUI shows a count. */
  count?: string
  grid: string
  narrowGrid: string
  head: { t: string; align: Cell['align'] }[]
  sumTitle: string
  sumNote: string
  sum: string
  rows: Row[]
}

const gif = (name: string) => `/demos/${name}.gif`

const DEMOS = join(process.cwd(), '..', 'assets', 'demos')
const VIDEO_TYPES = [
  ['webm', 'video/webm'],
  ['mp4', 'video/mp4'],
] as const

/** Each recording has a PNG of its last frame next to the GIF (the tapes' `Screenshot`). */
function stillFor(gifPath?: string): string | undefined {
  const png = gifPath?.replace(/\.gif$/, '.png')
  return png && existsSync(join(DEMOS, png.slice('/demos/'.length))) ? png : undefined
}

function videosFor(gifPath?: string): Detail['videos'] {
  if (!gifPath) return []
  const name = gifPath.slice('/demos/'.length, -'.gif'.length)
  return VIDEO_TYPES.filter(([ext]) => existsSync(join(DEMOS, `${name}.${ext}`))).map(([ext, type]) => ({
    src: `/demos/${name}.${ext}`,
    type,
  }))
}
const L = (t: string, tone: Tone = 'fg', bold = false): Line => ({ t, tone, bold })

interface ItemSpec {
  label: string
  tone: Tone
  cols: string[]
  sub?: string
  lines?: Line[]
  cmd?: string
  gif?: string
}

type Spec = { section: string; note: string } | ItemSpec

const S = (section: string, note: string): Spec => ({ section, note })

const ABOUT: Spec[] = [
  S('Browse', 'fast, and faster the second time'),
  { label: 'Fast', tone: 'tool', cols: ['dfdu ~'], sub: 'parallel scanner', lines: [L('Parallel scanning tuned to the storage device, and you can start browsing before it finishes.')], cmd: 'dfdu ~', gif: gif('browse') },
  { label: "Opens last time's scan at once", tone: 'media', cols: ['dfdu PATH'], sub: 'cache', lines: [L('Scans that took 3 s or more are cached, so the next dfdu PATH opens at once and refreshes in the background.'), L('On macOS, Windows and btrfs only the changed directories are read again.', 'dim'), L('cached 3 h ago, refreshing', 'warn')], cmd: 'dfdu ~' },
  S('Understand', 'what your space is made of'),
  { label: 'Cleanup view', tone: 'cache', cols: ['3'], sub: 'view 3 · rules', lines: [L('Recognises caches, build folders, SDKs and old installers, says how safe each is to delete and how much it frees. Rules are extendable.'), L('✓ Safe to delete', 'safe', true), L('↻ Regenerable', 'safe', true), L('? Review first', 'review', true), L('⚒ Manual', 'keep', true), L('⊘ Protected', 'keep', true)], cmd: 'dfdu report ~/dev', gif: gif('cleanup') },
  { label: 'Treemap, tree, duplicates, largest', tone: 'vm', cols: ['2  T  4  5'], sub: 'views · one key away', lines: [L('Treemap, tree mode, duplicates and largest files, one key away from the list.')], gif: gif('treemap') },
  { label: 'History', tone: 'download', cols: ['dfdu diff'], sub: 'scan history', lines: [L('Every scan is recorded, so you can ask what grew since last week.'), L('+1.2 GiB since 2026-10-18', 'warn')], cmd: 'dfdu diff ~ --since 7d', gif: gif('diff') },
  S('Reach', 'other machines and scripts'),
  { label: 'Remote', tone: 'container', cols: ['--remote'], sub: 'over ssh', lines: [L('Browse and delete on another machine over ssh.'), L('dfdu must be installed on the other host.', 'dim')], cmd: 'dfdu --remote server /srv', gif: gif('remote') },
  { label: 'Scriptable', tone: 'other', cols: ['--format json'], sub: 'JSON · NDJSON · CI', lines: [L('JSON and NDJSON output, a CI mode that fails above a size limit, and ncdu export import.'), L('Exit 4 when the report is over the limit.', 'dim')], cmd: 'dfdu report ~/dev --format json --fail-above 20GiB', gif: gif('report') },
  { label: 'Safe by default', tone: 'safe', cols: ['d'], sub: 'deleting', lines: [L('Deletes go to the Trash.'), L('Permanent deletes ask you to type delete.'), L('--read-only removes the delete keys.', 'dim')], cmd: 'dfdu --read-only ~', gif: gif('delete') },
]

const INSTALL: Spec[] = [
  S('Package managers', 'updated with your other packages'),
  { label: 'Homebrew', tone: 'cache', cols: ['macOS, Linux'], sub: 'jordond/tap', lines: [L('Installs the binary, the man page and shell completions.')], cmd: BREW },
  { label: 'cargo binstall', tone: 'build', cols: ['any'], sub: 'prebuilt, through cargo', lines: [L('Downloads the release binary instead of compiling.')], cmd: 'cargo binstall --git https://github.com/jordond/dfdu dfdu' },
  { label: 'Nix', tone: 'tool', cols: ['macOS, Linux'], sub: 'flake', lines: [L('Runs dfdu from the flake without installing it.')], cmd: 'nix run github:jordond/dfdu' },
  S('Installers', 'one line, no package manager'),
  { label: 'Shell script', tone: 'media', cols: ['macOS, Linux'], sub: 'dfdu.app/install.sh', lines: [L('Installs into ~/.local/bin, with the man pages and shell completions. Run it again to update.'), L('--bin-dir DIR to install elsewhere, --version VER to pin a release.', 'dim')], cmd: INSTALL_SH },
  { label: 'PowerShell', tone: 'vm', cols: ['Windows'], sub: 'dfdu-installer.ps1', lines: [L('Installs into %USERPROFILE%\\.cargo\\bin.')], cmd: 'powershell -ExecutionPolicy Bypass -c "irm https://github.com/jordond/dfdu/releases/latest/download/dfdu-installer.ps1 | iex"' },
  { label: 'Release binaries', tone: 'download', cols: ['all'], sub: 'github releases', lines: [L('macOS (arm64, x86_64), Linux musl (arm64, x86_64), Windows x86_64.'), L('With man pages and shell completions.', 'dim'), L('github.com/jordond/dfdu/releases', 'key')] },
  S('From source', 'Rust 1.95+'),
  { label: 'cargo install', tone: 'other', cols: ['Rust 1.95+'], sub: 'build it yourself', lines: [L('Compiles from the repository with the locked dependencies.')], cmd: 'cargo install --locked --git https://github.com/jordond/dfdu dfdu' },
]

const cli = (cmd: string, tone: Tone, does: string, name?: string): ItemSpec =>
  ({ label: cmd, tone, cols: [does], cmd, gif: name && gif(name) })

const CLI: Spec[] = [
  S('Look', "print, don't browse"),
  cli('dfdu scan ~', 'dir', 'print a summary', 'scan-cli'),
  cli('dfdu scan ~ --by-type', 'dir', 'usage by file type', 'scan-cli'),
  cli('dfdu disks', 'dir', 'disks and free space, like df -h', 'disks'),
  cli('dfdu dupes ~ --min-size 10MiB', 'dir', 'duplicate files', 'dupes'),
  S('Clean', 'what the rules found'),
  cli('dfdu report ~/dev', 'cache', 'what the cleanup rules found', 'report'),
  cli('dfdu report ~/dev --format json --fail-above 20GiB', 'cache', 'CI: exit 4 over the limit', 'report'),
  cli('dfdu clean ~/dev', 'cache', 'dry run of the recommended deletes'),
  cli('dfdu clean ~/dev --yes', 'err', 'move them to the Trash'),
  S('Over time', 'snapshots and diffs'),
  cli('dfdu diff ~ --since 7d', 'tool', 'what grew in the last week', 'diff'),
  cli('dfdu export ~ --snapshot -o home.snap', 'tool', 'save a scan', 'snapshot'),
  cli('dfdu -f home.snap', 'tool', 'browse a saved scan or an ncdu export', 'snapshot'),
]

interface TabSpec extends Omit<Tab, 'rows' | 'narrowGrid'> {
  narrowGrid?: string
  specs: Spec[]
}

function detailFor(tab: TabSpec['id'], s: ItemSpec): Detail {
  if (tab === 'cli') {
    return { title: String(s.cols[0]), sub: s.label.split(' ').slice(0, 2).join(' '), lines: [], cmd: s.cmd, gif: s.gif, gifAlt: `Recording of ${s.label}`, still: stillFor(s.gif), videos: videosFor(s.gif) }
  }
  return { title: s.label, sub: s.sub ?? '', lines: s.lines ?? [], cmd: s.cmd, gif: s.gif, gifAlt: s.gif && `Recording: ${s.label}`, still: stillFor(s.gif), videos: videosFor(s.gif) }
}

function build(t: TabSpec): Tab {
  const { specs, ...tab } = t
  const sectioned = specs.some((s) => 'section' in s)
  const rows: Row[] = specs.map((s, i) => {
    if ('section' in s) {
      let n = 0
      for (let j = i + 1; j < specs.length && !('section' in specs[j]!); j++) n++
      return { kind: 'section', label: s.section, note: s.note, count: `${n} ${n === 1 ? 'item' : 'items'}`, first: i === 0 }
    }
    const next = specs[i + 1]
    const last = !next || 'section' in next
    const cells: Cell[] = s.cols.map((v, ci) => ({
      t: v,
      tone: tab.id === 'cli' ? 'fg' : 'dim',
      bold: false,
      align: tab.head[ci + 1]?.align ?? 'left',
    }))
    return { kind: 'item', label: s.label, tone: s.tone, tree: sectioned ? (last ? '  └─ ' : '  ├─ ') : '  ', cells, detail: detailFor(tab.id, s) }
  })
  return { ...tab, narrowGrid: t.narrowGrid ?? 'minmax(0,1fr)', rows }
}

const SPECS: TabSpec[] = [
  {
    id: 'about', name: 'About', band: 'Features · what dfdu does', bandRight: '8 entries', count: '8',
    grid: 'minmax(0,1fr) 22ch', head: [{ t: '  feature', align: 'left' }, { t: 'try', align: 'right' }],
    sumTitle: 'dfdu', sumNote: 'disk usage browser',
    sum: 'dfdu scans a directory once and shows it as a list, a treemap, a cleanup view, duplicates or the largest files. It is written in Rust and runs on macOS, Linux and Windows.',
    specs: ABOUT,
  },
  {
    id: 'install', name: 'Install', band: 'Install · pick one', bandRight: '7 ways', count: '7',
    grid: 'minmax(0,1fr) 18ch', head: [{ t: '  method', align: 'left' }, { t: 'platform', align: 'right' }],
    sumTitle: 'Release binaries', sumNote: 'every platform',
    sum: 'Binaries for macOS, Linux (static) and Windows are on the releases page, with man pages and shell completions.',
    specs: INSTALL,
  },
  {
    id: 'cli', name: 'CLI', band: 'Commands for scripts and CI', bandRight: 'JSON is versioned',
    grid: 'minmax(0,1.3fr) minmax(0,1fr)', head: [{ t: '  command', align: 'left' }, { t: 'does', align: 'left' }],
    sumTitle: 'clean', sumNote: 'dry run first',
    sum: 'clean changes nothing without --yes. JSON output is versioned; the schemas are in schema/. dfdu --help lists every option and exit code.',
    specs: CLI,
  },
]

export const TABS: Tab[] = SPECS.map(build)

export const firstItem = (tab: Tab) => tab.rows.findIndex((r) => r.kind === 'item')
