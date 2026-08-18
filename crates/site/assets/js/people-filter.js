// People index: chamber segment, party pills and a text box, all combinable.
// Without JS every card stays visible.
const text = document.getElementById('people-filter')
const houseGroup = document.getElementById('house-filter')
const partyGroup = document.getElementById('group-filter')
const cells = [...document.querySelectorAll('.person-cell')]
const grid = document.getElementById('person-grid')
const count = document.getElementById('filter-count')
const empty = document.getElementById('filter-empty')
const clear = document.getElementById('filter-clear')

let house = ''
let party = ''

function apply() {
  const needle = text ? text.value.trim().toLowerCase() : ''
  let shown = 0
  for (const cell of cells) {
    const match =
      (!needle || (cell.dataset.name ?? '').includes(needle)) &&
      (!house || cell.dataset.house === house) &&
      (!party || cell.dataset.group === party)
    cell.style.display = match ? '' : 'none'
    if (match) shown += 1
  }
  const filtered = Boolean(needle || house || party)
  if (count) count.textContent = filtered ? `Showing ${shown} of ${cells.length} people` : ''
  // An all-hidden grid would leave its frame behind under the empty state.
  const nothing = filtered && shown === 0
  if (empty) empty.hidden = !nothing
  if (grid) grid.hidden = nothing
}

// Each segmented/pill group tracks one value; the first button is the "all" reset.
function wire(group, set) {
  group?.addEventListener('click', (event) => {
    const button = event.target.closest('button')
    if (!button) return
    for (const other of group.querySelectorAll('button')) {
      other.setAttribute('aria-pressed', String(other === button))
    }
    set(button.dataset.house ?? button.dataset.group ?? '')
    apply()
  })
}

wire(houseGroup, (value) => (house = value))
wire(partyGroup, (value) => (party = value))
text?.addEventListener('input', apply)

clear?.addEventListener('click', () => {
  if (text) text.value = ''
  house = ''
  party = ''
  for (const group of [houseGroup, partyGroup]) {
    const buttons = [...(group?.querySelectorAll('button') ?? [])]
    buttons.forEach((button, i) => button.setAttribute('aria-pressed', String(i === 0)))
  }
  apply()
  text?.focus()
})
