# WaterMelon Sync

The self-hostable synchronization server for [WaterMelon Browser](../README.md).
It mirrors profiles, proxies, groups, extensions and their metadata to an
S3-compatible bucket so they can be shared across devices. State lives entirely
in S3 — there is no database.

Built with [NestJS](https://nestjs.com/) and the AWS SDK v3.

## How it works

Clients never talk to S3 directly. They call this server, which issues
short-lived **presigned URLs** for uploads/downloads and exposes cheap
metadata/list/delete operations. A per-scope marker object
(`.watermelon-sync-manifest`) lets subscribers detect changes with a single
`HeadObject` per poll instead of repeated `ListObjectsV2` calls. The target
bucket is created automatically on startup (`ensureBucketExists`).

Authentication runs in one of two modes:

- **Self-hosted** — a shared bearer token (`SYNC_TOKEN`). No per-user scoping.
- **Cloud** — RS256 JWTs verified against `SYNC_JWT_PUBLIC_KEY`, scoped per
  user/team with optional profile quotas.

At least one of `SYNC_TOKEN` / `SYNC_JWT_PUBLIC_KEY` must be set or the server
refuses to boot.

## Environment variables

| Variable | Required | Default | Description |
|---|---|---|---|
| `SYNC_TOKEN` | Yes\* | – | Bearer token for self-hosted auth |
| `SYNC_JWT_PUBLIC_KEY` | Yes\* | – | RS256 public key (PEM) for cloud JWT auth |
| `PORT` | No | `3929` | HTTP port |
| `S3_ENDPOINT` | No | `http://localhost:8987` | S3-compatible endpoint |
| `S3_REGION` | No | `us-east-1` | S3 region |
| `S3_ACCESS_KEY_ID` | Yes | `minioadmin` | S3 access key |
| `S3_SECRET_ACCESS_KEY` | Yes | `minioadmin` | S3 secret key |
| `S3_BUCKET` | No | `watermelon-sync` | Bucket for sync data |
| `S3_FORCE_PATH_STYLE` | No | `true` | Path-style URLs (MinIO). Set `false` for AWS S3 |
| `BACKEND_INTERNAL_URL` | No | – | Optional backend URL for profile-usage reporting (cloud) |
| `BACKEND_INTERNAL_KEY` | No | – | Internal key sent with backend reporting (cloud) |
| `INTERNAL_KEY` | No | – | Key guarding `POST /v1/internal/cleanup-excess-profiles` |

\* Provide either `SYNC_TOKEN` or `SYNC_JWT_PUBLIC_KEY`.

See `.env.example` for a ready-to-edit template.

## API

All `/v1/objects/*` routes require `Authorization: Bearer <token>`.

| Method & path | Purpose |
|---|---|
| `POST /v1/objects/stat` | Existence + size/metadata of one key |
| `POST /v1/objects/presign-upload` | Presigned PUT URL |
| `POST /v1/objects/presign-download` | Presigned GET URL |
| `POST /v1/objects/presign-upload-batch` | Presigned PUT URLs (batch) |
| `POST /v1/objects/presign-download-batch` | Presigned GET URLs (batch) |
| `POST /v1/objects/delete` | Delete a key (+ optional tombstone) |
| `POST /v1/objects/delete-prefix` | Delete every key under a prefix |
| `POST /v1/objects/list` | List keys under a prefix (paginated) |
| `GET /v1/objects/subscribe` | SSE stream of change events |

Unauthenticated health endpoints:

| Method & path | Purpose |
|---|---|
| `GET /health` | Liveness — `{"status":"ok"}` |
| `GET /readyz` | Readiness — checks S3, returns `{"status":"ready","s3":true}` or HTTP 503 |

## Development

```bash
npm install
cp .env.example .env          # edit SYNC_TOKEN / S3_* as needed
docker compose up -d minio    # local S3 on http://localhost:8987
npm run start:dev             # watch mode
```

## Testing

```bash
npm test                      # unit tests (no S3 needed)
docker compose up -d minio    # required for the e2e suite
npm run test:e2e              # spins the app against MinIO at 127.0.0.1:8987
```

The e2e suite (`test/sync.e2e-spec.ts`) exercises the full presigned
upload → stat → download → delete cycle against real S3.

## Running the whole stack

```bash
docker compose up             # builds the server + starts MinIO
curl localhost:12342/health   # {"status":"ok"}
curl localhost:12342/readyz   # {"status":"ready","s3":true}
```

## Self-hosting in production

See the [Self-Hosting Guide](../docs/self-hosting-watermelon-sync.md) for
deployment with external S3 (AWS, Cloudflare R2, …), TLS via a reverse proxy,
and security hardening.
