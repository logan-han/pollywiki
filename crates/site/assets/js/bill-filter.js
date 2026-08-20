// Bills index: status pills plus a text box, combinable. ?q= pre-fills the
// text box so /search/ and the quick-search footer row can hand off here.
// Month divider rows recount as rows hide, and drop out once their month is
// empty.
const text = document.getElementById('bill-filter')
const statusGroup = document.getElementById('bill-status')
const items = [...document.querySelectorAll('#bill-rows > li')]
const list = document.getElementById('bill-rows')
const legend = document.querySelector('#bill-rows + .dots-legend')
const count = document.getElementById('filter-count')
const empty = document.getElementById('filter-empty')
const clear = document.getElementById('filter-clear')

// A month row owns every bill row that follows it up to the next one.
const months = []
for (const item of items) {
  if (item.dataset.month !== undefined) {
    months.push({ row: item, rows: [], label: item.querySelector('.n') })
  } else if (months.length) {
    months[months.length - 1].rows.push(item)
  }
}
const rows = items.filter((item) => item.dataset.status !== undefined)

let status = ''

function apply() {
  const needle = text ? text.value.trim().toLowerCase() : ''
  let shown = 0
  for (const row of rows) {
    const match =
      (!needle || (row.dataset.text ?? '').includes(needle)) &&
      (!status || row.dataset.status === status)
    row.style.display = match ? '' : 'none'
    if (match) shown += 1
  }
  for (const month of months) {
    const visible = month.rows.filter((row) => row.style.display !== 'none').length
    month.row.style.display = visible ? '' : 'none'
    if (month.label) month.label.textContent = `${visible} bill${visible === 1 ? '' : 's'}`
  }
  const filtered = Boolean(needle || status)
  if (count) count.textContent = filtered ? `Showing ${shown} of ${rows.length} bills` : ''
  // An all-hidden list would leave its rules behind under the empty state.
  const nothing = filtered && shown === 0
  if (empty) empty.hidden = !nothing
  if (list) list.hidden = nothing
  if (legend) legend.hidden = nothing
}

statusGroup?.addEventListener('click', (event) => {
  const button = event.target.closest('button')
  if (!button) return
  for (const other of statusGroup.querySelectorAll('button')) {
    other.setAttribute('aria-pressed', String(other === button))
  }
  status = button.dataset.status ?? ''
  apply()
})

text?.addEventListener('input', apply)

clear?.addEventListener('click', () => {
  if (text) text.value = ''
  status = ''
  const buttons = [...(statusGroup?.querySelectorAll('button') ?? [])]
  buttons.forEach((button, i) => button.setAttribute('aria-pressed', String(i === 0)))
  apply()
  text?.focus()
})

const q = new URLSearchParams(location.search).get('q')
if (q && text) {
  text.value = q
  apply()
}
