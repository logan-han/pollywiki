// Copies build-time data artefacts that must also be served as static files.
import { copyFileSync, existsSync, mkdirSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const bundlesDir = process.env.BUNDLES_DIR
  ? resolve(process.env.BUNDLES_DIR)
  : resolve(here, '../../../data/sample/bundles')
const publicDir = resolve(here, '../public')

mkdirSync(publicDir, { recursive: true })
for (const file of ['quick-search.json']) {
  const src = join(bundlesDir, file)
  if (existsSync(src)) copyFileSync(src, join(publicDir, file))
  else console.warn(`prepare-public: ${src} missing, skipped`)
}
console.log(`prepare-public: bundles from ${bundlesDir}`)
