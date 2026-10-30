// Copies the demo recordings and icons from ../assets into public/, so git holds one copy.

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

const icons = {
  'favicon.ico': 'favicon.ico',
  'apple-touch-icon.png': 'apple-touch-icon.png',
  'dfdu-mark.svg': 'favicon.svg',
  'dfdu-mark-512.png': 'icon-512.png',
}
for (const [from, to] of Object.entries(icons)) cpSync(join(assets, 'logo', from), join(pub, to))

console.log(`synced ${recordings.length} demo recordings and ${Object.keys(icons).length} icons from assets/`)
