// Bills index: status pills plus a text box, combinable, and kept in the URL
// as ?q= and ?status=, so /search/ and the quick-search footer row can hand a
// query off here. Month dividers and the month strip recount as rows hide,
// and drop out once their month is empty.
const text = document.getElementById('bill-filter')
const statusGroup = document.getElementById('bill-status')
const items = [...document.querySelectorAll('#bill-rows > li')]
const list = document.getElementById('bill-rows')
const strip = document.getElementById('month-jump')
const legend = document.querySelector('.dots-legend')
const count = document.getElementById('filter-count')
const empty = document.getElementById('filter-empty')
const clear = document.getElementById('filter-clear')

const months = monthsOf(items, strip)
const rows = items.filter((item) => item.dataset.status !== undefined)
// Each row's title and portfolio, folded once rather than on every keystroke.
const hay = new Map(rows.map((row) => [row, fold(row.dataset.text)]))

let status = ''

function apply() {
  const terms = termsOf(text)
  let shown = 0
  for (const row of rows) {
    const match = holdsAll(hay.get(row), terms) && (!status || row.dataset.status === status)
    row.style.display = match ? '' : 'none'
    if (match) shown += 1
  }
  for (const month of months) {
    const visible = month.rows.filter((row) => row.style.display !== 'none').length
    recountMonth(month, visible, 'bill')
  }
  const filtered = Boolean(terms.length || status)
  settleCount(count, filtered ? `Showing ${shown} of ${rows.length} bills` : '')
  // An all-hidden list would leave its rules behind under the empty state.
  const nothing = filtered && shown === 0
  if (empty) empty.hidden = !nothing
  if (list) list.hidden = nothing
  if (strip) strip.hidden = nothing
  if (legend) legend.hidden = nothing
  writeUrl({ q: text?.value.trim(), status })
}

statusGroup?.addEventListener('click', (event) => {
  const button = event.target.closest('button')
  if (!button) return
  status = press(statusGroup, 'status', button.dataset.status ?? '')
  apply()
})

text?.addEventListener('input', apply)
doneOnEnter(text)

clear?.addEventListener('click', () => {
  if (text) text.value = ''
  status = press(statusGroup, 'status', '')
  apply()
  text?.focus()
})

// A shared or reloaded link brings its filter with it. The rows are filtered
// on load even without one, so a query the browser restored into the box
// after Back is applied rather than left showing over every row.
const params = new URLSearchParams(location.search)
if (text && params.has('q')) text.value = params.get('q')
status = press(statusGroup, 'status', params.get('status') ?? '')
apply()

// Back or Forward between the page's own entries brings back that entry's
// filter.
onRestore((params) => {
  restoreText(text, params.get('q'))
  status = press(statusGroup, 'status', params.get('status') ?? '')
  apply()
})
