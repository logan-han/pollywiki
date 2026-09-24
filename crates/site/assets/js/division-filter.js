// Divisions index: chamber segment plus a text box, kept in the URL as ?q=
// and ?house=. A folded series row stands for every division in it, so each
// count here is of divisions: a row weighs its data-count, or one. Month
// dividers and the month strip recount as rows hide, and drop out once
// their month is empty.
const text = document.getElementById('division-filter-text')
const houseGroup = document.getElementById('division-house')
const items = [...document.querySelectorAll('#division-list > li')]
const list = document.getElementById('division-list')
const strip = document.getElementById('month-jump')
const count = document.getElementById('filter-count')
const empty = document.getElementById('filter-empty')
const clear = document.getElementById('filter-clear')

const months = monthsOf(items, strip)
const rows = items.filter((item) => item.dataset.house !== undefined)
const weight = (row) => Number(row.dataset.count ?? 1)
const total = rows.reduce((sum, row) => sum + weight(row), 0)

// What each row answers to, folded once: the division's name. A series
// shows its names in two parts, the matter over the stage of each step, so
// it answers to those; a step's written description is not its name, and a
// series answers to no more than its divisions would as plain rows. It is
// read from the row itself rather than a copy in the markup, which would add
// a fifth to the page's weight.
function words(row) {
  const parts = row.classList.contains('ledger-series')
    ? [row.querySelector('.matter'), ...row.querySelectorAll('.series-steps a.stage')]
    : [row.querySelector('.what')]
  return parts.map((part) => part?.textContent ?? '').join(' ')
}
const hay = new Map(rows.map((row) => [row, fold(words(row))]))

let house = ''

function apply() {
  const terms = termsOf(text)
  let shown = 0
  for (const row of rows) {
    const match = holdsAll(hay.get(row), terms) && (!house || row.dataset.house === house)
    row.style.display = match ? '' : 'none'
    if (match) shown += weight(row)
  }
  for (const month of months) {
    const visible = month.rows.reduce(
      (sum, row) => (row.style.display === 'none' ? sum : sum + weight(row)),
      0,
    )
    recountMonth(month, visible, 'division')
  }
  const filtered = Boolean(terms.length || house)
  settleCount(count, filtered ? `Showing ${shown} of ${total} divisions` : '')
  // An all-hidden ledger would leave its rules behind under the empty state.
  const nothing = filtered && shown === 0
  if (empty) empty.hidden = !nothing
  if (list) list.hidden = nothing
  if (strip) strip.hidden = nothing
  writeUrl({ q: text?.value.trim(), house })
}

houseGroup?.addEventListener('click', (event) => {
  const button = event.target.closest('button')
  if (!button) return
  house = press(houseGroup, 'house', button.dataset.house ?? '')
  apply()
})

text?.addEventListener('input', apply)
doneOnEnter(text)

clear?.addEventListener('click', () => {
  if (text) text.value = ''
  house = press(houseGroup, 'house', '')
  apply()
  text?.focus()
})

// A shared or reloaded link brings its filter with it. The rows are filtered
// on load even without one, so a query the browser restored into the box
// after Back is applied rather than left showing over every row.
const params = new URLSearchParams(location.search)
if (text && params.has('q')) text.value = params.get('q')
house = press(houseGroup, 'house', params.get('house') ?? '')
apply()

// Back or Forward between the page's own entries brings back that entry's
// filter.
onRestore((params) => {
  restoreText(text, params.get('q'))
  house = press(houseGroup, 'house', params.get('house') ?? '')
  apply()
})
