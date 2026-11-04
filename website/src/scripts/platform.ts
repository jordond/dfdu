// Which desktop app build to offer: the visitor's OS and, where the browser tells, its CPU.

import type { Arch, Os } from '../data/gui'

export interface Platform {
  os: Os
  /** Missing when the browser does not say. */
  arch?: Arch
}

interface UaData {
  platform?: string
  getHighEntropyValues?(hints: string[]): Promise<{ architecture?: string }>
}

/** The visitor's desktop platform, or undefined on a phone, a tablet or an OS dfdu has no build for. */
export async function detectPlatform(): Promise<Platform | undefined> {
  const uaData = (navigator as Navigator & { userAgentData?: UaData }).userAgentData
  const ua = navigator.userAgent
  // iPadOS Safari reports itself as a Mac; a touch screen gives it away.
  if (/android|iphone|ipad|ipod/i.test(ua) || (/mac/i.test(navigator.platform) && navigator.maxTouchPoints > 1)) return undefined
  // The most specific source first: client hints, then navigator.platform, then the user agent.
  const os = [uaData?.platform, navigator.platform, ua].reduce<Os | undefined>((found, s) => found ?? (s ? osOf(s) : undefined), undefined)
  return os && { os, arch: await archOf(os, uaData, `${navigator.platform} ${ua}`) }
}

export function osOf(s: string): Os | undefined {
  if (/\bwin/i.test(s)) return 'windows'
  if (/mac/i.test(s)) return 'mac'
  if (/linux|x11|cros/i.test(s)) return 'linux'
  return undefined
}

async function archOf(os: Os, uaData: UaData | undefined, ua: string): Promise<Arch | undefined> {
  try {
    const { architecture } = (await uaData?.getHighEntropyValues?.(['architecture'])) ?? {}
    if (architecture === 'arm') return 'arm64'
    if (architecture === 'x86') return 'x86_64'
  } catch {
    // Hints refused: fall back to the user agent.
  }
  if (/aarch64|arm64|armv8/i.test(ua)) return 'arm64'
  // Every Mac browser says Intel in its user agent; the GPU name tells Apple silicon apart.
  if (os === 'mac') return macArch()
  if (/x86_64|x86-64|amd64|win64|wow64|x64/i.test(ua)) return 'x86_64'
  return undefined
}

function macArch(): Arch | undefined {
  try {
    const gl = document.createElement('canvas').getContext('webgl')
    if (!gl) return undefined
    const info = gl.getExtension('WEBGL_debug_renderer_info')
    const gpu = String(gl.getParameter(info ? info.UNMASKED_RENDERER_WEBGL : gl.RENDERER))
    gl.getExtension('WEBGL_lose_context')?.loseContext()
    // Safari says "Apple GPU" on every Mac, so only a named M chip counts.
    if (/apple m\d/i.test(gpu)) return 'arm64'
    if (/intel|amd|radeon|nvidia/i.test(gpu)) return 'x86_64'
  } catch {
    // No WebGL.
  }
  return undefined
}
