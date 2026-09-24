// People index: chamber segment, party pills and a text box, all combinable
// and kept in the URL as ?q=, ?house= and ?party=. Without JS every card
// stays visible. Former members answer to the text alone: the chamber and
// party controls describe the parliament as it stands.
const text = document.getElementById('people-filter')
const houseGroup = document.getElementById('house-filter')
const partyGroup = document.getElementById('group-filter')
const cells = [...document.querySelectorAll('#person-grid .person-cell')]
const former = [...document.querySelectorAll('#former-members .person-cell')]
const formerSection = document.getElementById('former-members')
const grid = document.getElementById('person-grid')
const count = document.getElementById('filter-count')
const empty = document.getElementById('filter-empty')
const clear = document.getElementById('filter-clear')
const partyButtons = [...(partyGroup?.querySelectorAll('button[data-group]:not([data-group=""])') ?? [])]

// Each card's name, seat and state, folded once rather than on every keystroke.
const hay = new Map([...cells, ...former].map((cell) => [cell, fold(cell.dataset.name)]))

let house = ''
let party = ''

function apply() {
  const terms = termsOf(text)
  const named = (cell) => holdsAll(hay.get(cell), terms)
  const inHouse = (cell) => !house || cell.dataset.house === house
  let shown = 0
  for (const cell of cells) {
    const match = named(cell) && inHouse(cell) && (!party || cell.dataset.group === party)
    cell.style.display = match ? '' : 'none'
    if (match) shown += 1
  }
  let formerShown = 0
  for (const cell of former) {
    const match = !house && !party && named(cell)
    cell.style.display = match ? '' : 'none'
    if (match) formerShown += 1
  }
  // A party no card could match under the chosen chamber and text is greyed
  // out rather than left to lead to an empty grid. The pressed pill stays
  // live, so it can always be let go.
  for (const button of partyButtons) {
    const possible = cells.some(
      (cell) => cell.dataset.group === button.dataset.group && inHouse(cell) && named(cell),
    )
    button.disabled = !possible && button.getAttribute('aria-pressed') !== 'true'
  }
  const filtered = Boolean(terms.length || house || party)
  const alsoFormer = formerShown
    ? ` and ${formerShown} former member${formerShown === 1 ? '' : 's'}`
    : ''
  settleCount(count, filtered ? `Showing ${shown} of ${cells.length} people${alsoFormer}` : '')
  // An all-hidden grid would leave its frame behind, so each grid goes with
  // its last card, and the empty state shows only when both have gone.
  if (grid) grid.hidden = filtered && shown === 0
  if (formerSection) formerSection.hidden = filtered && formerShown === 0
  const nothing = filtered && shown === 0 && formerShown === 0
  if (empty) empty.hidden = !nothing
  writeUrl({ q: text?.value.trim(), house, party })
}

houseGroup?.addEventListener('click', (event) => {
  const button = event.target.closest('button')
  if (!button) return
  house = press(houseGroup, 'house', button.dataset.house ?? '')
  apply()
})

partyGroup?.addEventListener('click', (event) => {
  const button = event.target.closest('button')
  if (!button) return
  party = press(partyGroup, 'group', button.dataset.group ?? '')
  apply()
})

text?.addEventListener('input', apply)
doneOnEnter(text)

clear?.addEventListener('click', () => {
  if (text) text.value = ''
  house = press(houseGroup, 'house', '')
  party = press(partyGroup, 'group', '')
  apply()
  text?.focus()
})

// A shared or reloaded link brings its filter with it. The cards are filtered
// on load even without one, so a query the browser restored into the box
// after Back is applied rather than left showing over every card.
const params = new URLSearchParams(location.search)
if (text && params.has('q')) text.value = params.get('q')
house = press(houseGroup, 'house', params.get('house') ?? '')
party = press(partyGroup, 'group', params.get('party') ?? '')
apply()
