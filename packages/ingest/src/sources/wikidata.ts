import { slugify, type House, type Person, type StateCode, STATES } from '@pollywiki/schema'
import { fetchJson } from '../http.js'
import type { Store } from '../store/types.js'

const SPARQL_ENDPOINT = 'https://query.wikidata.org/sparql'
const COMMONS_API = 'https://commons.wikimedia.org/w/api.php'

const HOUSE_POSITION: Record<string, House> = {
  Q18912794: 'representatives',
  Q6814428: 'senate',
}

// Current members: an open P39 (position held) statement with no end date.
const MEMBERS_QUERY = `
SELECT ?person ?personLabel ?houseQ ?partyLabel ?electorateLabel ?img ?start ?article WHERE {
  VALUES ?houseQ { wd:Q18912794 wd:Q6814428 }
  ?person p:P39 ?ps .
  ?ps ps:P39 ?houseQ .
  FILTER NOT EXISTS { ?ps pq:P582 ?end . }
  OPTIONAL { ?ps pq:P580 ?start . }
  OPTIONAL { ?ps pq:P768 ?electorate . }
  OPTIONAL { ?ps pq:P4100 ?party . }
  OPTIONAL { ?person wdt:P18 ?img . }
  OPTIONAL { ?article schema:about ?person ; schema:isPartOf <https://en.wikipedia.org/> . }
  SERVICE wikibase:label { bd:serviceParam wikibase:language "en" . }
}`

interface SparqlBinding {
  [key: string]: { value: string } | undefined
}

interface RawMember {
  wikidata: string
  name: string
  house: House
  group?: string
  district?: string
  commonsFile?: string
  since?: string
  wikipedia?: string
}

const FREE_LICENCES = /^(cc0|cc.by(.sa)?.\d|public domain|pd|no restrictions|attribution)/i

export async function syncWikidata(store: Store): Promise<Person[]> {
  const url = `${SPARQL_ENDPOINT}?query=${encodeURIComponent(MEMBERS_QUERY)}`
  const data = await fetchJson<{ results: { bindings: SparqlBinding[] } }>(url, {
    accept: 'application/sparql-results+json',
    minIntervalMs: 2000,
  })
  await store.putJson('raw/wikidata/members.json', data)

  const members = dedupe(data.results.bindings)
  const licences = await fetchCommonsLicences(
    members.map((m) => m.commonsFile).filter((f): f is string => Boolean(f)),
  )

  const people: Person[] = []
  const taken = new Map<string, RawMember>()
  for (const m of members) {
    let slug = slugify(m.name)
    if (taken.has(slug)) slug = slugify(`${m.name} ${m.district ?? m.house}`)
    taken.set(slug, m)

    const group = m.group ?? 'Independent'
    const isSenator = m.house === 'senate'
    const state = isSenator ? asState(m.district) : undefined
    const licence = m.commonsFile ? licences.get(m.commonsFile) : undefined

    people.push({
      slug,
      name: m.name,
      house: m.house,
      state,
      electorate: !isSenator && m.district ? slugify(m.district) : undefined,
      group,
      groupSlug: slugify(group),
      since: m.since?.slice(0, 10),
      ids: { wikidata: m.wikidata },
      photo:
        m.commonsFile && licence && FREE_LICENCES.test(licence.licence)
          ? {
              commonsFile: m.commonsFile,
              url: thumbUrl(m.commonsFile),
              licence: licence.licence,
              attribution: licence.attribution,
            }
          : undefined,
      links: m.wikipedia ? { wikipedia: m.wikipedia } : {},
    })
  }

  for (const person of people) {
    await store.putJson(`canonical/people/${person.slug}.json`, person)
  }
  return people
}

function dedupe(bindings: SparqlBinding[]): RawMember[] {
  const byId = new Map<string, RawMember & { _start: string }>()
  for (const b of bindings) {
    const uri = b.person?.value
    const name = b.personLabel?.value
    const houseQ = b.houseQ?.value.split('/').pop() ?? ''
    const house = HOUSE_POSITION[houseQ]
    if (!uri || !name || !house) continue
    const start = b.start?.value ?? ''
    const existing = byId.get(uri)
    // A person can carry several open statements; keep the most recent seat.
    if (existing && existing._start >= start) continue
    byId.set(uri, {
      _start: start,
      wikidata: uri.split('/').pop() ?? uri,
      name,
      house,
      group: b.partyLabel?.value,
      district: b.electorateLabel?.value,
      commonsFile: b.img?.value ? decodeURIComponent(b.img.value.split('/Special:FilePath/').pop() ?? '') : undefined,
      since: start || undefined,
      wikipedia: b.article?.value,
    })
  }
  return [...byId.values()].map(({ _start, ...m }) => m)
}

function asState(label: string | undefined): StateCode | undefined {
  const byName: Record<string, StateCode> = {
    'new south wales': 'NSW',
    victoria: 'VIC',
    queensland: 'QLD',
    'western australia': 'WA',
    'south australia': 'SA',
    tasmania: 'TAS',
    'australian capital territory': 'ACT',
    'northern territory': 'NT',
  }
  if (!label) return undefined
  const key = label.toLowerCase()
  if (byName[key]) return byName[key]
  return (STATES as readonly string[]).includes(label) ? (label as StateCode) : undefined
}

function thumbUrl(file: string): string {
  return `https://commons.wikimedia.org/wiki/Special:FilePath/${encodeURIComponent(file)}?width=400`
}

async function fetchCommonsLicences(
  files: string[],
): Promise<Map<string, { licence: string; attribution: string }>> {
  const out = new Map<string, { licence: string; attribution: string }>()
  for (let i = 0; i < files.length; i += 40) {
    const batch = files.slice(i, i + 40)
    const titles = batch.map((f) => `File:${f}`).join('|')
    const url =
      `${COMMONS_API}?action=query&prop=imageinfo&iiprop=extmetadata` +
      `&iiextmetadatafilter=LicenseShortName|Artist&format=json&titles=${encodeURIComponent(titles)}`
    const data = await fetchJson<{
      query?: { pages?: Record<string, { title?: string; imageinfo?: Array<{ extmetadata?: Record<string, { value?: string }> }> }> }
    }>(url, { minIntervalMs: 1500 })
    for (const page of Object.values(data.query?.pages ?? {})) {
      const file = page.title?.replace(/^File:/, '')
      const meta = page.imageinfo?.[0]?.extmetadata
      if (!file || !meta) continue
      out.set(file, {
        licence: meta.LicenseShortName?.value ?? 'unknown',
        attribution: stripHtml(meta.Artist?.value ?? 'Wikimedia Commons'),
      })
    }
  }
  return out
}

function stripHtml(html: string): string {
  return html.replace(/<[^>]*>/g, '').trim() || 'Wikimedia Commons'
}
