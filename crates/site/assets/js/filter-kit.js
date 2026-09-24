// Shared by the four index filters, inlined ahead of each page's own script.
// Every index keeps its filter in the query string, so a filtered view can
// be shared or bookmarked, and a reload, or a Back that misses the page
// cache, comes back to the same rows.

// Case, accents and apostrophes drop out, as in the quick search: "veterans'
// affairs" finds the title APH writes with a curly apostrophe, and 'oneil'
// finds O'Neil.
function fold(value) {
  return String(value ?? '')
    .toLowerCase()
    .normalize('NFD')
    .replace(/[\u0300-\u036f]/g, '')
    .replace(/['\u2018\u2019\u02bc]/g, '')
}

// The words in the box, folded, split wherever the quick search splits a
// name: at anything not a letter or a digit, so a dash typed where the
// record has an em dash, or a stray bracket, costs nothing. A row matches
// when it holds every word, in any order: 'fair work 2025' finds the Fair
// Work bill of 2025.
function termsOf(input) {
  return fold(input?.value)
    .split(/[^\p{L}\p{N}]+/u)
    .filter(Boolean)
}

function holdsAll(hay, terms) {
  return terms.every((term) => hay.includes(term))
}

// A button group holds one value: the button whose data value matches is
// pressed. A value no button carries, from a stale or mistyped link, falls
// back to the first button, the "all". Returns the value that took.
function press(group, key, value) {
  const buttons = [...(group?.querySelectorAll('button') ?? [])]
  const chosen = buttons.some((b) => (b.dataset[key] ?? '') === value) ? value : ''
  for (const button of buttons) {
    button.setAttribute('aria-pressed', String((button.dataset[key] ?? '') === chosen))
  }
  return chosen
}

// The state goes into the query string a moment after the last change:
// WebKit throws once a page rewrites its history too often. Parameters the
// filter does not own are left alone, and so is the hash, so a link to a
// month keeps its place. A write still waiting when the reader follows a
// row is made on the way out, so Back returns to the view they left.
let urlState = null
let urlTimer = 0

function writeUrl(state) {
  urlState = state
  clearTimeout(urlTimer)
  urlTimer = setTimeout(flushUrl, 200)
}

function flushUrl() {
  clearTimeout(urlTimer)
  if (!urlState) return
  const params = new URLSearchParams(location.search)
  for (const [key, value] of Object.entries(urlState)) {
    if (value) params.set(key, value)
    else params.delete(key)
  }
  urlState = null
  // Not params.size, which Safari only has from version 17.
  const qs = params.toString()
  const url = `${location.pathname}${qs ? `?${qs}` : ''}${location.hash}`
  if (url === `${location.pathname}${location.search}${location.hash}`) return
  try {
    history.replaceState(history.state, '', url)
  } catch {
    // Over the limit: the view is right, only the address lags.
  }
}

addEventListener('pagehide', flushUrl)

// The live count speaks once the reader pauses rather than on every
// keystroke; the rows themselves change at once.
let countTimer = 0

function settleCount(count, message) {
  clearTimeout(countTimer)
  countTimer = setTimeout(() => {
    if (count && count.textContent !== message) count.textContent = message
  }, 400)
}

// On a phone the keyboard's Done key puts the keyboard away, so the rows it
// was covering can be seen. With a mouse and keyboard, focus stays put.
function doneOnEnter(input) {
  input?.addEventListener('keydown', (event) => {
    if (event.key === 'Enter' && !event.isComposing && matchMedia('(pointer: coarse)').matches) {
      input.blur()
    }
  })
}

// A month row owns every row that follows it up to the next one, and its
// link in the month strip.
function monthsOf(items, strip) {
  const months = []
  for (const item of items) {
    if (item.dataset.month !== undefined) {
      months.push({
        row: item,
        rows: [],
        label: item.querySelector('.n'),
        link: strip?.querySelector(`a[data-month="${item.dataset.month}"]`),
      })
    } else if (months.length) {
      months[months.length - 1].rows.push(item)
    }
  }
  return months
}

// A month's divider and its link in the strip give the same count, and both
// drop out once the month is empty.
function recountMonth(month, visible, noun) {
  const counted = `${noun}${visible === 1 ? '' : 's'}`
  month.row.style.display = visible ? '' : 'none'
  if (month.label) month.label.textContent = `${visible} ${counted}`
  if (!month.link) return
  month.link.hidden = !visible
  const figure = month.link.querySelector('.n')
  if (figure) figure.textContent = String(visible)
  const spoken = month.link.querySelector('.visually-hidden')
  if (spoken) spoken.textContent = ` ${counted}`
}
