import type { APIRoute } from 'astro'
import { isPreview } from '../site'

// Generated so pull request previews can disallow everything.
const PRODUCTION = `User-agent: *
Allow: /

Sitemap: https://dfdu.app/sitemap-index.xml
`

const PREVIEW = `User-agent: *
Disallow: /
`

export const GET: APIRoute = () =>
  new Response(isPreview() ? PREVIEW : PRODUCTION, {
    headers: { 'content-type': 'text/plain; charset=utf-8' },
  })
