// Electorates index: one text box over the table rows, kept in the URL as ?q=.
const text = document.getElementById('electorate-filter')
const rows = [...document.querySelectorAll('#electorate-table tbody tr')]
const table = document.getElementById('electorate-table')
const count = document.getElementById('filter-count')
const empty = document.getElementById('filter-empty')
const clear = document.getElementById('filter-clear')

// Each row's seat, state and member, folded once rather than on every keystroke.
const hay = new Map(rows.map((row) => [row, fold(row.dataset.text)]))

function apply() {
  const terms = termsOf(text)
  let shown = 0
  for (const row of rows) {
    const match = holdsAll(hay.get(row), terms)
    row.style.display = match ? '' : 'none'
    if (match) shown += 1
  }
  const filtered = terms.length > 0
  settleCount(count, filtered ? `Showing ${shown} of ${rows.length} electorates` : '')
  // An all-hidden table would leave its header behind under the empty state.
  const nothing = filtered && shown === 0
  if (empty) empty.hidden = !nothing
  if (table) table.hidden = nothing
  writeUrl({ q: text?.value.trim() })
}

text?.addEventListener('input', apply)
doneOnEnter(text)

clear?.addEventListener('click', () => {
  if (text) text.value = ''
  apply()
  text?.focus()
})

// A shared or reloaded link brings its filter with it. The rows are filtered
// on load even without one, so a query the browser restored into the box
// after Back is applied rather than left showing over every row.
const params = new URLSearchParams(location.search)
if (text && params.has('q')) text.value = params.get('q')
apply()
