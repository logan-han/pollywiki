import { LocalStore } from './store/local.js'
import { S3Store } from './store/s3.js'
import type { Store } from './store/types.js'
import { recordSync } from './manifest.js'
import { syncWikidata } from './sources/wikidata.js'
import { syncAec } from './sources/aec.js'
import { syncTvfy } from './sources/tvfy.js'
import { syncAphBills } from './sources/aph-bills.js'
import { derive } from './derive.js'
import type { Person } from '@pollywiki/schema'

const USAGE = `usage: cli.ts <sync|derive|all> [options]
  --store local|s3       default local (.store/); s3 needs POLLYWIKI_DATA_BUCKET
  --sources a,b,c        default wikidata,aph,tvfy; also: aec
  --event <id>           AEC event id for the aec source (default 31496)
`

async function main(): Promise<void> {
  const [command, ...rest] = process.argv.slice(2)
  const options = parseOptions(rest)
  if (!command || !['sync', 'derive', 'all'].includes(command)) {
    process.stderr.write(USAGE)
    process.exit(2)
  }

  const store = makeStore(options.store)
  let failures = 0

  if (command === 'sync' || command === 'all') {
    failures = await sync(store, options.sources, options.event)
  }
  if (command === 'derive' || command === 'all') {
    await derive(store)
  }
  if (failures > 0) {
    console.error(`${failures} source(s) failed`)
    process.exit(1)
  }
}

async function sync(store: Store, sources: string[], event: string): Promise<number> {
  let people: Person[] = []
  let failures = 0

  const run = async (name: string, fn: () => Promise<void>): Promise<void> => {
    try {
      await fn()
      await recordSync(store, name, true)
      console.log(`${name}: ok`)
    } catch (err) {
      failures++
      const note = err instanceof Error ? err.message : String(err)
      await recordSync(store, name, false, note)
      console.error(`${name}: FAILED - ${note}`)
    }
  }

  if (sources.includes('wikidata')) {
    await run('wikidata', async () => {
      people = await syncWikidata(store)
    })
  }
  if (sources.includes('aec')) {
    await run('aec', () => syncAec(store, event))
  }
  if (sources.includes('aph')) {
    await run('aph-bills', () => syncAphBills(store))
  }
  if (sources.includes('tvfy')) {
    if (process.env.TVFY_API_KEY) {
      if (people.length === 0) people = await loadPeople(store)
      await run('tvfy', () => syncTvfy(store, people))
    } else {
      console.log('tvfy: skipped (TVFY_API_KEY not set)')
    }
  }
  return failures
}

async function loadPeople(store: Store): Promise<Person[]> {
  const keys = await store.list('canonical/people/')
  const people: Person[] = []
  for (const key of keys) {
    const person = await store.getJson<Person>(key)
    if (person) people.push(person)
  }
  return people
}

function makeStore(kind: string): Store {
  if (kind === 's3') {
    const bucket = process.env.POLLYWIKI_DATA_BUCKET
    if (!bucket) throw new Error('POLLYWIKI_DATA_BUCKET must be set for --store s3')
    return new S3Store(bucket)
  }
  return new LocalStore(new URL('../../../.store', import.meta.url).pathname)
}

function parseOptions(args: string[]): { store: string; sources: string[]; event: string } {
  const get = (flag: string): string | undefined => {
    const i = args.indexOf(`--${flag}`)
    return i >= 0 ? args[i + 1] : undefined
  }
  return {
    store: get('store') ?? 'local',
    sources: (get('sources') ?? 'wikidata,aph,tvfy').split(',').map((s) => s.trim()),
    event: get('event') ?? '31496',
  }
}

main().catch((err) => {
  console.error(err)
  process.exit(1)
})
