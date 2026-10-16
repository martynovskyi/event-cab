# event-cab

[![CI](https://github.com/martynovskyi/event-cab/actions/workflows/ci.yml/badge.svg)](https://github.com/martynovskyi/event-cab/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/martynovskyi/event-cab?sort=semver)](https://github.com/martynovskyi/event-cab/releases)
[![Image](https://img.shields.io/badge/image-ghcr.io-blue?logo=docker)](https://github.com/martynovskyi/event-cab/pkgs/container/event-cab)
![Rust](https://img.shields.io/badge/rust-1.88%2B-orange?logo=rust)

A small, self-hosted events service for static websites: reactions (👍 🔥 ❤️), feedback forms, and other visitor events, tied to pages or resources. No visitor accounts. Built with Rust, axum, and PostgreSQL.

- **Append-only event log.** A topic's *mode* is applied when reading, so changing it in the CMS reinterprets the whole history without a data migration.
- **Topics create themselves.** Unknown topics start as `pending` and stay hidden until a moderator approves them through the admin API.
- **Abuse limits.** Per-IP and per-topic rate limits, a honeypot field, caps on new topics, and only HMAC-hashed IPs are stored.
- **Two ports.** The public API is exposed through nginx; the admin API stays inside the Docker network and requires a bearer token.
- **OpenAPI 3.1 is generated from the code** and committed, so clients can generate their types from it.

> The full design, including validation rules, SQL, and security notes, is in [initial-spec.md](initial-spec.md).
,.
## How it works

```mermaid
flowchart LR
    Browser -- "POST /api/events/{topic}<br/>GET .../summary" --> nginx
    nginx -- ":3001 public" --> events[(events service)]
    CMS[CMS backend] -- ":3002 admin<br/>Bearer token" --> events
    events --> PG[(PostgreSQL)]
```

| Mode | How events are counted | Typical use |
|---|---|---|
| `append` | Every event counts | Feedback forms (write-only; at most a public total) |
| `toggle` | Only each visitor's latest event per key counts, and only if it's still active | Reactions |

For `toggle` topics, the frontend sends the reaction's current **state**, not a click, so repeating a request is harmless:

```json
{ "type": "like", "active": true }
```

## Quick start

You need [Rust](https://rustup.rs) 1.88+, Docker, and PowerShell 7.

```powershell
./scripts/dev.ps1          # starts a disposable Postgres container and runs the service
```

| Command | Description |
|---|---|
| `./scripts/dev.ps1` | Run the service on `:3001` (public) and `:3002` (admin) |
| `./scripts/dev.ps1 test` | Run the tests against the local Postgres |
| `./scripts/dev.ps1 check` | Run what CI runs: `fmt`, `clippy -D warnings`, tests |
| `./scripts/dev.ps1 openapi` | Regenerate `openapi/*.json` |
| `./scripts/dev.ps1 db` / `psql` / `stop` | Start the database / open a psql shell / remove the container |

The script sets development defaults for the required settings. Any environment variable you've already set takes priority.

Try it:

```bash
# Send a reaction (the topic is auto-created as pending)
curl -X POST localhost:3001/api/events/reactions \
  -H 'content-type: application/json' -H 'x-real-ip: 127.0.0.1' \
  -d '{"resourceUrl":"/blog/hello","visitorId":"v1","data":{"type":"like","active":true}}'

# Approve it as a public toggle topic
curl -X PATCH localhost:3002/api/admin/topics/reactions \
  -H 'authorization: Bearer dev-admin-token-0123456789abcdef' \
  -H 'content-type: application/json' \
  -d '{"status":"approved","public":true,"mode":"toggle","keyField":"type"}'

# Read the summary
curl 'localhost:3001/api/events/reactions/summary?resourceUrl=/blog/hello&visitorId=v1'
# {"mode":"toggle","counts":{"like":1},"mine":["like"]}
```

## API

The full schemas are in [`openapi/public.json`](openapi/public.json) and [`openapi/admin.json`](openapi/admin.json).

Ready-to-run requests for every endpoint are in [`http/`](http/). They work with the VS Code [REST Client](https://marketplace.visualstudio.com/items?itemName=humao.rest-client) extension and with the JetBrains HTTP Client (pick the `local` environment), and they target the server started by `./scripts/dev.ps1`.

### Public (`:3001`)

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/events/{topic}` | Record an event. Returns `200` with the summary if the topic is public, otherwise `204` |
| `GET` | `/api/events/{topic}/summary?resourceId=&resourceUrl=&visitorId=` | Counts for a resource; only for topics that are approved and public |
| `GET` | `/health` | `200` if Postgres responds, otherwise `503` (not proxied by nginx) |

### Admin (`:3002`, `Authorization: Bearer <ADMIN_TOKEN>`)

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/admin/topics?status=` | List topics with event counts |
| `PATCH` | `/api/admin/topics/{name}` | Set `status`, `mode`, `keyField`, `public` |
| `DELETE` | `/api/admin/topics/{name}` | Delete a topic and all its events |
| `GET` | `/api/admin/topics/{name}/sample?limit=` | Latest events, for moderation |
| `GET` | `/api/admin/events?topic=&resourceId=&resourceUrl=&after=&limit=` | Feed of events, paged by `id` |
| `DELETE` | `/api/admin/events?visitorId=` | Delete all of one visitor's events (data-erasure requests) |
| `GET` | `/api/admin/openapi/{public,admin}.json` | The OpenAPI documents |

## Configuration

All settings come from environment variables; see [`.env.example`](.env.example).

| Variable | Default | Description |
|---|---|---|
| `DATABASE_URL` | **required** | Postgres connection string |
| `ADMIN_TOKEN` | **required** | Bearer token for the admin API, at least 32 characters |
| `IP_HASH_SALT` | **required** | HMAC key used to hash client IPs |
| `ALLOWED_HOSTS` | *(empty)* | Comma-separated site domains accepted in absolute `resourceUrl`s. If empty, only relative URLs are accepted |
| `PUBLIC_PORT` / `ADMIN_PORT` | `3001` / `3002` | Listener ports |
| `EVENTS_PER_IP_MIN` | `60` | Events per minute per client, across all topics |
| `EVENTS_PER_MIN` | `20` | Events per minute per client, per topic |
| `NEW_TOPICS_PER_IP_HOUR` | `3` | New topics a client may create per hour |
| `MAX_PENDING_TOPICS` | `100` | No new topics are created once this many are pending |
| `TOGGLE_TOP_N` | `20` | Maximum number of keys in a toggle summary |
| `RUST_LOG` | `info` | Log filter (logs are JSON) |

Generate the secrets with `openssl rand -hex 32`.

## Deployment

Each release publishes a Docker image to GHCR:

```bash
docker pull ghcr.io/martynovskyi/event-cab:latest   # or :0.1 / :0.1.0
```

[`docker-compose.yml`](docker-compose.yml) attaches the service to three existing Docker networks: Postgres, the CMS, and nginx. No ports are published on the host. Migrations run automatically at startup. One-time database setup:

```sql
CREATE ROLE events_svc LOGIN PASSWORD '...';
CREATE DATABASE events OWNER events_svc;
```

nginx should proxy only the public API and must pass the client IP:

```nginx
location /api/events/ {
    proxy_pass http://events:3001;
    proxy_set_header X-Real-IP $remote_addr;
}
```

> [!IMPORTANT]
> The service reads the client IP only from `X-Real-IP`. After deploying, check that nginx sees real client addresses and not the Docker bridge gateway. Otherwise every visitor shares one rate-limit bucket. See [spec §10](initial-spec.md#10-security).

## Development

```
src/
  main.rs         # entry point: serve | healthcheck | openapi <dir>
  config.rs       # env → Config
  routes/         # public.rs, admin.rs
  resource.rs     # URL normalization, resource keys
  topics.rs       # topic auto-creation and limits
  summary.rs      # append / toggle read-time queries
  rate_limit.rs   # in-memory rate limiter
  openapi.rs      # OpenAPI documents
migrations/       # SQL migrations (run on startup)
openapi/          # generated OpenAPI documents (committed)
tests/            # integration tests (each test gets a fresh database)
```

- Integration tests need `DATABASE_URL` pointing at a Postgres server where the user can create databases. `./scripts/dev.ps1 test` sets this up for you.
- If you change an endpoint or schema, run `./scripts/dev.ps1 openapi` and commit the result. A test fails if the committed documents are out of date.

## Releasing

```powershell
./scripts/release.ps1 0.2.0
```

The script checks that you're on a clean, up-to-date `main`, bumps the version in `Cargo.toml`, runs the CI checks, then commits and tags `v0.2.0`. It asks before pushing. The pushed tag starts the [release workflow](.github/workflows/release.yml), which re-runs CI, publishes the image to GHCR, and creates a GitHub release.

Options: `-SkipChecks`, `-NoPush`, `-Yes` (push without asking).
