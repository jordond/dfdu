// The GUI face: tabs, selection, the filter, copying, the screenshot carousel and keys.

import { REPO } from '../site'
import { toggleFace } from './face'
import { COPIED_MS, cycleTheme, writeClipboard } from './landing'
import { detectPlatform, type Platform } from './platform'

const HERO_MS = 5000

export function startGui() {
  const root = document.getElementById('gui')
  const detail = document.getElementById('gdetail')
  const filter = document.getElementById('gfilter') as HTMLInputElement | null
  if (!root || !detail || !filter) return

  const panes = [...root.querySelectorAll<HTMLElement>('[data-gpane]')]
  const tabIds = panes.map((p) => p.dataset.gpane!)
  const pane = (id: string) => panes.find((p) => p.dataset.gpane === id)!
  const current = () => root.dataset.gtab!
  const visible = (b: HTMLElement) => !b.closest('li')!.hidden
  const rows = (id: string) => [...pane(id).querySelectorAll<HTMLButtonElement>('button.grow')].filter(visible)
  const selected = (id: string) =>
    [...pane(id).querySelectorAll<HTMLButtonElement>('button.grow')].find((r) => r.getAttribute('aria-current') === 'true')

  function showDetail(id: string) {
    const row = selected(id)
    const template = row && (document.getElementById(`gdetail-${id}-${row.dataset.grow}`) as HTMLTemplateElement | null)
    if (template) detail!.replaceChildren(template.content.cloneNode(true))
  }

  function select(row: HTMLButtonElement) {
    const id = current()
    for (const r of pane(id).querySelectorAll('button.grow')) r.setAttribute('aria-current', String(r === row))
    row.scrollIntoView({ block: 'nearest' })
    showDetail(id)
  }

  function move(by: number) {
    const list = rows(current())
    const at = list.indexOf(selected(current())!)
    const next = at < 0 ? list[0] : list[at + by]
    if (next) select(next)
  }

  /** Hides the rows (and then whole sections) whose label does not contain the filter text. */
  function applyFilter() {
    const q = filter!.value.trim().toLowerCase()
    const p = pane(current())
    for (const group of p.querySelectorAll<HTMLElement>('.ggroup')) {
      let any = false
      for (const li of group.querySelectorAll('li')) {
        li.hidden = q !== '' && !li.querySelector('.gname')!.textContent!.toLowerCase().includes(q)
        any ||= !li.hidden
      }
      group.hidden = !any
    }
    p.querySelector<HTMLElement>('.gempty')!.hidden = rows(current()).length > 0
  }

  function setTab(id: string) {
    if (!tabIds.includes(id)) id = tabIds[0]!
    root!.dataset.gtab = id
    for (const p of panes) p.hidden = p.dataset.gpane !== id
    for (const el of root!.querySelectorAll<HTMLElement>('[data-gtab-only]')) el.hidden = el.dataset.gtabOnly !== id
    for (const b of root!.querySelectorAll<HTMLElement>('[data-gtab-button]')) {
      b.setAttribute('aria-pressed', String(b.dataset.gtabButton === id))
    }
    document.getElementById('gcrumb')!.textContent = pane(id).getAttribute('aria-label')
    applyFilter()
    showDetail(id)
  }

  let copiedTimer: number | undefined
  async function copy(text: string, from: HTMLElement) {
    const ok = await writeClipboard(text)
    const note = from.querySelector<HTMLElement>('[data-note]') ?? root!.querySelector<HTMLElement>('.ghero .copied')!
    note.dataset.label ??= note.textContent ?? ''
    note.textContent = ok ? 'Copied' : 'Could not copy; select the text instead'
    note.classList.toggle('t-err', !ok)
    note.hidden = false
    clearTimeout(copiedTimer)
    copiedTimer = window.setTimeout(() => {
      note.textContent = note.dataset.label!
      note.classList.remove('t-err')
      if (note.classList.contains('copied')) note.hidden = true
    }, COPIED_MS)
  }

  // The hero's screenshots: a crossfade every 5 s, with the active pill's progress bar; none of it
  // moves on its own for prefers-reduced-motion.
  const slides = [...root.querySelectorAll<HTMLElement>('[data-slide]')]
  const pills = [...root.querySelectorAll<HTMLButtonElement>('[data-pill]')]
  const still = matchMedia('(prefers-reduced-motion: reduce)')
  let hero = 0
  let heroTimer: number | undefined

  function showHero(i: number) {
    hero = (i + slides.length) % slides.length
    slides.forEach((s, si) => {
      s.classList.toggle('on', si === hero)
      if (si === hero) s.removeAttribute('aria-hidden')
      else s.setAttribute('aria-hidden', 'true')
    })
    for (const p of pills) {
      const on = Number(p.dataset.pill) === hero
      p.setAttribute('aria-pressed', String(on))
      p.querySelector('.prog')?.remove()
      if (on && !still.matches) {
        const prog = document.createElement('span')
        prog.className = 'prog'
        prog.setAttribute('aria-hidden', 'true')
        p.append(prog)
      }
    }
    clearTimeout(heroTimer)
    if (!still.matches) heroTimer = window.setTimeout(() => showHero(hero + 1), HERO_MS)
  }

  /** Opens the Download tab on the row for a build. */
  function openBuild(id: string) {
    setTab('download')
    const row = pane('download').querySelector<HTMLButtonElement>(`button.grow[data-build="${id}"]`)
    if (row) select(row)
  }

  const builds = [...root.querySelectorAll<HTMLAnchorElement>('.ghero a[data-build]')]

  /** Points the hero's download at the build for this platform, and lists the others for its OS. */
  function offer(p: Platform) {
    const mine = builds.filter((b) => b.dataset.os === p.os)
    const pick = mine.find((b) => b.dataset.arch === p.arch) ?? mine[0]
    if (!pick) return
    const hero = root!.querySelector<HTMLElement>('.ghero')!
    const button = hero.querySelector<HTMLAnchorElement>('[data-dl]')!
    button.href = pick.href
    hero.querySelector('[data-dl-os]')!.textContent = hero.querySelector(`[data-os-link="${p.os}"]`)!.textContent!.trim()
    hero.querySelector('[data-dl-label]')!.textContent = pick.textContent!.trim()
    hero.querySelector('[data-dl-file]')!.textContent = pick.href.split('/').pop()!
    for (const b of builds) b.hidden = b.dataset.os !== p.os || b === pick
    // The other builds for this CPU come first.
    const lead = hero.querySelector('.plat > span')!
    for (const b of mine.filter((b) => b.dataset.arch !== pick.dataset.arch).reverse()) lead.after(b)
    for (const b of mine.filter((b) => b.dataset.arch === pick.dataset.arch).reverse()) lead.after(b)
    for (const l of hero.querySelectorAll<HTMLElement>('[data-os-link]')) l.hidden = l.dataset.osLink === p.os
    for (const c of hero.querySelectorAll<HTMLElement>('.gact [data-os]')) c.hidden = c.dataset.os !== p.os
    // The Download tab opens on this build's row.
    const rows = [...pane('download').querySelectorAll<HTMLButtonElement>('button.grow')]
    for (const r of rows) r.setAttribute('aria-current', String(r.dataset.build === pick.dataset.build))
    if (current() === 'download') showDetail('download')
  }

  root.addEventListener('click', (e) => {
    const target = e.target as Element
    const osLink = target.closest<HTMLElement>('[data-os-link]')
    if (osLink) return openBuild(builds.find((b) => b.dataset.os === osLink.dataset.osLink)!.dataset.build!)
    const tab = target.closest<HTMLElement>('[data-gtab-button], [data-gtab-link]')
    if (tab) return setTab(tab.dataset.gtabButton ?? tab.dataset.gtabLink!)
    const row = target.closest<HTMLButtonElement>('button.grow')
    if (row) return select(row)
    const pill = target.closest<HTMLElement>('[data-pill]')
    if (pill) return showHero(Number(pill.dataset.pill))
    const mode = target.closest<HTMLElement>('[data-size-mode]')
    if (mode) {
      for (const b of root.querySelectorAll('[data-size-mode]')) b.setAttribute('aria-pressed', String(b === mode))
      return
    }
    const copyButton = target.closest<HTMLElement>('[data-copy]')
    if (copyButton) void copy(copyButton.dataset.copy!, copyButton)
  })

  filter.addEventListener('input', applyFilter)
  filter.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' || e.key === 'Enter') {
      if (e.key === 'Escape') filter.value = ''
      applyFilter()
      root.focus({ preventScroll: true })
      e.preventDefault()
    }
    // Keys typed into the filter are text, not page keys.
    e.stopPropagation()
  })

  root.addEventListener('keydown', (e) => {
    if (e.metaKey || e.ctrlKey || e.altKey) return
    const at = tabIds.indexOf(current())
    const k = e.key
    if (k >= '1' && k <= String(tabIds.length)) setTab(tabIds[Number(k) - 1]!)
    else if (k === 'ArrowDown' || k === 'j') move(1)
    else if (k === 'ArrowUp' || k === 'k') move(-1)
    else if (k === 'ArrowRight' || k === 'l') setTab(tabIds[(at + 1) % tabIds.length]!)
    else if (k === 'ArrowLeft' || k === 'h') setTab(tabIds[(at + tabIds.length - 1) % tabIds.length]!)
    else if (k === 'c') {
      const button = detail.querySelector<HTMLElement>('[data-copy]') ?? root.querySelector<HTMLElement>('.ghero [data-copy]')!
      void copy(button.dataset.copy!, button)
    } else if (k === '/') filter.focus()
    else if (k === 'g') window.open(REPO, '_blank', 'noopener')
    else if (k === 't') cycleTheme()
    else if (k === 'm') toggleFace()
    else return
    e.preventDefault()
  })

  // The TUI's Install tab is the GUI's Download tab.
  if (location.hash === '#install') setTab('download')
  showHero(0)
  void detectPlatform().then((p) => p && offer(p))
}
