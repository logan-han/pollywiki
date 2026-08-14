import { mkdir, readFile, readdir, writeFile } from 'node:fs/promises'
import { dirname, join, relative } from 'node:path'
import type { Store } from './types.js'

export class LocalStore implements Store {
  constructor(private readonly root: string) {}

  private path(key: string): string {
    return join(this.root, key)
  }

  async putRaw(key: string, body: string | Uint8Array): Promise<void> {
    const file = this.path(key)
    await mkdir(dirname(file), { recursive: true })
    await writeFile(file, body)
  }

  async getRaw(key: string): Promise<string | null> {
    try {
      return await readFile(this.path(key), 'utf8')
    } catch (err) {
      if ((err as NodeJS.ErrnoException).code === 'ENOENT') return null
      throw err
    }
  }

  async putJson(key: string, value: unknown): Promise<void> {
    await this.putRaw(key, JSON.stringify(value, null, 1))
  }

  async getJson<T>(key: string): Promise<T | null> {
    const raw = await this.getRaw(key)
    return raw === null ? null : (JSON.parse(raw) as T)
  }

  async list(prefix: string): Promise<string[]> {
    const dir = this.path(prefix)
    const keys: string[] = []
    const walk = async (d: string): Promise<void> => {
      let entries
      try {
        entries = await readdir(d, { withFileTypes: true })
      } catch (err) {
        if ((err as NodeJS.ErrnoException).code === 'ENOENT') return
        throw err
      }
      for (const entry of entries) {
        const full = join(d, entry.name)
        if (entry.isDirectory()) await walk(full)
        else keys.push(join(prefix, relative(dir, full)))
      }
    }
    await walk(dir)
    return keys.sort()
  }
}
