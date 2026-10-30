import type { APIRoute } from 'astro'
import { isPreview } from '../site'

// Generated, not shipped from public/: pull request previews (DFDU_SITE_ENV=preview) serve the
// same pages on another host and must stay out of search indexes.
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
