// www.dfdu.app -> dfdu.app, keeping the path and query. A Worker of its own so that the site's
// Worker stays assets-only: requests to dfdu.app never run code.
export default {
  fetch(request) {
    const url = new URL(request.url)
    url.hostname = 'dfdu.app'
    url.protocol = 'https:'
    url.port = ''
    return Response.redirect(url.href, 301)
  },
}
