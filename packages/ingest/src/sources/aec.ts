import { parse } from 'csv-parse/sync'
import {
  slugify,
  StateCode,
  type CandidateResult,
  type Electorate,
  type ElectorateResult,
} from '@pollywiki/schema'
import { fetchText } from '../http.js'
import type { Store } from '../store/types.js'

const EVENT_NAMES: Record<string, string> = {
  '31496': '2025 federal election',
  '27966': '2022 federal election',
}

const FILES = {
  members: 'HouseMembersElectedDownload',
  firstPrefs: 'HouseFirstPrefsByCandidateByVoteTypeDownload',
  tcp: 'HouseTcpByCandidateByVoteTypeDownload',
} as const

interface AecRow {
  [column: string]: string
}

export async function syncAec(store: Store, eventId: string): Promise<void> {
  const rows: Record<keyof typeof FILES, AecRow[]> = { members: [], firstPrefs: [], tcp: [] }

  for (const [name, file] of Object.entries(FILES) as Array<[keyof typeof FILES, string]>) {
    const url = `https://results.aec.gov.au/${eventId}/Website/Downloads/${file}-${eventId}.csv`
    const csv = await fetchText(url, { minIntervalMs: 1500 })
    await store.putRaw(`raw/aec/${eventId}/${file}.csv`, csv)
    // AEC files carry a metadata title line above the real header.
    rows[name] = parse(csv.slice(csv.indexOf('\n') + 1), {
      columns: true,
      skip_empty_lines: true,
    }) as AecRow[]
  }

  const eventName = EVENT_NAMES[eventId] ?? `AEC event ${eventId}`
  const byElectorate = new Map<string, { name: string; state: string }>()
  for (const row of rows.members) {
    byElectorate.set(row.DivisionNm ?? '', {
      name: row.DivisionNm ?? '',
      state: row.StateAb ?? '',
    })
  }

  for (const { name, state } of byElectorate.values()) {
    const electorateSlug = slugify(name)
    const stateCode = StateCode.parse(state)

    const electorate: Electorate = { slug: electorateSlug, name, state: stateCode }
    await store.putJson(`canonical/electorates/${electorateSlug}.json`, electorate)

    const result: ElectorateResult = {
      eventId,
      eventName,
      electorateSlug,
      electorateName: name,
      state: stateCode,
      firstPrefs: toCandidates(rows.firstPrefs, name),
      tcp: toCandidates(rows.tcp, name),
    }
    await store.putJson(`canonical/elections/${eventId}/${electorateSlug}.json`, result)
  }
}

export function toCandidates(rows: AecRow[], electorateName: string): CandidateResult[] {
  const mine = rows.filter((r) => r.DivisionNm === electorateName)
  const total = mine.reduce((sum, r) => sum + Number(r.TotalVotes ?? 0), 0)
  return mine
    .map((r) => ({
      name: titleCase(`${r.GivenNm ?? ''} ${r.Surname ?? ''}`.trim()),
      party: r.PartyNm || 'Independent',
      partyCode: r.PartyAb || undefined,
      votes: Number(r.TotalVotes ?? 0),
      pct: total > 0 ? round2((Number(r.TotalVotes ?? 0) / total) * 100) : 0,
      swing: r.Swing !== undefined && r.Swing !== '' ? Number(r.Swing) : undefined,
      elected: r.Elected === 'Y',
    }))
    .sort((a, b) => b.votes - a.votes)
}

function titleCase(name: string): string {
  return name
    .toLowerCase()
    .replace(/(^|[\s\-'])([a-z])/g, (m, sep: string, ch: string) => sep + ch.toUpperCase())
    .replace(/\bMc([a-z])/g, (_, ch: string) => `Mc${ch.toUpperCase()}`)
}

function round2(n: number): number {
  return Math.round(n * 100) / 100
}
