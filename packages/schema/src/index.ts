import { z } from 'zod'

export const HOUSES = ['representatives', 'senate'] as const
export const House = z.enum(HOUSES)
export type House = z.infer<typeof House>

export const STATES = ['NSW', 'VIC', 'QLD', 'WA', 'SA', 'TAS', 'ACT', 'NT'] as const
export const StateCode = z.enum(STATES)
export type StateCode = z.infer<typeof StateCode>

export const Photo = z.object({
  commonsFile: z.string(),
  url: z.string().url(),
  licence: z.string(),
  attribution: z.string(),
})
export type Photo = z.infer<typeof Photo>

export const Person = z.object({
  slug: z.string().min(1),
  name: z.string().min(1),
  house: House,
  state: StateCode.optional(),
  electorate: z.string().optional(),
  group: z.string().min(1),
  groupSlug: z.string().min(1),
  since: z.string().optional(),
  ids: z
    .object({
      wikidata: z.string().optional(),
      tvfy: z.number().optional(),
      aph: z.string().optional(),
      aecCandidate: z.number().optional(),
    })
    .default({}),
  photo: Photo.optional(),
  links: z.object({ wikipedia: z.string().url().optional() }).default({}),
  stats: z
    .object({
      divisionsEligible: z.number(),
      divisionsVoted: z.number(),
      againstGroupMajority: z.number(),
    })
    .optional(),
})
export type Person = z.infer<typeof Person>

export const Party = z.object({
  slug: z.string().min(1),
  name: z.string().min(1),
  code: z.string().optional(),
  colour: z.string().optional(),
  seats: z
    .object({
      representatives: z.number(),
      senate: z.number(),
    })
    .optional(),
})
export type Party = z.infer<typeof Party>

export const Electorate = z.object({
  slug: z.string().min(1),
  name: z.string().min(1),
  state: StateCode,
  memberSlug: z.string().optional(),
})
export type Electorate = z.infer<typeof Electorate>

export const VoteCast = z.object({
  personSlug: z.string(),
  vote: z.enum(['aye', 'no']),
  teller: z.boolean().optional(),
  againstGroupMajority: z.boolean().optional(),
})
export type VoteCast = z.infer<typeof VoteCast>

export const Division = z.object({
  id: z.string().min(1),
  house: House,
  date: z.string().min(10),
  number: z.number(),
  name: z.string().min(1),
  clarifiedName: z.string().optional(),
  result: z.enum(['passed', 'rejected']),
  ayes: z.number(),
  noes: z.number(),
  billIds: z.array(z.string()).default([]),
  links: z
    .object({
      hansard: z.string().url().optional(),
      tvfy: z.string().url().optional(),
    })
    .default({}),
  votes: z.array(VoteCast).default([]),
})
export type Division = z.infer<typeof Division>

export const Bill = z.object({
  id: z.string().min(1),
  title: z.string().min(1),
  parliament: z.number(),
  chamber: House,
  type: z.string().optional(),
  sponsor: z.string().optional(),
  portfolio: z.string().optional(),
  status: z.string().min(1),
  timeline: z.array(z.object({ date: z.string(), event: z.string() })).default([]),
  links: z
    .object({
      aph: z.string().url().optional(),
      text: z.string().url().optional(),
      em: z.string().url().optional(),
    })
    .default({}),
  divisionIds: z.array(z.string()).default([]),
})
export type Bill = z.infer<typeof Bill>

export const CandidateResult = z.object({
  name: z.string(),
  party: z.string(),
  partyCode: z.string().optional(),
  votes: z.number(),
  pct: z.number(),
  swing: z.number().optional(),
  elected: z.boolean(),
})
export type CandidateResult = z.infer<typeof CandidateResult>

export const ElectorateResult = z.object({
  eventId: z.string(),
  eventName: z.string(),
  electorateSlug: z.string(),
  electorateName: z.string(),
  state: StateCode,
  firstPrefs: z.array(CandidateResult),
  tcp: z.array(CandidateResult).default([]),
})
export type ElectorateResult = z.infer<typeof ElectorateResult>

export const SourceStatus = z.object({
  lastSync: z.string(),
  ok: z.boolean(),
  note: z.string().optional(),
})

export const Meta = z.object({
  generatedAt: z.string(),
  sample: z.boolean().default(false),
  sources: z.record(z.string(), SourceStatus).default({}),
})
export type Meta = z.infer<typeof Meta>

/** Kebab-case slug: lowercase, ASCII, hyphen separated. Stable across syncs. */
export function slugify(input: string): string {
  return input
    .normalize('NFKD')
    .replace(/[̀-ͯ]/g, '')
    .toLowerCase()
    .replace(/['’]/g, '')
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
}

export const BUNDLE_FILES = {
  people: 'people.jsonl',
  parties: 'parties.jsonl',
  electorates: 'electorates.jsonl',
  divisions: 'divisions.jsonl',
  bills: 'bills.jsonl',
  elections: 'elections.jsonl',
} as const
export type BundleName = keyof typeof BUNDLE_FILES

export const BUNDLE_SCHEMAS: Record<BundleName, z.ZodTypeAny> = {
  people: Person,
  parties: Party,
  electorates: Electorate,
  divisions: Division,
  bills: Bill,
  elections: ElectorateResult,
}
