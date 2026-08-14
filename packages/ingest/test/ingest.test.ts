import { describe, expect, it } from 'vitest'
import { slugify } from '@pollywiki/schema'
import { toCandidates } from '../src/sources/aec.js'
import { keyFor } from '../src/sources/tvfy.js'

describe('slugify', () => {
  it('handles names with punctuation and diacritics', () => {
    expect(slugify('Anthony Albanese')).toBe('anthony-albanese')
    expect(slugify("Pauline Hanson's One Nation")).toBe('pauline-hansons-one-nation')
    expect(slugify('Liberal–National Coalition')).toBe('liberal-national-coalition')
    expect(slugify('Zoë Daniel')).toBe('zoe-daniel')
    expect(slugify("O'Brien")).toBe('obrien')
  })
})

describe('aec toCandidates', () => {
  const rows = [
    {
      DivisionNm: 'Bean',
      GivenNm: 'DAVID',
      Surname: 'SMITH',
      PartyNm: 'Australian Labor Party',
      PartyAb: 'ALP',
      TotalVotes: '60000',
      Swing: '1.25',
      Elected: 'Y',
    },
    {
      DivisionNm: 'Bean',
      GivenNm: 'Jessie',
      Surname: 'PRICE',
      PartyNm: 'Independent',
      PartyAb: 'IND',
      TotalVotes: '40000',
      Swing: '',
      Elected: 'N',
    },
    {
      DivisionNm: 'Fenner',
      GivenNm: 'Andrew',
      Surname: 'LEIGH',
      PartyNm: 'Australian Labor Party',
      PartyAb: 'ALP',
      TotalVotes: '70000',
      Swing: '2.0',
      Elected: 'Y',
    },
  ]

  it('filters to the electorate, computes percentages and sorts by votes', () => {
    const candidates = toCandidates(rows, 'Bean')
    expect(candidates).toHaveLength(2)
    expect(candidates[0]).toMatchObject({
      name: 'David Smith',
      party: 'Australian Labor Party',
      votes: 60000,
      pct: 60,
      swing: 1.25,
      elected: true,
    })
    expect(candidates[1]?.swing).toBeUndefined()
  })

  it('title-cases surnames including Mc prefixes', () => {
    const candidates = toCandidates(
      [{ ...rows[0], GivenNm: 'MICHAEL', Surname: 'MCCORMACK' }],
      'Bean',
    )
    expect(candidates[0]?.name).toBe('Michael McCormack')
  })
})

describe('tvfy keyFor', () => {
  it('flattens division ids into store keys', () => {
    expect(keyFor('representatives/2025-07-24/3')).toBe('representatives-2025-07-24-3')
  })
})
