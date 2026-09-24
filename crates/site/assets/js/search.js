// /search/: Pagefind's own interface, named by the page's heading, its
// result count announced as it changes, and the query kept in the address,
// so Back from a result, a copied link and the header's quick find all land
// on the same search.
const mount = document.getElementById('search')

if (typeof PagefindUI === 'undefined') {
  mount.textContent = 'Search index not built. Run the full build to generate it.'
} else {
  // Replaced, not pushed: a query typed a letter at a time is one visit, and
  // Back should leave the page rather than untype it.
  const keep = (term) => {
    const target = term ? `?q=${encodeURIComponent(term)}` : ''
    if (location.search !== target) {
      history.replaceState(history.state, '', target || location.pathname)
    }
  }
  const ui = new PagefindUI({
    element: '#search',
    showSubResults: false,
    showImages: false,
    // Runs once per search, after Pagefind's own debounce.
    processTerm: (term) => {
      keep(term.trim())
      return term
    },
  })

  const input = mount.querySelector('.pagefind-ui__search-input')
  if (input) {
    // Named by the visible heading, not by Pagefind's title attribute.
    input.setAttribute('aria-labelledby', 'search-heading')
    // An emptied box runs no search, so processTerm never hears of it.
    input.addEventListener('input', () => {
      if (!input.value.trim()) keep('')
    })
  }
  // Nor does Pagefind's Clear button, which empties the box without an input
  // event; without this, a reload would bring the cleared query back.
  mount.addEventListener('click', (event) => {
    if (event.target.closest('.pagefind-ui__search-clear')) keep('')
  })

  // Pagefind's count is a plain paragraph. Relayed into a status region, a
  // screen reader hears each settled count; the interim "Searching for"
  // line is left out so it does not talk over the answer.
  const status = document.getElementById('search-status')
  const relay = () => {
    const message = mount.querySelector('.pagefind-ui__message')
    const text = message?.textContent.trim() ?? ''
    if (text.endsWith('...') || text.endsWith('\u2026')) return
    if (status.textContent !== text) status.textContent = text
  }
  new MutationObserver(relay).observe(mount, {
    childList: true,
    subtree: true,
    characterData: true,
  })

  const q = new URLSearchParams(location.search).get('q')
  if (q) ui.triggerSearch(q)
  input?.focus()
}
