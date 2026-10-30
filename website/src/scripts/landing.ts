// The landing page's behaviour, on the server-rendered screen in src/pages/index.astro: tabs
// (deep-linked as #install and so on), row selection, the Details pane (cloned from each row's
// <template>), copying commands, pausing the recordings, and the TUI's keys on the focused screen.

import { BREW, REPO } from '../site'

const THEMES = ['dark', 'black', 'light'] as const
const COPIED_MS = 1600

/** Copies `text`, through a hidden textarea where the Clipboard API is missing or refused. */
async function writeClipboard(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text)
    return true
  } catch {
    const area = document.createElement('textarea')
    area.value = text
    area.setAttribute('readonly', '')
    area.style.position = 'fixed'
    area.style.opacity = '0'
    document.body.append(area)
    area.select()
    let ok = false
    try {
      ok = document.execCommand('copy')
    } catch {}
    area.remove()
    return ok
  }
}

export function start() {
  const root = document.getElementById('tui')
  const detail = document.getElementById('detail')
  if (!root || !detail) return
  const aside = detail.parentElement!

  const panes = [...root.querySelectorAll<HTMLElement>('.pane')]
  const tabIds = panes.map((p) => p.dataset.tab!)
  const pane = (id: string) => panes.find((p) => p.dataset.tab === id)!
  const current = () => root.dataset.tab!
  const rows = (id: string) => [...pane(id).querySelectorAll<HTMLButtonElement>('button.row')]
  const selected = (id: string) => rows(id).find((r) => r.getAttribute('aria-current') === 'true') ?? rows(id)[0]!

  // Recordings start paused for people who ask for reduced motion; `p` or the button toggles.
  let paused = matchMedia('(prefers-reduced-motion: reduce)').matches

  /** Plays or stops the recording in the Details pane to match `paused`. */
  function applyMotion() {
    const media = detail!.querySelector<HTMLElement>('[data-media]')
    const button = detail!.querySelector<HTMLButtonElement>('[data-pause]')
    if (!media || !button) return
    button.hidden = false
    button.textContent = paused ? 'p · play' : 'p · pause'
    button.setAttribute('aria-pressed', String(paused))
    if (media instanceof HTMLVideoElement) {
      if (paused) media.pause()
      else media.play().catch(() => {})
      return
    }
    // A <picture>: the GIF, or its still frame while paused. The reduced-motion <source> only
    // matters before this script runs.
    media.querySelector('source')?.remove()
    media.querySelector('img')!.src = paused ? media.dataset.still! : media.dataset.gif!
  }

  function showDetail(id: string) {
    const template = document.getElementById(`detail-${id}-${selected(id).dataset.row}`) as HTMLTemplateElement | null
    if (!template) return
    detail!.replaceChildren(template.content.cloneNode(true))
    aside.classList.toggle('has-gif', detail!.querySelector('.gif') !== null)
    applyMotion()
  }

  function setTab(id: string) {
    if (!tabIds.includes(id)) id = tabIds[0]!
    root!.dataset.tab = id
    for (const p of panes) p.hidden = p.dataset.tab !== id
    for (const b of root!.querySelectorAll<HTMLElement>('[data-tab-button]')) {
      b.setAttribute('aria-pressed', String(b.dataset.tabButton === id))
    }
    document.getElementById('scan-path')!.textContent = `~/${id}`
    const hash = id === tabIds[0] ? '' : `#${id}`
    if (location.hash !== hash) history.replaceState(null, '', `${location.pathname}${location.search}${hash}`)
    showDetail(id)
  }

  function select(row: HTMLButtonElement) {
    const id = current()
    for (const r of rows(id)) r.setAttribute('aria-current', String(r === row))
    row.scrollIntoView({ block: 'nearest' })
    showDetail(id)
  }

  function move(by: number) {
    const list = rows(current())
    const next = list[list.indexOf(selected(current())) + by]
    if (next) select(next)
  }

  let copiedTimer: number | undefined
  async function copy(text: string, from: Element | null) {
    const ok = await writeClipboard(text)
    root!.focus({ preventScroll: true })
    const note = from?.parentElement?.querySelector<HTMLElement>('.copied')
    for (const n of root!.querySelectorAll<HTMLElement>('.copied')) n.hidden = n !== note
    if (!note) return
    note.dataset.ok ??= note.textContent ?? ''
    note.textContent = ok ? note.dataset.ok : '✗ could not copy; select the text instead'
    note.classList.toggle('t-err', !ok)
    clearTimeout(copiedTimer)
    copiedTimer = window.setTimeout(() => (note.hidden = true), COPIED_MS)
  }

  function cycleTheme() {
    const now = document.documentElement.dataset.theme as (typeof THEMES)[number]
    const next = THEMES[(THEMES.indexOf(now) + 1) % THEMES.length]!
    document.documentElement.dataset.theme = next
    try {
      localStorage.setItem('dfdu-theme', next)
    } catch {}
  }

  function togglePause() {
    paused = !paused
    applyMotion()
  }

  root.addEventListener('click', (e) => {
    const target = e.target as Element
    const tab = target.closest<HTMLElement>('[data-tab-button], [data-tab-link]')
    if (tab) {
      e.preventDefault()
      setTab(tab.dataset.tabButton ?? tab.dataset.tabLink!)
      return
    }
    const row = target.closest<HTMLButtonElement>('button.row')
    if (row) return select(row)
    if (target.closest('[data-pause]')) return togglePause()
    const copyButton = target.closest<HTMLElement>('[data-copy]')
    if (copyButton) void copy(copyButton.dataset.copy!, copyButton)
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
      const button = detail.querySelector<HTMLElement>('[data-copy]')
      void copy(button?.dataset.copy ?? BREW, button ?? root.querySelector('.foot [data-copy]'))
    } else if (k === 'p') togglePause()
    else if (k === 'g') window.open(REPO, '_blank', 'noopener')
    else if (k === 't') cycleTheme()
    else return
    e.preventDefault()
  })

  addEventListener('hashchange', () => setTab(location.hash.slice(1)))

  if (location.hash) setTab(location.hash.slice(1))
  else applyMotion()
  root.focus({ preventScroll: true })
}
