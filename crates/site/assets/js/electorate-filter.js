// Electorates index: one text box over the table rows.
const text = document.getElementById('electorate-filter')
const rows = [...document.querySelectorAll('#electorate-table tbody tr')]
const table = document.getElementById('electorate-table')
const count = document.getElementById('filter-count')
const empty = document.getElementById('filter-empty')
const clear = document.getElementById('filter-clear')

function apply() {
  const needle = text ? text.value.trim().toLowerCase() : ''
  let shown = 0
  for (const row of rows) {
    const match = !needle || (row.dataset.text ?? '').includes(needle)
    row.style.display = match ? '' : 'none'
    if (match) shown += 1
  }
  if (count) count.textContent = needle ? `Showing ${shown} of ${rows.length} electorates` : ''
  // An all-hidden table would leave its header behind under the empty state.
  const nothing = Boolean(needle) && shown === 0
  if (empty) empty.hidden = !nothing
  if (table) table.hidden = nothing
}

text?.addEventListener('input', apply)

clear?.addEventListener('click', () => {
  if (text) text.value = ''
  apply()
  text?.focus()
})
