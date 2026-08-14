/**
 * Single seam for all persistence. The S3 implementation is canonical;
 * the local one mirrors the same key layout under .store/ for development.
 * A future runtime database implements this same interface.
 */
export interface Store {
  putRaw(key: string, body: string | Uint8Array): Promise<void>
  getRaw(key: string): Promise<string | null>
  putJson(key: string, value: unknown): Promise<void>
  getJson<T>(key: string): Promise<T | null>
  list(prefix: string): Promise<string[]>
}
