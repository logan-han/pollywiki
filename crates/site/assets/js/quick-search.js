// Header quick search: a combobox over the build-time index of bills, people
// and electorates. Progressive enhancement — with no JS the input does nothing
// and every page stays reachable through the nav and /search/.
const input = document.getElementById('quick-search-input')
const list = document.getElementById('quick-search-results')
const LIMIT = 8
// Bills first: they are what readers search for most.
const GROUPS = [
  { t: 'bill', label: 'Bills', prefix: '/bills/' },
  { t: 'person', label: 'People', prefix: '/people/' },
  { t: 'electorate', label: 'Electorates', prefix: '/electorates/' },
]

let index = null
let options = []
let activeIndex = -1
let generation = 0

function esc(value) {
  return String(value ?? '').replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`)
}

async function load() {
  if (!index) {
    const res = await fetch('/quick-search.json')
    index = res.ok ? await res.json() : []
  }
  return index
}

function close() {
  list.hidden = true
  list.innerHTML = ''
  options = []
  activeIndex = -1
  input.setAttribute('aria-expanded', 'false')
  input.removeAttribute('aria-activedescendant')
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
  return `<li role="presentation"><a role="option" id="${id}" aria-selected="false" tabindex="-1" href="${href}">${name}<span class="sub">${sub}</span></a></li>`
}

async function render() {
  const mine = ++generation
  const term = input.value.trim()
  if (term.length < 2) {
    close()
    return
  }
  const needle = term.toLowerCase()
  const loaded = await load()
  if (mine !== generation) return
  const entries = loaded.filter((e) => String(e.name ?? '').toLowerCase().includes(needle))

  let html = ''
  let n = 0
  for (const group of GROUPS) {
    if (n >= LIMIT) break
    const hits = entries.filter((e) => e.t === group.t).slice(0, LIMIT - n)
    if (!hits.length) continue
    html += `<li class="group" role="presentation">${group.label}</li>`
    for (const hit of hits) {
      const href = `${group.prefix}${encodeURIComponent(hit.slug)}/`
      html += option(`qs-opt-${n}`, href, esc(hit.name), esc(hit.sub))
      n += 1
    }
  }

  // Always offer the full-text index as the way out of a thin suggestion list.
  const everything = `/search/?q=${encodeURIComponent(term)}`
  html += `<li class="foot" role="presentation"><a role="option" id="qs-opt-${n}" aria-selected="false" tabindex="-1" href="${everything}">Search everything<span class="hint">↑↓ move · ↵ open · esc close</span></a></li>`

  list.innerHTML = html
  list.hidden = false
  options = [...list.querySelectorAll('[role="option"]')]
  activeIndex = -1
  input.setAttribute('aria-expanded', 'true')
  input.removeAttribute('aria-activedescendant')
}

input?.addEventListener('input', render)

input?.addEventListener('keydown', (event) => {
  if (event.key === 'Escape') {
    close()
    return
  }
  if (list.hidden || !options.length) return
  if (event.key === 'ArrowDown') {
    event.preventDefault()
    highlight(activeIndex + 1)
  } else if (event.key === 'ArrowUp') {
    event.preventDefault()
    highlight(activeIndex - 1)
  } else if (event.key === 'Enter') {
    // No selection yet means the first suggestion, matching what readers expect.
    event.preventDefault()
    const target = options[activeIndex] ?? options[0]
    if (target) location.href = target.href
  }
})

document.addEventListener('click', (event) => {
  if (list && !list.hidden && !event.target.closest('.quick-search')) close()
})
