// Divisions index: chamber segment plus a text box, kept in the URL as ?q=
// and ?house=. Month dividers recount as rows hide, and drop out once their
// month is empty.
const text = document.getElementById('division-filter-text')
const houseGroup = document.getElementById('division-house')
const items = [...document.querySelectorAll('#division-list > li')]
const list = document.getElementById('division-list')
const count = document.getElementById('filter-count')
const empty = document.getElementById('filter-empty')
const clear = document.getElementById('filter-clear')

const months = monthsOf(items)
const rows = items.filter((item) => item.dataset.house !== undefined)
// Each row's title, folded once. It is read from the row itself rather than
// a copy in the markup, which would add a fifth to the page's weight.
const hay = new Map(rows.map((row) => [row, fold(row.querySelector('.what')?.textContent)]))

let house = ''

function apply() {
  const terms = termsOf(text)
  let shown = 0
  for (const row of rows) {
    const match = holdsAll(hay.get(row), terms) && (!house || row.dataset.house === house)
    row.style.display = match ? '' : 'none'
    if (match) shown += 1
  }
  for (const month of months) {
    const visible = month.rows.filter((row) => row.style.display !== 'none').length
    recountMonth(month, visible, 'division')
  }
  const filtered = Boolean(terms.length || house)
  settleCount(count, filtered ? `Showing ${shown} of ${rows.length} divisions` : '')
  // An all-hidden ledger would leave its rules behind under the empty state.
  const nothing = filtered && shown === 0
  if (empty) empty.hidden = !nothing
  if (list) list.hidden = nothing
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
