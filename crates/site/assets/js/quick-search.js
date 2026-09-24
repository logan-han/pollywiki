// Header quick search: a combobox over the build-time index of bills, people
// and electorates. Progressive enhancement: the box is a real search form, so
// with no JS (or before the index arrives) Enter still goes to /search/?q=.
const input = document.getElementById('quick-search-input')
const form = input?.form
const list = document.getElementById('quick-search-results')
const status = document.getElementById('quick-search-status')
const LIMIT = 8
// Every group with a hit gets this many rows before any group gets more.
const FLOOR = 2
// Groups are shown closest match first; on a tie they keep this order.
const GROUPS = [
  { t: 'bill', label: 'Bills', prefix: '/bills/' },
  { t: 'person', label: 'People', prefix: '/people/' },
  { t: 'electorate', label: 'Electorates', prefix: '/electorates/' },
]

let pending = null
let options = []
let activeIndex = -1
let generation = 0

function esc(value) {
  return String(value ?? '').replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`)
}

// Case, accents and apostrophes drop out, so 'oneil' finds O’Neil and a name
// typed without its accents still finds the name that has them.
function fold(value) {
  return String(value ?? '')
    .toLowerCase()
    .normalize('NFD')
    .replace(/[\u0300-\u036f]/g, '')
    .replace(/['\u2018\u2019\u02bc]/g, '')
}

function words(value) {
  return fold(value)
    .split(/[^\p{L}\p{N}]+/u)
    .filter(Boolean)
}

// One request however fast the reader types. A fetch that fails on the
// network or with a server error can be retried; any other answer, such as
// the 404 of a build that shipped without the index, stands as an empty one.
// Each name is folded once here rather than on every keystroke.
function load() {
  pending ??= fetch('/quick-search.json')
    .then((res) => {
      if (res.status >= 500) throw new Error(String(res.status))
      return res.ok ? res.json() : []
    })
    .then((entries) => entries.map((e) => ({ e, flat: words(e.name).join(' ') })))
    .catch(() => {
      pending = null
      return []
    })
  return pending
}

// How the query meets the name, never anything about the item itself:
// 0 the name starts with it, 1 every term starts a word, 2 a term sits inside
// a word. -1 is no match.
function closeness(flat, terms) {
  if (!terms.every((t) => flat.includes(t))) return -1
  if (flat.startsWith(terms.join(' '))) return 0
  const parts = flat.split(' ')
  return terms.every((t) => parts.some((w) => w.startsWith(t))) ? 1 : 2
}

function announce(text) {
  // The same words again would only be read out again.
  if (status && status.textContent !== text) status.textContent = text
}

function close() {
  // A render still waiting on the index must not reopen the list.
  generation += 1
  list.hidden = true
  list.innerHTML = ''
  options = []
  activeIndex = -1
  input.setAttribute('aria-expanded', 'false')
  input.removeAttribute('aria-activedescendant')
  announce('')
}

function highlight(next) {
  if (!options.length) return
  activeIndex = (next + options.length) % options.length
  options.forEach((option, i) => {
    const on = i === activeIndex
    option.classList.toggle('active', on)
    option.setAttribute('aria-selected', String(on))
  })
  const active = options[activeIndex]
  input.setAttribute('aria-activedescendant', active.id)
  active.scrollIntoView({ block: 'nearest' })
}

function option(id, href, name, sub) {
  return `<a role="option" id="${id}" aria-selected="false" tabindex="-1" href="${href}">${name}<span class="sub">${sub}</span></a>`
}

async function render() {
  const mine = ++generation
  const term = input.value.trim()
  if (term.length < 2) {
    close()
    return
  }
  const loaded = await load()
  if (mine !== generation) return

  const terms = words(term)
  const hits = terms.length
    ? loaded
        .map((x) => ({ e: x.e, c: closeness(x.flat, terms) }))
        .filter((x) => x.c >= 0)
    : []
  // Mid-word hits ('Customs' for 'tom') only show when nothing starts a word.
  const best = hits.some((x) => x.c < 2) ? hits.filter((x) => x.c < 2) : hits
  const groups = GROUPS.map((g) => ({
    ...g,
    // Stable sort: within a tier the index order holds.
    hits: best.filter((x) => x.e.t === g.t).sort((a, b) => a.c - b.c),
  }))
    .filter((g) => g.hits.length)
    .sort((a, b) => a.hits[0].c - b.hits[0].c)
  // A long run of bill titles must not starve people and seats of any rows.
  const quota = groups.map((g) => Math.min(FLOOR, g.hits.length))
  let left = LIMIT - quota.reduce((a, b) => a + b, 0)
  groups.forEach((g, i) => {
    const extra = Math.min(left, g.hits.length - quota[i])
    quota[i] += extra
    left -= extra
  })

  let html = ''
  let n = 0
  groups.forEach((group, i) => {
    // A labelled group, so a screen reader hears "Bills" with the options under
    // it. The label is read as the group's name, so it is hidden as loose text.
    const label = `qs-group-${group.t}`
    html += `<li role="none"><div role="group" aria-labelledby="${label}"><span class="group" id="${label}" aria-hidden="true">${group.label}</span>`
    for (const { e } of group.hits.slice(0, quota[i])) {
      const href = `${group.prefix}${encodeURIComponent(e.slug)}/`
      html += option(`qs-opt-${n}`, href, esc(e.name), esc(e.sub))
      n += 1
    }
    html += '</div></li>'
  })
  // Only an index that actually arrived can say nothing matched; offline, the
  // full-text way out below is all there is to offer.
  if (!n && loaded.length) {
    html += `<li class="none" role="none" aria-hidden="true">No bill, person or electorate matches “${esc(term)}”.</li>`
  }

  // Always offer the full-text index as the way out of a thin suggestion list.
  const everything = `/search/?q=${encodeURIComponent(term)}`
  html += `<li class="foot" role="none"><a role="option" id="qs-opt-${n}" aria-selected="false" tabindex="-1" href="${everything}">Search everything<span class="hint" aria-hidden="true">↑↓ move · ↵ open · esc close</span></a></li>`

  list.innerHTML = html
  list.hidden = false
  options = [...list.querySelectorAll('[role="option"]')]
  activeIndex = -1
  input.setAttribute('aria-expanded', 'true')
  input.removeAttribute('aria-activedescendant')
  if (n) {
    announce(`${n} suggestion${n === 1 ? '' : 's'}. Up and down arrows to choose.`)
  } else if (loaded.length) {
    announce('No bill, person or electorate matches. Enter searches everything.')
  } else {
    announce('Suggestions unavailable. Enter searches everything.')
  }
}

input?.addEventListener('input', render)

// Warm the index as soon as the reader shows intent, and bring back the list
// for a query that is still in the box.
input?.addEventListener('focus', () => {
  load()
  if (input.value.trim().length >= 2) render()
})

input?.addEventListener('keydown', (event) => {
  // Enter that confirms an IME composition is not a choice.
  if (event.isComposing) return
  if (event.key === 'Escape') {
    // The first Escape closes the list and keeps the query; a second one
    // clears the field, as a search field does.
    if (!list.hidden) event.preventDefault()
    close()
    return
  }
  if (list.hidden || !options.length) {
    if (event.key === 'ArrowDown' && input.value.trim().length >= 2) {
      event.preventDefault()
      render()
    }
    return // Enter falls through to the form: /search/?q=…
  }
  if (event.key === 'ArrowDown') {
    event.preventDefault()
    highlight(activeIndex + 1)
  } else if (event.key === 'ArrowUp') {
    event.preventDefault()
    // Up from nothing chosen wraps round to the last option.
    highlight(activeIndex < 0 ? options.length - 1 : activeIndex - 1)
  } else if (event.key === 'Enter') {
    // No selection yet means the first suggestion, matching what readers expect.
    event.preventDefault()
    const target = options[activeIndex] ?? options[0]
    if (target) location.href = target.href
  }
})

// Pressing an option keeps focus in the input (Safari does not focus links on
// click), so the list stays under the pointer until the click lands.
list?.addEventListener('mousedown', (event) => event.preventDefault())

// Tabbing or clicking to another control closes the list, so it never covers
// the control that now has focus. A dismissed on-screen keyboard or a window
// switch has no relatedTarget and leaves it open; the click handler below
// still closes it on a tap elsewhere.
form?.addEventListener('focusout', (event) => {
  const to = event.relatedTarget
  if (to && !form.contains(to)) close()
})

// A click outside closes the list. It also cancels a list still waiting on a
// slow index, which would otherwise open after the reader had moved on.
document.addEventListener('click', (event) => {
  if (!list || event.target.closest('.quick-search')) return
  if (list.hidden) generation += 1
  else close()
})

// A page kept for Back comes back as it was left. Closing on the way out
// means a choice made from the list does not return with the list still open,
// or with a render still waiting to open it.
addEventListener('pagehide', () => {
  if (list) close()
})

// Phones scroll the nav row sideways; keep the current section, and whichever
// link has keyboard focus, clear of the fade on the row's trailing edge. This
// moves the row itself rather than calling scrollIntoView, so a #hash load
// never jumps back up to the header. On wider screens the row does not scroll
// and this does nothing.
function reveal(link) {
  const nav = link.parentElement
  // 48px clears the 40px fade.
  const over = link.getBoundingClientRect().right - nav.getBoundingClientRect().right + 48
  if (over > 0) nav.scrollLeft += over
}
const nav = document.querySelector('.site-nav')
const here = nav?.querySelector('[aria-current]')
if (here) {
  reveal(here)
  // The current link's heavier face can arrive after that first measure.
  document.fonts?.ready.then(() => reveal(here))
}
// A link only partly under the fade still counts as visible to the browser,
// so focus alone would leave its ring faded out.
nav?.addEventListener('focusin', (event) => reveal(event.target))
