// Bills index: status pills plus a text box, combinable. ?q= pre-fills the
// text box so /search/ and the quick-search footer row can hand off here.
const text = document.getElementById('bill-filter')
const statusGroup = document.getElementById('bill-status')
const rows = [...document.querySelectorAll('#bill-rows > li')]
const list = document.getElementById('bill-rows')
const legend = document.querySelector('#bill-rows + .dots-legend')
const count = document.getElementById('filter-count')
const empty = document.getElementById('filter-empty')
const clear = document.getElementById('filter-clear')

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
