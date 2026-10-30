import starlight from '@astrojs/starlight'
import { defineConfig } from 'astro/config'
import starlightLlmsTxt from 'starlight-llms-txt'
import { llms } from './llms.mjs'

// `DFDU_SITE_ENV=preview` builds the pull request previews: same pages, but robots.txt disallows
// everything and every page is noindex (src/site.ts).
const preview = process.env.DFDU_SITE_ENV === 'preview'

const repo = 'https://github.com/jordond/dfdu'

export default defineConfig({
  site: 'https://dfdu.app',
  output: 'static',
  trailingSlash: 'ignore',
  integrations: [
    starlight({
      title: 'dfdu',
      description: 'Docs for dfdu, a disk usage browser for the terminal.',
      logo: {
        light: '../assets/logo/dfdu-logo.svg',
        dark: '../assets/logo/dfdu-logo-dark.svg',
        replacesTitle: true,
      },
      favicon: '/favicon.svg',
      social: [{ icon: 'github', label: 'GitHub', href: repo }],
      editLink: { baseUrl: `${repo}/edit/main/website/` },
      lastUpdated: true,
      plugins: [starlightLlmsTxt(llms)],
      // The site's own 404 (src/pages/404.astro) is a dfdu screen, like the landing page.
      disable404Route: true,
      customCss: ['@fontsource/jetbrains-mono/400.css', '@fontsource/jetbrains-mono/700.css', './src/styles/starlight.css'],
      head: [
        { tag: 'link', attrs: { rel: 'icon', href: '/favicon.ico', sizes: '32x32' } },
        { tag: 'link', attrs: { rel: 'apple-touch-icon', href: '/apple-touch-icon.png' } },
        { tag: 'meta', attrs: { property: 'og:image', content: 'https://dfdu.app/og.png' } },
        { tag: 'meta', attrs: { property: 'og:image:width', content: '1200' } },
        { tag: 'meta', attrs: { property: 'og:image:height', content: '630' } },
        { tag: 'meta', attrs: { name: 'twitter:image', content: 'https://dfdu.app/og.png' } },
        ...(preview ? [{ tag: 'meta', attrs: { name: 'robots', content: 'noindex, nofollow' } }] : []),
      ],
      sidebar: [
        {
          label: 'Start',
          items: [
            { label: 'Overview', slug: 'docs' },
            { label: 'Install', slug: 'docs/install' },
            { label: 'Browsing', slug: 'docs/usage' },
            { label: 'Command line', slug: 'docs/cli' },
          ],
        },
        {
          label: 'Guides',
          items: [
            { label: 'Cleanup rules', slug: 'docs/rules' },
            { label: 'Themes', slug: 'docs/themes' },
            { label: 'Benchmarks', slug: 'docs/benchmarks' },
          ],
        },
        {
          label: 'Project',
          items: [
            { label: 'Releases', link: `${repo}/releases`, attrs: { target: '_blank' } },
            { label: 'Contributing', link: `${repo}/blob/main/CONTRIBUTING.md`, attrs: { target: '_blank' } },
          ],
        },
      ],
    }),
  ],
  vite: {
    // Starlight's logo comes from the repository's assets/, one level above the site.
    server: { fs: { allow: ['..'] } },
  },
})
