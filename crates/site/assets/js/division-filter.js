// Divisions index: chamber segment plus a text box. Month divider rows recount
// as rows hide, and drop out entirely once their month is empty.
const text = document.getElementById('division-filter-text')
const houseGroup = document.getElementById('division-house')
const items = [...document.querySelectorAll('#division-list > li')]
const list = document.getElementById('division-list')
const count = document.getElementById('filter-count')
const empty = document.getElementById('filter-empty')
const clear = document.getElementById('filter-clear')

// A month row owns every division row that follows it up to the next one.
const months = []
for (const item of items) {
  if (item.dataset.month !== undefined) {
    months.push({ row: item, rows: [], label: item.querySelector('.n') })
  } else if (months.length) {
    months[months.length - 1].rows.push(item)
  }
}
const rows = items.filter((item) => item.dataset.house !== undefined)

let house = ''

function apply() {
  const needle = text ? text.value.trim().toLowerCase() : ''
  let shown = 0
  for (const row of rows) {
    const match =
      (!needle || (row.dataset.text ?? '').includes(needle)) &&
      (!house || row.dataset.house === house)
    row.style.display = match ? '' : 'none'
    if (match) shown += 1
  }
  for (const month of months) {
    const visible = month.rows.filter((row) => row.style.display !== 'none').length
    month.row.style.display = visible ? '' : 'none'
    if (month.label) month.label.textContent = `${visible} division${visible === 1 ? '' : 's'}`
  }
  const filtered = Boolean(needle || house)
  if (count) count.textContent = filtered ? `Showing ${shown} of ${rows.length} divisions` : ''
  // An all-hidden ledger would leave its rules behind under the empty state.
  const nothing = filtered && shown === 0
  if (empty) empty.hidden = !nothing
  if (list) list.hidden = nothing
}

houseGroup?.addEventListener('click', (event) => {
  const button = event.target.closest('button')
  if (!button) return
  for (const other of houseGroup.querySelectorAll('button')) {
    other.setAttribute('aria-pressed', String(other === button))
  }
  house = button.dataset.house ?? ''
  apply()
})

text?.addEventListener('input', apply)

clear?.addEventListener('click', () => {
  if (text) text.value = ''
  house = ''
  const buttons = [...(houseGroup?.querySelectorAll('button') ?? [])]
  buttons.forEach((button, i) => button.setAttribute('aria-pressed', String(i === 0)))
  apply()
  text?.focus()
})
