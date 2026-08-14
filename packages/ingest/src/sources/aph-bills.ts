import { slugify, type Bill } from '@pollywiki/schema'
import { fetchJson } from '../http.js'
import type { Store } from '../store/types.js'

/**
 * Bills before the federal parliament. There is no official API; this reads
 * the JSON endpoints behind ParlWork (parlwork.aph.gov.au). Endpoints are
 * undocumented and may change; failures are reported loudly, never written.
 */
export async function syncAphBills(store: Store, parliament = 48): Promise<void> {
  const bills: Bill[] = []
  let page = 1
  for (;;) {
    const url = `https://parlwork.aph.gov.au/api/bills/all?pageSize=100&pageNumber=${page}&parliamentNumber=${parliament}`
    const data = await fetchJson<ParlWorkPage>(url, { minIntervalMs: 1500 })
    if (!Array.isArray(data.items)) throw new Error('parlwork: unexpected response shape')
    await store.putJson(`raw/aph/bills-page-${page}.json`, data)
    for (const item of data.items) bills.push(toBill(item, parliament))
    if (data.items.length < 100 || page >= 10) break
    page++
  }
  for (const bill of bills) {
    await store.putJson(`canonical/bills/${bill.id}.json`, bill)
  }
  console.log(`aph-bills: ${bills.length} bills`)
}

interface ParlWorkItem {
  billId?: string
  id?: string
  title?: string
  shortTitle?: string
  chamber?: string
  originatingChamber?: string
  status?: string
  billType?: string
  sponsor?: string
  portfolio?: string
  introducedDate?: string
  link?: string
}

interface ParlWorkPage {
  items?: ParlWorkItem[]
}

function toBill(item: ParlWorkItem, parliament: number): Bill {
  const title = item.title ?? item.shortTitle ?? 'Untitled bill'
  const id = String(item.billId ?? item.id ?? slugify(title))
  const chamberRaw = (item.chamber ?? item.originatingChamber ?? '').toLowerCase()
  return {
    id,
    title,
    parliament,
    chamber: chamberRaw.includes('senate') ? 'senate' : 'representatives',
    type: item.billType || undefined,
    sponsor: item.sponsor || undefined,
    portfolio: item.portfolio || undefined,
    status: item.status ?? 'Before parliament',
    timeline: item.introducedDate
      ? [{ date: item.introducedDate.slice(0, 10), event: 'Introduced' }]
      : [],
    links: item.link ? { aph: item.link } : {},
    divisionIds: [],
  }
}
