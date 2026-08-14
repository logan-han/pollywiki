import {
  GetObjectCommand,
  ListObjectsV2Command,
  PutObjectCommand,
  S3Client,
} from '@aws-sdk/client-s3'
import type { Store } from './types.js'

export class S3Store implements Store {
  private readonly client: S3Client

  constructor(
    private readonly bucket: string,
    region = process.env.AWS_REGION ?? 'ap-southeast-2',
  ) {
    this.client = new S3Client({ region })
  }

  async putRaw(key: string, body: string | Uint8Array): Promise<void> {
    await this.client.send(
      new PutObjectCommand({
        Bucket: this.bucket,
        Key: key,
        Body: body,
        ContentType: contentTypeFor(key),
      }),
    )
  }

  async getRaw(key: string): Promise<string | null> {
    try {
      const res = await this.client.send(new GetObjectCommand({ Bucket: this.bucket, Key: key }))
      return (await res.Body?.transformToString()) ?? null
    } catch (err) {
      if ((err as { name?: string }).name === 'NoSuchKey') return null
      throw err
    }
  }

  async putJson(key: string, value: unknown): Promise<void> {
    await this.putRaw(key, JSON.stringify(value))
  }

  async getJson<T>(key: string): Promise<T | null> {
    const raw = await this.getRaw(key)
    return raw === null ? null : (JSON.parse(raw) as T)
  }

  async list(prefix: string): Promise<string[]> {
    const keys: string[] = []
    let token: string | undefined
    do {
      const res = await this.client.send(
        new ListObjectsV2Command({ Bucket: this.bucket, Prefix: prefix, ContinuationToken: token }),
      )
      for (const obj of res.Contents ?? []) if (obj.Key) keys.push(obj.Key)
      token = res.NextContinuationToken
    } while (token)
    return keys.sort()
  }
}

function contentTypeFor(key: string): string {
  if (key.endsWith('.json')) return 'application/json'
  if (key.endsWith('.jsonl')) return 'application/x-ndjson'
  if (key.endsWith('.csv')) return 'text/csv'
  if (key.endsWith('.xml')) return 'application/xml'
  return 'application/octet-stream'
}
