const USER_AGENT = 'pollywiki/0.1 (https://pollywiki.han.life; contact: lhan@pay.com.au)'

const lastRequestAt = new Map<string, number>()

/** Polite fetch: identifying UA, per-host rate limit, retry with backoff on 429/5xx. */
export async function politeFetch(
  url: string,
  init: RequestInit & { minIntervalMs?: number; accept?: string } = {},
): Promise<Response> {
  const { minIntervalMs = 1000, accept, ...rest } = init
  const host = new URL(url).host

  for (let attempt = 1; ; attempt++) {
    const waitUntil = (lastRequestAt.get(host) ?? 0) + minIntervalMs
    const delay = waitUntil - Date.now()
    if (delay > 0) await sleep(delay)
    lastRequestAt.set(host, Date.now())

    const res = await fetch(url, {
      ...rest,
      headers: {
        'user-agent': USER_AGENT,
        ...(accept ? { accept } : {}),
        ...(rest.headers ?? {}),
      },
    })
    if (res.ok) return res
    const retryable = res.status === 429 || res.status >= 500
    if (!retryable || attempt >= 4) {
      throw new Error(`GET ${url} failed: ${res.status} ${res.statusText}`)
    }
    await sleep(attempt * 2000)
  }
}

export async function fetchJson<T>(url: string, init?: Parameters<typeof politeFetch>[1]): Promise<T> {
  const res = await politeFetch(url, { accept: 'application/json', ...init })
  return (await res.json()) as T
}

export async function fetchText(url: string, init?: Parameters<typeof politeFetch>[1]): Promise<string> {
  const res = await politeFetch(url, init)
  return await res.text()
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}
