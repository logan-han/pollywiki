import { slugify, type Division, type Person, type VoteCast } from '@pollywiki/schema'
import { fetchJson } from '../http.js'
import type { Store } from '../store/types.js'

const BASE = 'https://theyvoteforyou.org.au/api/v1'
const REQUEST_CAP = 500

interface TvfyPersonSummary {
  id: number
  latest_member: {
    id: number
    name: { first: string; last: string }
    electorate: string
    house: 'representatives' | 'senate'
    party: string
  }
}

interface TvfyDivisionSummary {
  id: number
  house: 'representatives' | 'senate'
  date: string
  number: number
  name: string
  clarified_name?: string
  aye_votes: number
  no_votes: number
  possible_turnout?: number
  rebellions?: number
}

interface TvfyDivisionDetail extends TvfyDivisionSummary {
  summary?: string
  votes: Array<{
    member: {
      person: { id: number }
      name: { first: string; last: string }
      party: string
    }
    vote: 'aye' | 'no'
  }>
  bills?: Array<{ id: number; title: string; official_id?: string }>
}

/**
 * Divisions and votes from They Vote For You (data licence: ODbL 1.0).
 * Requires TVFY_API_KEY. Free access is low-volume and non-commercial;
 * email the OpenAustralia Foundation before any bulk backfill.
 */
export async function syncTvfy(store: Store, people: Person[]): Promise<void> {
  const key = process.env.TVFY_API_KEY
  if (!key) throw new Error('TVFY_API_KEY not set; skipping They Vote For You sync')
  let requests = 0
  const get = async <T>(path: string, params: Record<string, string> = {}): Promise<T> => {
    if (++requests > REQUEST_CAP) throw new Error(`TVFY request cap (${REQUEST_CAP}) reached`)
    const qs = new URLSearchParams({ ...params, key })
    return fetchJson<T>(`${BASE}/${path}.json?${qs}`, { minIntervalMs: 1200 })
  }

  const tvfyPeople = await get<TvfyPersonSummary[]>('people')
  await store.putJson('raw/tvfy/people.json', tvfyPeople)
  const crosswalk = matchPeople(tvfyPeople, people)

  const manifest = await store.getJson<{ lastDivisionDate?: string }>('state/tvfy-cursor.json')
  const overlapDays = 14
  const startDate = manifest?.lastDivisionDate
    ? isoDaysBefore(manifest.lastDivisionDate, overlapDays)
    : '2025-07-01' // opening of the 48th Parliament

  let latestDate = manifest?.lastDivisionDate ?? startDate
  for (const house of ['representatives', 'senate'] as const) {
    const summaries = await get<TvfyDivisionSummary[]>('divisions', {
      house,
      start_date: startDate,
    })
    for (const summary of summaries) {
      const id = `${summary.house}/${summary.date}/${summary.number}`
      const existing = await store.getJson<Division>(`canonical/divisions/${keyFor(id)}.json`)
      if (existing && existing.ayes === summary.aye_votes && existing.noes === summary.no_votes) {
        continue
      }
      const detail = await get<TvfyDivisionDetail>(`divisions/${summary.id}`)
      await store.putJson(`raw/tvfy/divisions/${summary.id}.json`, detail)
      const division = toDivision(detail, crosswalk)
      await store.putJson(`canonical/divisions/${keyFor(division.id)}.json`, division)
      if (division.date > latestDate) latestDate = division.date
    }
  }
  await store.putJson('state/tvfy-cursor.json', { lastDivisionDate: latestDate })

  // Persist discovered TVFY ids back onto people.
  for (const person of people) {
    const tvfyId = crosswalk.get(person.slug)
    if (tvfyId && person.ids.tvfy !== tvfyId) {
      person.ids.tvfy = tvfyId
      await store.putJson(`canonical/people/${person.slug}.json`, person)
    }
  }
}

/** Match TVFY people to canonical people by name, then by electorate on ties. */
function matchPeople(tvfyPeople: TvfyPersonSummary[], people: Person[]): Map<string, number> {
  const bySlug = new Map<string, number>()
  const unmatched: string[] = []
  for (const tp of tvfyPeople) {
    const name = `${tp.latest_member.name.first} ${tp.latest_member.name.last}`
    const nameSlug = slugify(name)
    const candidates = people.filter(
      (p) => p.slug === nameSlug || slugify(p.name) === nameSlug,
    )
    const match =
      candidates.length === 1
        ? candidates[0]
        : candidates.find(
            (p) =>
              p.house === tp.latest_member.house &&
              (p.electorate === slugify(tp.latest_member.electorate) ||
                p.state === tp.latest_member.electorate),
          )
    if (match) bySlug.set(match.slug, tp.id)
    else unmatched.push(name)
  }
  if (unmatched.length > 0) {
    console.warn(`tvfy: ${unmatched.length} people unmatched: ${unmatched.join(', ')}`)
  }
  const tvfyToSlug = new Map<string, number>()
  for (const [slug, id] of bySlug) tvfyToSlug.set(slug, id)
  return tvfyToSlug
}

function toDivision(detail: TvfyDivisionDetail, crosswalk: Map<string, number>): Division {
  const idBySlug = new Map<number, string>()
  for (const [slug, id] of crosswalk) idBySlug.set(id, slug)

  const votes: VoteCast[] = []
  const groupTallies = new Map<string, { aye: number; no: number }>()
  for (const v of detail.votes) {
    const tally = groupTallies.get(v.member.party) ?? { aye: 0, no: 0 }
    tally[v.vote]++
    groupTallies.set(v.member.party, tally)
  }
  for (const v of detail.votes) {
    const slug = idBySlug.get(v.member.person.id) ?? slugify(`${v.member.name.first} ${v.member.name.last}`)
    const tally = groupTallies.get(v.member.party)
    const majority = tally && tally.aye !== tally.no ? (tally.aye > tally.no ? 'aye' : 'no') : undefined
    votes.push({
      personSlug: slug,
      vote: v.vote,
      againstGroupMajority: majority !== undefined && v.vote !== majority ? true : undefined,
    })
  }

  return {
    id: `${detail.house}/${detail.date}/${detail.number}`,
    house: detail.house,
    date: detail.date,
    number: detail.number,
    name: detail.name,
    clarifiedName: detail.clarified_name || undefined,
    result: detail.aye_votes > detail.no_votes ? 'passed' : 'rejected',
    ayes: detail.aye_votes,
    noes: detail.no_votes,
    billIds: (detail.bills ?? []).map((b) => String(b.official_id ?? b.id)),
    links: {
      tvfy: `https://theyvoteforyou.org.au/divisions/${detail.house}/${detail.date}/${detail.number}`,
    },
    votes,
  }
}

export function keyFor(divisionId: string): string {
  return divisionId.replaceAll('/', '-')
}

function isoDaysBefore(iso: string, days: number): string {
  const d = new Date(`${iso}T00:00:00Z`)
  d.setUTCDate(d.getUTCDate() - days)
  return d.toISOString().slice(0, 10)
}
