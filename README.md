# pollywiki

[![ci](https://github.com/logan-han/pollywiki/actions/workflows/ci.yml/badge.svg)](https://github.com/logan-han/pollywiki/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/logan-han/pollywiki/branch/main/graph/badge.svg)](https://codecov.io/gh/logan-han/pollywiki)

The Australian federal record, unedited. A public register of federal
parliamentarians, division votes, bills and election results, generated
automatically from official sources with no editorial layer.

**This service does not evaluate politicians or laws.** No scores, no rankings,
no opinions. Official records plus arithmetic, always linked to the source.

## Architecture

```
Wikidata  TVFY API  APH bills + Handbook  AEC results + disclosures
Legislation Register  IPEA expenses  ABS boundaries
    └──────────┴──────────┴──────────┘
                  ▼
   [GitHub Actions: ingest, nightly]
                  ▼
   s3://data  raw/ → canonical/ → bundles/*.jsonl
                  ▼
   [GitHub Actions: deploy on push or after ingest]
   pollywiki-site build + pagefind → s3://site → CloudFront
```

Everything is Rust, one Cargo workspace:

- `crates/schema`: entity types, the single source of truth
- `crates/ingest`: source syncs, normalisation, derived bundles
- `crates/site`: static site generator; reads bundles, never computes
- `data/reference`: hand-curated party colours, AEC party names and parliament dates
- `data/sample`: fictional bundles so the site builds without credentials
- `infra`: Terraform for S3, CloudFront and the GitHub OIDC deploy roles
- `tools`: one-off asset generators (the default social card)

The site build also rasterises one 1200x630 share card per division under
`/og/divisions/`, drawn from the same tokens and vendored fonts as the pages.

## Develop

```sh
cargo run -p pollywiki-site -- --out dist --serve   # site on sample data
cargo test                                          # unit tests
cargo clippy --workspace --all-targets
cargo llvm-cov --workspace --codecov --output-path codecov.json   # coverage
python3 tools/og-default.py                         # redraw /og-default.png
```

Run a real ingest locally (no key needed for wikidata/aec):

```sh
cargo run -p pollywiki-ingest -- sync --sources wikidata,aec --event 31496   # writes .store/
cargo run -p pollywiki-ingest -- sync --sources aec --event all              # every event since 2004
cargo run -p pollywiki-ingest -- derive
BUNDLES_DIR=$PWD/.store/bundles cargo run -p pollywiki-site -- --out dist
```

They Vote For You divisions need `TVFY_API_KEY`
([sign up](https://theyvoteforyou.org.au/help/data), free for low-volume
non-commercial use; email the OpenAustralia Foundation before bulk backfills).

## Deploy

GitHub Actions assume scoped IAM roles via OIDC (no stored keys):

- `deploy.yml`: push to main → build from live bundles → S3 → CloudFront invalidation
- `ingest.yml`: nightly sync → bundles to S3 → dispatches deploy when data changed
- Repo variables: `SITE_BUCKET`, `DATA_BUCKET`, `CLOUDFRONT_DISTRIBUTION_ID`, `SITE_URL`
- Repo secrets: `AWS_DEPLOY_ROLE_ARN`, `AWS_INGEST_ROLE_ARN`, `TVFY_API_KEY`,
  `CODECOV_TOKEN`; optional `LANGFUSE_PUBLIC_KEY`, `LANGFUSE_SECRET_KEY`
  (and variable `LANGFUSE_BASE_URL` outside the EU cloud) for tracing

Infra lives in `infra/` (Terraform, state in S3). The CloudFront distribution
serves `pollywiki.au` and `www.pollywiki.au`; add further aliases to `domains`
once their ACM validation records are in place.

## Observability

With `LANGFUSE_PUBLIC_KEY` and `LANGFUSE_SECRET_KEY` set, every Gemini call
in `summarise` is traced to Langfuse as one generation: prompt, reply, tokens,
timing and failures. Traces are named by the job (`pollywiki.bill-notes`,
`pollywiki.division-context`, `pollywiki.member-note`) and grouped into one
session per workflow run. Spans go to Langfuse's v4 OpenTelemetry endpoint as
OTLP/HTTP JSON; without the keys nothing is sent. `LANGFUSE_RECORD_CONTENT=off`
keeps only the shape of each call. Read traces back through the Observations
API v2 (`GET /api/public/v2/observations`, scores through
`GET /api/public/v3/scores`): the v1 reads such as `/api/public/observations`
and `/api/public/traces` stop working when Langfuse Cloud moves to v4 on
16 November 2026, and until then each call shows as an action item on the
project's migration page.

## Data licences

- Voting data © [They Vote For You](https://theyvoteforyou.org.au), ODbL 1.0
- Election data and funding disclosures © Commonwealth of Australia (AEC), CC BY 4.0
- Acts sourced from the [Federal Register of Legislation](https://www.legislation.gov.au), CC BY 4.0
- Parliamentary expenses © Independent Parliamentary Expenses Authority, CC BY 3.0 AU
- Electorate boundaries © Commonwealth of Australia (ABS), CC BY 4.0
- Parliamentary material reproduced fairly and accurately with acknowledgement
- People data from Wikidata (CC0); photos from Wikimedia Commons, credited per page
