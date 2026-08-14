import { readFile } from 'node:fs/promises'
import {
  BUNDLE_FILES,
  type Bill,
  type Division,
  type Electorate,
  type ElectorateResult,
  type Meta,
  type Party,
  type Person,
  slugify,
} from '@pollywiki/schema'
import { readManifest } from './manifest.js'
import type { Store } from './store/types.js'

interface PartyReference {
  [groupSlug: string]: { name?: string; code?: string; colour?: string }
}

/**
 * Turns canonical entities into the precomputed bundles the site build reads.
 * Everything expensive happens here so page templates only render.
 */
export async function derive(store: Store): Promise<void> {
  const people = await loadAll<Person>(store, 'canonical/people/')
  const electorates = await loadAll<Electorate>(store, 'canonical/electorates/')
  const divisions = await loadAll<Division>(store, 'canonical/divisions/')
  const bills = await loadAll<Bill>(store, 'canonical/bills/')
  const elections = await loadAll<ElectorateResult>(store, 'canonical/elections/')

  const electorateBySlug = new Map(electorates.map((e) => [e.slug, e]))
  for (const person of people) {
    if (person.electorate) {
      const electorate = electorateBySlug.get(person.electorate)
      if (electorate) {
        person.state = electorate.state
        electorate.memberSlug = person.slug
      }
    }
  }

  computeVoteStats(people, divisions)
  linkBills(bills, divisions)
  const parties = await buildParties(people)

  const latestEvent = elections.map((e) => e.eventId).sort().at(-1)
  const currentElections = elections.filter((e) => e.eventId === latestEvent)

  await writeBundle(store, BUNDLE_FILES.people, sortBy(people, (p) => p.slug))
  await writeBundle(store, BUNDLE_FILES.parties, sortBy(parties, (p) => p.slug))
  await writeBundle(store, BUNDLE_FILES.electorates, sortBy(electorates, (e) => e.slug))
  await writeBundle(store, BUNDLE_FILES.divisions, sortBy(divisions, (d) => `${d.date}-${String(d.number).padStart(4, '0')}-${d.house}`).reverse())
  await writeBundle(store, BUNDLE_FILES.bills, sortBy(bills, (b) => b.title))
  await writeBundle(store, BUNDLE_FILES.elections, sortBy(currentElections, (e) => e.electorateSlug))

  const manifest = await readManifest(store)
  const meta: Meta = {
    generatedAt: new Date().toISOString(),
    sample: false,
    sources: manifest.sources,
  }
  await store.putJson('bundles/meta.json', meta)

  const quickSearch = [
    ...people.map((p) => ({
      t: 'person',
      slug: p.slug,
      name: p.name,
      sub: p.house === 'senate' ? `Senator · ${p.state ?? ''}` : `MP · ${titleFromSlug(p.electorate)}`,
    })),
    ...electorates.map((e) => ({ t: 'electorate', slug: e.slug, name: e.name, sub: `Electorate · ${e.state}` })),
  ]
  await store.putJson('bundles/quick-search.json', quickSearch)

  console.log(
    `derive: ${people.length} people, ${parties.length} parties, ${electorates.length} electorates, ` +
      `${divisions.length} divisions, ${bills.length} bills, ${currentElections.length} electorate results`,
  )
}

function computeVoteStats(people: Person[], divisions: Division[]): void {
  const stats = new Map<string, { eligible: number; voted: number; against: number }>()
  for (const division of divisions) {
    for (const vote of division.votes) {
      const s = stats.get(vote.personSlug) ?? { eligible: 0, voted: 0, against: 0 }
      s.voted++
      if (vote.againstGroupMajority) s.against++
      stats.set(vote.personSlug, s)
    }
  }
  const divisionsPerHouse = {
    representatives: divisions.filter((d) => d.house === 'representatives').length,
    senate: divisions.filter((d) => d.house === 'senate').length,
  }
  for (const person of people) {
    const s = stats.get(person.slug)
    const eligible = divisionsPerHouse[person.house]
    if (eligible === 0) continue
    person.stats = {
      divisionsEligible: eligible,
      divisionsVoted: s?.voted ?? 0,
      againstGroupMajority: s?.against ?? 0,
    }
  }
}

function linkBills(bills: Bill[], divisions: Division[]): void {
  const byId = new Map(bills.map((b) => [b.id, b]))
  for (const division of divisions) {
    for (const billId of division.billIds) {
      const bill = byId.get(billId)
      if (bill && !bill.divisionIds.includes(division.id)) bill.divisionIds.push(division.id)
    }
  }
}

async function buildParties(people: Person[]): Promise<Party[]> {
  let reference: PartyReference = {}
  try {
    const url = new URL('../../../data/reference/parties.json', import.meta.url)
    reference = JSON.parse(await readFile(url, 'utf8')) as PartyReference
  } catch {
    console.warn('derive: data/reference/parties.json not found, using defaults')
  }

  const groups = new Map<string, Party>()
  for (const person of people) {
    const ref = reference[person.groupSlug] ?? {}
    const party = groups.get(person.groupSlug) ?? {
      slug: person.groupSlug,
      name: ref.name ?? person.group,
      code: ref.code,
      colour: ref.colour,
      seats: { representatives: 0, senate: 0 },
    }
    if (party.seats) party.seats[person.house]++
    groups.set(person.groupSlug, party)
  }
  return [...groups.values()]
}

async function loadAll<T>(store: Store, prefix: string): Promise<T[]> {
  const keys = await store.list(prefix)
  const out: T[] = []
  for (const key of keys) {
    if (!key.endsWith('.json')) continue
    const value = await store.getJson<T>(key)
    if (value !== null) out.push(value)
  }
  return out
}

async function writeBundle(store: Store, file: string, records: unknown[]): Promise<void> {
  const jsonl = records.map((r) => JSON.stringify(r)).join('\n') + '\n'
  await store.putRaw(`bundles/${file}`, jsonl)
}

function sortBy<T>(items: T[], key: (item: T) => string): T[] {
  return [...items].sort((a, b) => key(a).localeCompare(key(b)))
}

function titleFromSlug(slug: string | undefined): string {
  if (!slug) return ''
  return slug.replace(/-/g, ' ').replace(/\b[a-z]/g, (c) => c.toUpperCase())
}
