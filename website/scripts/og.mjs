// Renders public/og.png, the social card. The text uses a system monospace font (fontconfig).

import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import sharp from 'sharp'

const site = join(dirname(fileURLToPath(import.meta.url)), '..')
const logo = readFileSync(join(site, '..', 'assets', 'logo', 'dfdu-logo-dark.svg'), 'utf8')
const viewBox = logo.match(/viewBox="([^"]+)"/)[1]
const inner = logo.replace(/^[\s\S]*?<svg[^>]*>/, '').replace(/<\/svg>\s*$/, '').replace(/<title>.*?<\/title>/, '')

const W = 1200
const H = 630
const logoW = 640
const [, , vw, vh] = viewBox.split(/\s+/).map(Number)
const logoH = Math.round((logoW * vh) / vw)

const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}" viewBox="0 0 ${W} ${H}">
  <rect width="${W}" height="${H}" fill="#1f232b"/>
  <rect x="0" y="0" width="${W}" height="44" fill="#4c566a"/>
  <text x="40" y="29" font-family="Menlo, 'DejaVu Sans Mono', monospace" font-size="22" font-weight="700" fill="#eceff4">dfdu │ 1 About │ 2 Install │ 3 CLI │ 4 Compare</text>
  <svg x="${(W - logoW) / 2}" y="140" width="${logoW}" height="${logoH}" viewBox="${viewBox}">${inner}</svg>
  <text x="${W / 2}" y="${160 + logoH + 60}" text-anchor="middle" font-family="Menlo, 'DejaVu Sans Mono', monospace" font-size="38" font-weight="700" fill="#eceff4">a disk usage browser for the terminal</text>
  <text x="${W / 2}" y="${160 + logoH + 112}" text-anchor="middle" font-family="Menlo, 'DejaVu Sans Mono', monospace" font-size="24" fill="#959eb1">macOS · Linux · Windows · dfdu.app</text>
  <rect x="0" y="${H - 8}" width="${W}" height="8" fill="#88c0d0"/>
</svg>`

await sharp(Buffer.from(svg)).png({ compressionLevel: 9 }).toFile(join(site, 'public', 'og.png'))
console.log('wrote public/og.png')
