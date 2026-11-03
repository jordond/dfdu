// The TUI/GUI face switch. The face is data-face on <html> (set before first paint by the inline
// script in index.astro), kept in localStorage as dfdu-face, and linkable as ?face=gui or ?face=tui.

export type Face = 'tui' | 'gui'

const STORAGE_KEY = 'dfdu-face'
/** Longest entrance animation (the GUI's Details sheet: 0.2 s delay, 0.55 s rise), plus slack. */
const ENTER_MS = 900

let enterTimer: number | undefined

export const currentFace = (): Face => (document.documentElement.dataset.face === 'gui' ? 'gui' : 'tui')

function syncButtons(face: Face) {
  for (const b of document.querySelectorAll<HTMLElement>('.switch [data-face-set]')) {
    b.setAttribute('aria-pressed', String(b.dataset.faceSet === face))
  }
}

export function setFace(face: Face) {
  const html = document.documentElement
  if (face === currentFace()) return
  html.dataset.face = face
  syncButtons(face)
  try {
    localStorage.setItem(STORAGE_KEY, face)
  } catch {}

  // Keep the address linkable: ?face=gui shows the GUI face; the TUI face is the default.
  const url = new URL(location.href)
  if (face === 'gui') url.searchParams.set('face', 'gui')
  else url.searchParams.delete('face')
  history.replaceState(null, '', url)

  // The entrance animation (CSS, and off for prefers-reduced-motion): a CRT power-on for the TUI,
  // a fade with rising sheets for the GUI.
  const root = document.getElementById(face)
  if (!root) return
  for (const el of document.querySelectorAll('.enter')) el.classList.remove('enter')
  void root.offsetWidth
  root.classList.add('enter')
  clearTimeout(enterTimer)
  enterTimer = window.setTimeout(() => root.classList.remove('enter'), ENTER_MS)
  root.focus({ preventScroll: true })
}

export const toggleFace = () => setFace(currentFace() === 'gui' ? 'tui' : 'gui')

export function startFace() {
  syncButtons(currentFace())
  document.addEventListener('click', (e) => {
    const button = (e.target as Element).closest<HTMLElement>('[data-face-set]')
    if (!button) return
    const face = button.dataset.faceSet
    if (face === 'tui' || face === 'gui') setFace(face)
  })
  document.getElementById(currentFace())?.focus({ preventScroll: true })
}
