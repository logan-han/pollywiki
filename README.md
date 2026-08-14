# pollywiki

The Australian federal record, unedited. A public register of federal
parliamentarians, division votes, bills and election results, generated
automatically from official sources with no editorial layer. Modelled on
[POLI WIKI](https://poliwiki.kr).

**This service does not evaluate politicians or laws.** No scores, no rankings,
no opinions. Official records plus arithmetic, always linked to the source.

## Architecture

```
Wikidata   TVFY API   APH bills   AEC CSVs
    └──────────┴──────────┴──────────┘
                  ▼
   [GitHub Actions: ingest, nightly]
                  ▼
   s3://data  raw/ → canonical/ → bundles/*.jsonl
                  ▼
   [GitHub Actions: deploy on push or after ingest]
   astro build + pagefind → s3://site → CloudFront
```

- `packages/schema`: zod entity schemas, the single source of truth
- `packages/ingest`: source syncs, normalisation, derived bundles
- `packages/site`: Astro static site; reads bundles, never computes
- `data/reference`: hand-curated party colours and parliament dates
- `data/sample`: fictional bundles so the site builds without credentials
- `infra`: Terraform for S3, CloudFront and the GitHub OIDC deploy roles

## Develop

```sh
pnpm install
pnpm dev                 # site on sample data
pnpm test                # ingest unit tests
pnpm typecheck
```

Run a real ingest locally (no key needed for wikidata/aec):

```sh
pnpm ingest sync --sources wikidata,aec --event 31496   # writes .store/
pnpm ingest derive
BUNDLES_DIR=$PWD/.store/bundles pnpm build
```

They Vote For You divisions need `TVFY_API_KEY`
([sign up](https://theyvoteforyou.org.au/help/data), free for low-volume
non-commercial use; email the OpenAustralia Foundation before bulk backfills).

## Deploy

GitHub Actions assume scoped IAM roles via OIDC (no stored keys):

- `deploy.yml`: push to main → build from live bundles → S3 → CloudFront invalidation
- `ingest.yml`: nightly sync → bundles to S3 → dispatches deploy when data changed
- Repo variables: `SITE_BUCKET`, `DATA_BUCKET`, `CLOUDFRONT_DISTRIBUTION_ID`, `SITE_URL`
- Repo secrets: `AWS_DEPLOY_ROLE_ARN`, `AWS_INGEST_ROLE_ARN`, `TVFY_API_KEY`

Infra lives in `infra/` (Terraform, state in S3). The CloudFront distribution
supports multiple aliases; `pollywiki.han.life` first, `pollywiki.au` later by
adding to `domains` and flipping `enable_custom_domain` once the ACM validation
records are in place.

## Data licences

- Voting data © [They Vote For You](https://theyvoteforyou.org.au), ODbL 1.0
- Election data © Commonwealth of Australia (AEC), CC BY 4.0
- Parliamentary material reproduced fairly and accurately with acknowledgement
- People data from Wikidata (CC0); photos from Wikimedia Commons, credited per page
