/** The GUI face's tabs, rows and details: dfdu-gui, the desktop app. */

import { existsSync } from 'node:fs'
import { join } from 'node:path'
import { INSTALL_SH, REPO } from '../site'
import type { Line, Tone } from './landing'

export const CASK = 'brew install --cask jordond/tap/dfdu-gui'
export const LATEST = `${REPO}/releases/latest`

/** A screenshot in ../assets/gui, from `cargo xtask demo --gui`. */
export type ShotKey = 'tree' | 'map' | 'files' | 'cleanup' | 'delete' | 'largest' | 'dupes'

export interface Shot {
  src: string
  /** The light theme's screenshot, where there is one. */
  light?: string
  alt: string
}

/** Every screenshot is 2200x1440. */
export const SHOT_WIDTH = 2200
export const SHOT_HEIGHT = 1440

const SHOTS = join(process.cwd(), '..', 'assets', 'gui')

function shot(key: ShotKey, alt: string): Shot {
  const light = existsSync(join(SHOTS, `${key}-light.png`)) ? `/gui/${key}-light.png` : undefined
  return { src: `/gui/${key}.png`, light, alt }
}

export interface GuiDetail {
  title: string
  sub: string
  lines: Line[]
  cmd?: string
  /** A release file, linked to the latest release. */
  file?: string
  shot?: Shot
  /** Shown in place of a screenshot the app does not have yet. */
  placeholder: string
}

export interface GuiItem {
  label: string
  tone: Tone
  col: string
  detail: GuiDetail
}

export interface GuiSection {
  label: string
  note: string
  items: GuiItem[]
}

export interface GuiTab {
  id: 'overview' | 'download' | 'views'
  name: string
  head: [string, string]
  sections: GuiSection[]
}

const L = (t: string, tone: Tone = 'fg', bold = false): Line => ({ t, tone, bold })

const SAFETY = [
  L('✓ Safe to delete', 'safe', true),
  L('↻ Regenerable', 'safe', true),
  L('? Review first', 'review', true),
  L('⚒ Manual', 'keep', true),
  L('⊘ Protected', 'keep', true),
]

interface ItemSpec {
  label: string
  tone: Tone
  col: string
  sub: string
  lines: Line[]
  cmd?: string
  file?: string
  shot?: ShotKey
  ph: string
}

const S = (label: string, note: string, items: ItemSpec[]) => ({ label, note, items })

const OVERVIEW = [
  S('See it', 'while the scan is still running', [
    { label: 'Browse while it scans', tone: 'tool', col: 'List', sub: 'list view', lines: [L('Rows fill in as the scanner reads. Sort from the column headers; tree mode opens folders in place.')], shot: 'tree', ph: 'list view, mid-scan' },
    { label: 'A treemap you can click into', tone: 'vm', col: 'Treemap', sub: 'treemap view', lines: [L('Hover a tile for its size, click to select it, double-click to zoom in.'), L('Colour by category, with the name on every tile.', 'dim')], shot: 'map', ph: 'treemap view' },
    { label: 'Details for every entry', tone: 'media', col: 'Details', sub: 'right-hand panel', lines: [L('What it is, which rule matched, what you lose by deleting it, and its largest children.')], shot: 'files', ph: 'details panel' },
  ]),
  S('Clean', 'what the rules found', [
    { label: 'Cleanup view', tone: 'cache', col: 'Cleanup', sub: 'rules', lines: [L('Groups what the rules recognise by category and safety, with the recommended items marked on first open.'), ...SAFETY], shot: 'cleanup', ph: 'cleanup view' },
    { label: 'Trash first, then undo', tone: 'safe', col: 'Delete', sub: 'deleting', lines: [L('Deletes go to the Trash and can be undone. The dialog opens on Cancel.'), L('Permanent deletes, and anything over 50 GiB, ask you to type delete.', 'dim')], shot: 'delete', ph: 'delete dialog' },
    { label: 'Native cleanup commands', tone: 'build', col: 'dry run first', sub: "tools' own cleanup", lines: [L('Where a tool has its own cleanup command, dfdu runs its dry run first, and the real run only after that finished OK.')], shot: 'cleanup', ph: 'native command pane' },
  ]),
  S('Feel at home', 'desktop habits, terminal keys', [
    { label: 'Command palette', tone: 'key', col: '⌘K  Ctrl+K', sub: 'every action', lines: [L('Every action, searchable. ⌘F or Ctrl+F filters; ⌘C or Ctrl+C copies the path.')], ph: 'command palette' },
    { label: 'Same keys as the TUI', tone: 'dir', col: 'j  k  space  d', sub: 'keyboard', lines: [L('j and k, arrows, space to mark, d to delete, 1 to 5 for views. Help lists them all.')], shot: 'tree', ph: 'help overlay' },
    { label: 'Follows your system theme', tone: 'other', col: 'dark · light', sub: 'theme', lines: [L("The TUI's dark and light palettes. One user theme file colours both apps.")], shot: 'files', ph: 'light and dark side by side' },
  ]),
]

const DOWNLOAD = [
  S('One line', 'the CLI and the desktop app together', [
    { label: 'Install script', tone: 'media', col: 'macOS, Linux', sub: 'dfdu.app/install.sh --gui', lines: [L('Installs the CLI, then dfdu.app in ~/Applications on macOS, or the AppImage as dfdu-gui on x86_64 Linux.'), L('The desktop app ships from 0.3.0.', 'dim')], cmd: `${INSTALL_SH} -s -- --gui`, shot: 'map', ph: 'dfdu.app in the Dock' },
    { label: 'Homebrew cask', tone: 'cache', col: 'macOS', sub: 'jordond/tap/dfdu-gui', lines: [L('Signed and notarised. Updates with brew upgrade.')], cmd: CASK, shot: 'cleanup', ph: 'start screen' },
  ]),
  S('macOS', 'Apple silicon and Intel', [
    { label: 'Disk image', tone: 'tool', col: 'arm64 · x86_64', sub: '.dmg', lines: [L('Drag dfdu into Applications. Signed and notarised.'), L('dfdu-gui-x86_64-apple-darwin.dmg for Intel Macs.', 'dim')], file: 'dfdu-gui-aarch64-apple-darwin.dmg', shot: 'files', ph: 'the disk image window' },
  ]),
  S('Windows', 'x86_64', [
    { label: 'Installer', tone: 'vm', col: 'x86_64', sub: 'setup.exe', lines: [L('The setup program for Windows x86_64.')], file: 'dfdu-gui-x86_64-pc-windows-msvc-setup.exe', shot: 'largest', ph: 'dfdu on Windows' },
    { label: 'Portable zip', tone: 'other', col: 'x86_64', sub: '.zip', lines: [L('dfdu-gui.exe in a zip. Nothing to install.')], file: 'dfdu-gui-x86_64-pc-windows-msvc.zip', shot: 'dupes', ph: 'dfdu on Windows' },
  ]),
  S('Linux', 'x86_64, glibc', [
    { label: '.deb package', tone: 'build', col: 'Debian · Ubuntu', sub: '.deb', lines: [L('Installs /usr/bin/dfdu-gui.')], file: 'dfdu-gui-x86_64-unknown-linux-gnu.deb', shot: 'tree', ph: 'dfdu on GNOME' },
    { label: 'AppImage', tone: 'download', col: 'any x86_64', sub: '.AppImage', lines: [L('One file that runs without installing.'), L('Needs FUSE 2: libfuse2, or libfuse2t64 on Ubuntu 24.04.', 'dim')], file: 'dfdu-gui-x86_64-unknown-linux-gnu.AppImage', shot: 'map', ph: 'dfdu on KDE' },
  ]),
]

const VIEWS = [
  S('Views', 'same numbers as the TUI keys', [
    { label: 'List', tone: 'dir', col: '1', sub: 'virtualised table', lines: [L('Sortable columns, size bars, and tree mode with disclosure triangles.')], shot: 'tree', ph: 'list view' },
    { label: 'Treemap', tone: 'vm', col: '2', sub: 'painted tiles', lines: [L('Click to select, double-click to zoom, a colour mode menu, a tooltip on hover.')], shot: 'map', ph: 'treemap view' },
    { label: 'Cleanup', tone: 'cache', col: '3', sub: 'by category and safety', lines: [L('Recommended items are pre-marked the first time you open it.')], shot: 'cleanup', ph: 'cleanup view' },
    { label: 'Duplicates', tone: 'other', col: '4', sub: 'compared in the background', lines: [L('Compares with progress and a cancel button. The kept copy is never marked.')], shot: 'dupes', ph: 'duplicates view' },
    { label: 'Largest files', tone: 'download', col: '5', sub: 'the biggest files anywhere', lines: [L('The largest files in the whole scan, wherever they are.')], shot: 'largest', ph: 'largest files' },
  ]),
  S('Start', 'before the first scan', [
    { label: 'Start screen', tone: 'media', col: '—', sub: 'pick what to scan', lines: [L('Pick a disk, open a folder, drop one on the window, or reopen a recent scan.')], ph: 'start screen' },
  ]),
]

function item(s: ItemSpec): GuiItem {
  return {
    label: s.label,
    tone: s.tone,
    col: s.col,
    detail: {
      title: s.label,
      sub: s.sub,
      lines: s.lines,
      cmd: s.cmd,
      file: s.file,
      shot: s.shot && shot(s.shot, `dfdu-gui screenshot: ${s.label}`),
      placeholder: `[Screenshot: ${s.ph}]`,
    },
  }
}

const tab = (id: GuiTab['id'], name: string, head: GuiTab['head'], sections: ReturnType<typeof S>[]): GuiTab => ({
  id,
  name,
  head,
  sections: sections.map((s) => ({ label: s.label, note: s.note, items: s.items.map(item) })),
})

export const GUI_TABS: GuiTab[] = [
  tab('overview', 'Overview', ['Feature', 'Where'], OVERVIEW),
  tab('download', 'Download', ['Package', 'Platform'], DOWNLOAD),
  tab('views', 'Views', ['View', 'Key'], VIEWS),
]

/** The Overview hero's carousel, in order. */
export const HERO: { label: string; shot: Shot }[] = [
  { label: 'Treemap', shot: shot('map', 'dfdu-gui screenshot: the treemap view of a demo home directory') },
  { label: 'Cleanup', shot: shot('cleanup', 'dfdu-gui screenshot: the cleanup view of a demo home directory') },
  { label: 'Duplicates', shot: shot('dupes', 'dfdu-gui screenshot: the duplicates view of a demo home directory') },
  { label: 'Files and details', shot: shot('files', 'dfdu-gui screenshot: the file list and details panel of a demo home directory') },
]
