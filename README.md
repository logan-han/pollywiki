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
Wikidata   TVFY API   APH bills   AEC CSVs
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
- `data/reference`: hand-curated party colours and parliament dates
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
  `CODECOV_TOKEN`

Infra lives in `infra/` (Terraform, state in S3). The CloudFront distribution
serves `pollywiki.au` and `www.pollywiki.au`; add further aliases to `domains`
once their ACM validation records are in place.

## Data licences

- Voting data © [They Vote For You](https://theyvoteforyou.org.au), ODbL 1.0
- Election data © Commonwealth of Australia (AEC), CC BY 4.0
- Parliamentary material reproduced fairly and accurately with acknowledgement
- People data from Wikidata (CC0); photos from Wikimedia Commons, credited per page
