import type { Store } from './store/types.js'

const KEY = 'state/sync-manifest.json'

export interface SyncManifest {
  sources: Record<string, { lastSync: string; ok: boolean; note?: string }>
}

export async function readManifest(store: Store): Promise<SyncManifest> {
  return (await store.getJson<SyncManifest>(KEY)) ?? { sources: {} }
}

export async function recordSync(
  store: Store,
  source: string,
  ok: boolean,
  note?: string,
): Promise<void> {
  const manifest = await readManifest(store)
  manifest.sources[source] = {
    lastSync: new Date().toISOString(),
    ok,
    ...(note ? { note } : {}),
  }
  await store.putJson(KEY, manifest)
}
