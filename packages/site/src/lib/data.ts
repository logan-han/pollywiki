/**
 * Sole data access point for every page. Reads the JSONL bundles produced by
 * the ingest derive step (BUNDLES_DIR, or the committed sample data) once at
 * build time and exposes typed lookups. Pages template; they never compute.
 */
import { readFileSync, existsSync } from 'node:fs'
import { join, resolve } from 'node:path'
import {
  Bill,
  Division,
  Electorate,
  ElectorateResult,
  Meta,
  Party,
  Person,
} from '@pollywiki/schema'

const bundlesDir = process.env.BUNDLES_DIR
  ? resolve(process.env.BUNDLES_DIR)
  : resolve(process.cwd(), '../../data/sample/bundles')

function readJsonl<T>(file: string, schema: { parse: (v: unknown) => T }): T[] {
  const path = join(bundlesDir, file)
  if (!existsSync(path)) return []
  return readFileSync(path, 'utf8')
    .split('\n')
    .filter((line) => line.trim().length > 0)
    .map((line) => schema.parse(JSON.parse(line)))
}

export const people: Person[] = readJsonl('people.jsonl', Person)
export const parties: Party[] = readJsonl('parties.jsonl', Party).sort(
  (a, b) => seatTotal(b) - seatTotal(a) || a.name.localeCompare(b.name),
)
export const electorates: Electorate[] = readJsonl('electorates.jsonl', Electorate)
export const divisions: Division[] = readJsonl('divisions.jsonl', Division)
export const bills: Bill[] = readJsonl('bills.jsonl', Bill)
export const elections: ElectorateResult[] = readJsonl('elections.jsonl', ElectorateResult)

export const meta: Meta = (() => {
  const path = join(bundlesDir, 'meta.json')
  if (!existsSync(path)) {
    return Meta.parse({ generatedAt: new Date().toISOString(), sample: true, sources: {} })
  }
  return Meta.parse(JSON.parse(readFileSync(path, 'utf8')))
})()

const peopleBySlug = new Map(people.map((p) => [p.slug, p]))
const partiesBySlug = new Map(parties.map((p) => [p.slug, p]))
const electoratesBySlug = new Map(electorates.map((e) => [e.slug, e]))
const billsById = new Map(bills.map((b) => [b.id, b]))
const divisionsById = new Map(divisions.map((d) => [d.id, d]))
const electionsByElectorate = new Map(elections.map((e) => [e.electorateSlug, e]))

export const personBySlug = (slug: string): Person | undefined => peopleBySlug.get(slug)
export const partyBySlug = (slug: string): Party | undefined => partiesBySlug.get(slug)
export const electorateBySlug = (slug: string): Electorate | undefined => electoratesBySlug.get(slug)
export const billById = (id: string): Bill | undefined => billsById.get(id)
export const divisionById = (id: string): Division | undefined => divisionsById.get(id)
export const electionForElectorate = (slug: string): ElectorateResult | undefined =>
  electionsByElectorate.get(slug)

export function seatTotal(party: Party): number {
  return (party.seats?.representatives ?? 0) + (party.seats?.senate ?? 0)
}

export function peopleInHouse(house: Person['house']): Person[] {
  return people.filter((p) => p.house === house)
}

export function membersOfParty(slug: string): Person[] {
  return people.filter((p) => p.groupSlug === slug)
}

/** URL path segment for a division: date-number under its house. */
export function divisionKey(division: Division): string {
  return `${division.date}-${division.number}`
}

export function divisionByHouseKey(house: string, key: string): Division | undefined {
  const at = key.lastIndexOf('-')
  const id = `${house}/${key.slice(0, at)}/${key.slice(at + 1)}`
  return divisionsById.get(id)
}

export interface PersonVote {
  division: Division
  vote: 'aye' | 'no'
  againstGroupMajority: boolean
}

export function votesForPerson(slug: string): PersonVote[] {
  const out: PersonVote[] = []
  for (const division of divisions) {
    const vote = division.votes.find((v) => v.personSlug === slug)
    if (vote) {
      out.push({
        division,
        vote: vote.vote,
        againstGroupMajority: vote.againstGroupMajority === true,
      })
    }
  }
  return out
}

export interface GroupBreakdownRow {
  party: Party | undefined
  group: string
  groupSlug: string
  aye: number
  no: number
}

/** Per-party aye/no counts for one division. */
export function groupBreakdown(division: Division): GroupBreakdownRow[] {
  const rows = new Map<string, GroupBreakdownRow>()
  for (const vote of division.votes) {
    const person = peopleBySlug.get(vote.personSlug)
    const group = person?.group ?? 'Unknown'
    const groupSlug = person?.groupSlug ?? 'unknown'
    const row = rows.get(groupSlug) ?? {
      party: partiesBySlug.get(groupSlug),
      group,
      groupSlug,
      aye: 0,
      no: 0,
    }
    row[vote.vote]++
    rows.set(groupSlug, row)
  }
  return [...rows.values()].sort((a, b) => b.aye + b.no - (a.aye + a.no))
}

export function formatDate(iso: string): string {
  const [y, m, d] = iso.split('-').map(Number)
  if (!y || !m || !d) return iso
  const months = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec']
  return `${d} ${months[m - 1]} ${y}`
}

export function houseLabel(house: Person['house']): string {
  return house === 'senate' ? 'Senate' : 'House'
}

export const STATE_NAMES: Record<string, string> = {
  NSW: 'New South Wales',
  VIC: 'Victoria',
  QLD: 'Queensland',
  WA: 'Western Australia',
  SA: 'South Australia',
  TAS: 'Tasmania',
  ACT: 'Australian Capital Territory',
  NT: 'Northern Territory',
}
