// Copies the demo recordings, the desktop app's screenshots and the icons from ../assets, and ../install.sh, into public/, so git
// holds one copy.

import { cpSync, mkdirSync, readdirSync, rmSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const site = join(dirname(fileURLToPath(import.meta.url)), '..')
const assets = join(site, '..', 'assets')
const pub = join(site, 'public')

const demos = join(pub, 'demos')
rmSync(demos, { recursive: true, force: true })
mkdirSync(demos, { recursive: true })
const recordings = readdirSync(join(assets, 'demos')).filter((name) => /\.(gif|png|mp4|webm)$/.test(name))
for (const name of recordings) cpSync(join(assets, 'demos', name), join(demos, name))

// The desktop app's screenshots, from `cargo xtask demo --gui`.
const gui = join(pub, 'gui')
rmSync(gui, { recursive: true, force: true })
mkdirSync(gui, { recursive: true })
const screenshots = readdirSync(join(assets, 'gui')).filter((name) => name.endsWith('.png'))
for (const name of screenshots) cpSync(join(assets, 'gui', name), join(gui, name))

const icons = {
  'favicon.ico': 'favicon.ico',
  'apple-touch-icon.png': 'apple-touch-icon.png',
  'dfdu-mark.svg': 'favicon.svg',
  'dfdu-mark-512.png': 'icon-512.png',
}
for (const [from, to] of Object.entries(icons)) cpSync(join(assets, 'logo', from), join(pub, to))

// Served as https://dfdu.app/install.sh.
cpSync(join(site, '..', 'install.sh'), join(pub, 'install.sh'))

console.log(`synced ${recordings.length} demo recordings, ${screenshots.length} desktop app screenshots, ${Object.keys(icons).length} icons and install.sh`)
