// A table wider than the screen scrolls sideways inside its wrapper. The
// wrapper takes a tab stop only while its table overflows, so the arrow keys
// can reach the hidden columns, and a table that fits adds no dead stop to
// the tab order. The wrapper is a named region, so the stop says which table.
function fit(wrapper) {
  if (wrapper.scrollWidth > wrapper.clientWidth) wrapper.tabIndex = 0
  else wrapper.removeAttribute('tabindex')
}

// The table is watched as well as its wrapper: a font arriving or a filter
// hiding rows changes the table's width, not the wrapper's. Observing also
// reports every size once at the start, which makes the first fit.
const watch = new ResizeObserver((entries) => {
  for (const entry of entries) {
    const wrapper = entry.target.closest('.table-scroll')
    if (wrapper) fit(wrapper)
  }
})
for (const wrapper of document.querySelectorAll('.table-scroll')) {
  watch.observe(wrapper)
  if (wrapper.firstElementChild) watch.observe(wrapper.firstElementChild)
}
