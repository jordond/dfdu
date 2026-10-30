/** Site-wide constants for the pages outside Starlight. */

export const SITE_NAME = 'dfdu'
export const REPO = 'https://github.com/jordond/dfdu'
export const BREW = 'brew install jordond/tap/dfdu'

/**
 * Pull request previews (`DFDU_SITE_ENV=preview`) are kept out of search indexes. A function so
 * that the client script can import this module: it is only called at build time.
 */
export const isPreview = () => process.env.DFDU_SITE_ENV === 'preview'

export const DEFAULT_TITLE = 'dfdu: a disk usage browser for the terminal'
export const DEFAULT_DESCRIPTION =
  'dfdu is a fast disk usage browser for the terminal. Browse, sort and delete while the scan runs, see what your space is made of, and get a list of what is safe to remove. macOS, Linux and Windows.'
