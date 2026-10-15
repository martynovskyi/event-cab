# Events Service — Specification v1

A self-hosted service for collecting events (reactions, feedback, etc.) tied to resources of a static website (Astro + React islands + Tailwind). Visitors don't register. Topics are created automatically from incoming data and moderated manually in the CMS via the admin API.

## 1. Stack (fixed)

| Layer | Choice |
|---|---|
| Language | Rust (stable) |
| HTTP | axum 0.8 + tokio |
| Database | PostgreSQL (already running in Docker), dedicated `events` database and `events_svc` role |
| Driver | sqlx (`runtime-tokio`, `postgres`, `json`, `migrate`, `chrono`), connection pool |
| Migrations | `sqlx migrate`, SQL files in `migrations/` |
| Data | `JSONB` |
| Serialization | serde + serde_json |
| URLs | `url` crate |
| IP hashing | `sha2` crate |
| Token comparison | `subtle` crate (constant-time) |
| Logging | tracing + tracing-subscriber (JSON) |
| API docs | `utoipa` + `utoipa-axum` (OpenAPI 3.1, generated from code, see §9) |
| Rate limiting | custom, in-memory (no external crates) |
| Deployment | Docker multi-stage → `debian:bookworm-slim`, behind nginx on the same domain |

Deferred: Cloudflare Turnstile, counter cache table, ratings (`upsert` mode with average/distribution), public listing and per-event moderation of feedback.

## 2. Architecture

- One binary, **two ports**:
  - `:3001` — public API (`/api/events/...`, `/health`), proxied by nginx (running in Docker, reaches the service as `http://events:3001`).
  - `:3002` — admin API (`/api/admin/...`), **never published externally**, reachable only inside the CMS Docker network. Additionally protected by `Authorization: Bearer <ADMIN_TOKEN>`.
- The CMS calls the admin API **from its backend only** (`http://events:3002`).
- The service is stateless (apart from the in-memory rate-limit map); all persistence lives in PG.
- **Append-only event log.** A topic's mode (append / toggle) is an interpretation rule applied at read time. Changing the mode in the CMS immediately reinterprets the whole history, with no data migration.

## 3. Data model

```sql
-- migrations/0001_init.sql
CREATE TABLE topics (
  name        TEXT PRIMARY KEY,
  status      TEXT NOT NULL DEFAULT 'pending'
              CHECK (status IN ('pending', 'approved', 'blocked')),
  mode        TEXT NOT NULL DEFAULT 'append'
              CHECK (mode IN ('append', 'toggle')),
  key_field   TEXT,                       -- for toggle: field name in data (e.g. 'type')
  public      BOOLEAN NOT NULL DEFAULT false,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  CONSTRAINT toggle_needs_key CHECK (mode <> 'toggle' OR key_field IS NOT NULL)
);

CREATE TABLE events (
  id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  topic        TEXT NOT NULL REFERENCES topics(name),
  resource_id  TEXT,
  resource_url TEXT,
  resource_key TEXT GENERATED ALWAYS AS (
                 COALESCE('id:' || resource_id, 'url:' || resource_url)
               ) STORED NOT NULL,
  visitor_id   TEXT NOT NULL,
  data         JSONB NOT NULL DEFAULT '{}',
  ip_hash      TEXT,
  created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  CONSTRAINT resource_present CHECK (resource_id IS NOT NULL OR resource_url IS NOT NULL)
);

CREATE INDEX idx_events_key   ON events (topic, resource_key, visitor_id, id DESC);
CREATE INDEX idx_events_topic ON events (topic, id);
CREATE INDEX idx_events_url   ON events (resource_url) WHERE resource_url IS NOT NULL;
```

## 4. Topics

### Lifecycle
1. An event with an unknown topic → a `topics` row is created (`pending`, `append`, `public=false`). The event **is stored**.
2. A moderator in the CMS sets `approved` or `blocked`, the mode, `key_field`, and `public`.
3. `blocked` → new events are silently dropped (`204`).
4. The public summary is available only for `approved` + `public`.

### Topic name
- Regex: `^[a-z0-9][a-z0-9_.-]{0,49}$`, otherwise `400`.

### Garbage protection
- At most `NEW_TOPICS_PER_IP_HOUR` (default 3) new topics per hour per IP. Over the limit the event is dropped (`204`) with a `warn` log.
- Cap of `MAX_PENDING_TOPICS` (default 100). Once that many topics are `pending`, no new ones are created (`204` + `warn`).
- Topic creation: `INSERT ... ON CONFLICT (name) DO NOTHING` (race-safe).

## 5. Modes (read-time interpretation)

| Mode | Rule | Typical use |
|---|---|---|
| `append` | all events | one-way feedback forms (write-only; publicly at most a total count) |
| `toggle` | latest event per (`visitor_id`, `data->>key_field`); counted if `data.active != false` (missing `active` = `true`) | reactions |

Events of a toggle topic whose `data[key_field]` is missing or not a JSON string are ignored when counting (the mode may be set after events were stored, so this is enforced in the query, not only on ingest).

For toggle, the frontend sends **state**, not a "click": `{ "type": "like", "active": true|false }`. This is idempotent.

Key values are not restricted to a list. To bound the response, the summary returns only the `TOGGLE_TOP_N` (default 20) most frequent keys; ties are broken by key ascending.

```sql
-- toggle ($1 topic, $2 resource_key, $3 key_field, $4 top_n)
SELECT k, COUNT(*) AS n FROM (
  SELECT DISTINCT ON (visitor_id, data->>$3)
         data->>$3 AS k,
         COALESCE((data->>'active')::bool, true) AS active
  FROM events
  WHERE topic = $1 AND resource_key = $2 AND jsonb_typeof(data->$3) = 'string'
  ORDER BY visitor_id, data->>$3, id DESC
) s WHERE active GROUP BY k
ORDER BY n DESC, k
LIMIT $4;

-- append
SELECT COUNT(*) FROM events WHERE topic = $1 AND resource_key = $2;
```

`(data->>'active')::bool` is safe because `active` is validated as a boolean on ingest for every topic (§7).

## 6. Resource

- A request must contain at least one of `resourceId` / `resourceUrl`, otherwise `400`.
- `resourceId`: trimmed, 1..200 chars, no control characters.
- `resourceUrl`: relative (`/blog/x`) or absolute URL. Normalization:
  - `http`/`https` only;
  - host lowercased, `www.` stripped; host must be in `ALLOWED_HOSTS`, otherwise `400` (relative URLs always pass; `//evil.com` is rejected). `ALLOWED_HOSTS` entries are normalized the same way (lowercased, `www.` stripped) when the config is loaded;
  - port is ignored;
  - strip `?query`, `#hash`, and the trailing `/` (except root);
  - only the **path** is stored. The `url` crate percent-encodes raw characters (`/блог` → `/%D0%B1...`) but does **not** normalize existing escapes (`%d0` vs `%D0`, `%41` vs `A`), so the path is canonicalized afterwards: escapes of unreserved characters (`A-Z a-z 0-9 - . _ ~`) are decoded, all other escapes are upper-cased;
  - max 2048 bytes of input, and max **1024 bytes** of normalized path (encoding can triple the length, and `resource_key` is in a btree index with a ~2.7 KB entry limit), otherwise `400`.
- The host is not part of the key: with several `ALLOWED_HOSTS`, `site.com/x` and `blog.site.com/x` aggregate together. Use `resourceId` or separate topics if hosts must be kept apart.
- Aggregation key (`resource_key`, generated column): `id:<resource_id>`, otherwise `url:<path>`.
- Frontend guideline: within a topic, always send the resource the same way (preferably both fields).

```rust
const MAX_PATH: usize = 1024;

/// `allowed_hosts` must already be lowercased with `www.` stripped (done in config.rs).
fn normalize_url(raw: &str, allowed_hosts: &[String]) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 2048 { return None; }
    const LOCAL: &str = "local.invalid";
    let base = url::Url::parse(&format!("https://{LOCAL}")).unwrap();
    let u = base.join(raw).ok()?;
    if !matches!(u.scheme(), "http" | "https") { return None; }
    let host = u.host_str()?.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    if host != LOCAL && !allowed_hosts.iter().any(|h| h == host) { return None; }
    let mut path = canonical_escapes(u.path());
    if path.len() > 1 && path.ends_with('/') { path.pop(); }
    (path.len() <= MAX_PATH).then_some(path)
}

/// Decode escapes of unreserved chars, upper-case all other escapes.
fn canonical_escapes(p: &str) -> String {
    let b = p.as_bytes();
    let mut out = String::with_capacity(p.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len()
            && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit()
        {
            let v = u8::from_str_radix(&p[i + 1..i + 3], 16).unwrap();
            if v.is_ascii_alphanumeric() || b"-._~".contains(&v) {
                out.push(v as char);
            } else {
                out.push_str(&format!("%{v:02X}"));
            }
            i += 3;
            continue;
        }
        out.push(b[i] as char); // url::Url::path() is pure ASCII
        i += 1;
    }
    out
}
```

## 7. Public API (`:3001`)

### `POST /api/events/{topic}`
Body (≤ 8 KB via `DefaultBodyLimit`):
```json
{
  "resourceId": "post:123",
  "resourceUrl": "https://site.com/blog/post-1/?utm=x",
  "visitorId": "uuid",
  "data": { "type": "like", "active": true },
  "website": ""
}
```
Processing order (first match wins):
1. Client IP from `X-Real-IP`; missing or unparsable → `400` (only happens when nginx is bypassed).
2. Rate limit `EVENTS_PER_MIN` (default 20) per (topic, IP) → `429`.
3. `website` non-empty (honeypot) → `204`, store nothing.
4. Topic name invalid (§4) → `400`.
5. Body validation → `400` / `413`:
   - `visitorId`: 1..64 chars, no control characters.
   - `data`: JSON object (missing → `{}`), serialized ≤ 4 KB, otherwise `413`. If `active` is present it must be a boolean (all topics).
   - If the topic is `toggle` and `data[key_field]` is present, it must be a string of 1..64 chars.
   - Resource: see section 6.
6. Topic `blocked` → `204`, nothing stored.
7. Unknown topic → auto-creation with the limits in §4 (over a limit → `204`, nothing stored).
8. Store the event.

Responses:
- topic `approved` + `public` → `200` + summary (same as GET below, for this `visitorId`);
- otherwise (pending, private) → `204`.

### `GET /api/events/{topic}/summary?resourceId=&resourceUrl=&visitorId=`
- Only for `approved` + `public`, otherwise `404`.
- The resource key is computed with the same rule as on ingest. `visitorId` is optional (without it, `mine` is `[]`).
- Rate limited with the same `EVENTS_PER_MIN` budget per IP (separate bucket from ingest) → `429`.
- Response by mode:
  - append → `{ "mode": "append", "total": 12 }`
  - toggle → `{ "mode": "toggle", "counts": { "like": 12, "fire": 3 }, "mine": ["like"] }` (`counts` holds at most `TOGGLE_TOP_N` keys; `mine` is not truncated)

### `GET /health`
`200` if PG responds (`SELECT 1`), otherwise `503`. Not proxied by nginx. The runtime image has no `curl`, so the Docker healthcheck uses the binary itself (`events healthcheck` subcommand → GET `127.0.0.1:3001/health`, exit 0/1).

## 8. Admin API (`:3002`, Bearer token)

All routes sit behind a `require_admin_token` middleware (comparison via `subtle::ConstantTimeEq`).

- `GET /api/admin/topics` → list: `name, status, mode, keyField, public, eventCount, lastEventAt, createdAt, updatedAt`. Filter: `?status=`.
- `PATCH /api/admin/topics/{name}` → `{ status?, mode?, keyField?, public? }`. An absent field is left unchanged; `keyField: null` clears it (deserialize as `Option<Option<String>>`). `keyField` must match `^[A-Za-z0-9_]{1,50}$`. The resulting row is validated before the update: `mode = toggle` with no `keyField` → `400` (never let the DB `CHECK` surface as `500`). Bumps `updated_at`. `404` if the topic doesn't exist.
- `GET /api/admin/topics/{name}/sample?limit=10` → latest events of the topic (for moderation), `limit` 1..100.
- `GET /api/admin/events?topic=&resourceId=&resourceUrl=&after=&limit=` → cursor-based feed by `id` ascending (`id > after`, `limit` 1..1000, default 100). Filters combine with AND on the respective columns; the `resourceUrl` filter is normalized with the same rule. Response: `{ items: [...], nextAfter: <last id> | null }`, `nextAfter` is `null` when fewer than `limit` items were returned.

Event item shape (sample and feed): `{ id, topic, resourceId, resourceUrl, visitorId, data, createdAt }`.

`ip_hash` is never exposed, not even via the admin API.

## 9. OpenAPI

Both APIs are described by an **OpenAPI 3.1** document, the single source of truth for clients (Astro TS client, CMS backend).

- **Generated from code**, not hand-written, so it can't drift: `utoipa` (`#[utoipa::path]` on handlers, `#[derive(ToSchema)]` on request/response types) + `utoipa-axum` for registering routes together with their docs.
- Two documents, matching the two ports:
  - `openapi/public.json` — `/api/events/...` (no `/health`);
  - `openapi/admin.json` — `/api/admin/...`, with a `bearer` security scheme.
- Both files are **committed** to the repo. `events openapi <dir>` (binary subcommand) writes them; an integration test regenerates them in memory and fails if the committed files differ ("run `events openapi openapi/`").
- Served only on the admin port: `GET /api/admin/openapi/public.json` and `GET /api/admin/openapi/admin.json` (behind the token). Nothing is served publicly, and no Swagger UI in v1.
- The document must cover: all status codes per route (incl. `204`, `400`, `404`, `413`, `429`), the toggle/append summary as a `oneOf` discriminated by `mode`, `data` as a free-form object, and the validation limits (`maxLength`, `minimum`/`maximum`) from §6–§8.
- The Astro project generates its types from `openapi/public.json` (`openapi-typescript`) instead of declaring them by hand.

## 10. Security

- `ip_hash = hex(sha256(IP_HASH_SALT + ip))`. Raw IPs are never stored.
- The IP is taken from `X-Real-IP` (set by nginx); no port of the service is published on the host.
- nginx runs in Docker, so `$remote_addr` is the real client IP only if Docker preserves the source address. This holds for IPv4 with iptables DNAT, but traffic going through `docker-proxy` (e.g. IPv6 clients to an IPv4-only network) shows up as the bridge gateway IP, putting everyone in one rate-limit bucket. **Verify on deploy** (log `X-Real-IP` for a few requests); if it shows the gateway, enable IPv6 on the nginx network or use `network_mode: host` for nginx.
- If a CDN is added in front of nginx later, switch to `real_ip_header` with the CDN's ranges.
- Accepted risk: `visitorId` is client-generated, so one person can inflate counts by rotating it, bounded only by the per-IP rate limit.
- Accepted risk: the `NEW_TOPICS_PER_IP_HOUR` and `MAX_PENDING_TOPICS` checks are check-then-insert and may be slightly exceeded under concurrency.
- The rate-limit map is pruned periodically (tokio task every 10 min).
- Text from `data` is never rendered as HTML by the service. If the CMS displays it, the CMS must escape it.

## 11. Configuration (env)

| Variable | Description | Default |
|---|---|---|
| `DATABASE_URL` | `postgres://events_svc:...@postgres:5432/events` | — (required) |
| `ADMIN_TOKEN` | Bearer token for the admin API | — (required) |
| `IP_HASH_SALT` | salt for `ip_hash` | — (required) |
| `ALLOWED_HOSTS` | comma-separated site domains (`site.com,blog.site.com`); trimmed, lowercased, `www.` stripped on load | empty = relative URLs only |
| `PUBLIC_PORT` | | `3001` |
| `ADMIN_PORT` | | `3002` |
| `EVENTS_PER_MIN` | event limit per (topic, IP) | `20` |
| `NEW_TOPICS_PER_IP_HOUR` | | `3` |
| `MAX_PENDING_TOPICS` | | `100` |
| `TOGGLE_TOP_N` | max keys in a toggle summary | `20` |
| `RUST_LOG` | | `info` |

Config is read and validated once at startup; on error the process exits with a clear message.

## 12. Deployment

- PG runs in Docker. One-time setup:
  ```sql
  CREATE ROLE events_svc LOGIN PASSWORD '...';
  CREATE DATABASE events OWNER events_svc;
  ```
- Migrations run on service startup (`sqlx::migrate!()`).
- Dockerfile: multi-stage (`rust:1-bookworm` → `debian:bookworm-slim`), `ca-certificates`, non-root user.
- docker-compose:
  ```yaml
  services:
    events:
      build: .
      restart: unless-stopped
      env_file: .env
      # no ports published: nginx reaches 3001 over the `web` network, the CMS reaches 3002 over `cms`
      networks: [pg, cms, web]
      healthcheck:
        test: ["CMD", "/app/events", "healthcheck"]
        interval: 30s
  networks:
    pg:  { external: true, name: <PG network> }
    cms: { external: true, name: <CMS network> }
    web: { external: true, name: <nginx network> }
  ```
  Port `3002` is reachable from every container on these networks (not only the CMS); the bearer token is what protects it there.
- nginx (container on the `web` network):
  ```nginx
  location /api/events/ {
      proxy_pass http://events:3001;
      proxy_set_header X-Real-IP $remote_addr;
  }
  ```
  `/api/admin/` and `/health` are **not** proxied through nginx.
- sqlx: use runtime-checked queries (`sqlx::query_as`), or commit `.sqlx/` (`cargo sqlx prepare`) and build with `SQLX_OFFLINE=true`, since there is no DB during `docker build`.

## 13. Project structure (proposed)

```
events/
  Cargo.toml
  Dockerfile
  docker-compose.yml
  .env.example
  migrations/0001_init.sql
  openapi/           # public.json, admin.json (generated, committed)
  src/
    main.rs          # config, pool, migrations, two listeners
    config.rs        # env → Config
    state.rs         # AppState (PgPool, rate limiter, config)
    rate_limit.rs
    resource.rs      # normalize_url, resourceId validation, resource_key
    topics.rs        # auto-creation, limits, status cache (optional)
    routes/
      public.rs      # POST event, GET summary, health
      admin.rs       # topics, sample, feed
    error.rs         # AppError → IntoResponse
    openapi.rs       # ApiDoc for public + admin, `openapi` subcommand
  tests/             # integration tests against a test PG
```

## 14. Implementation order

1. Skeleton: Cargo, config, tracing, pool, migrations, `/health`, two listeners.
2. Event ingest: validation, resource, honeypot, rate limit, topic auto-creation with limits.
3. Summary per mode.
4. Admin API + token middleware.
5. OpenAPI: annotations on all handlers/types written in steps 2–4 (annotate as you go, finalize here), `openapi` subcommand, committed files, drift test.
6. Dockerfile, compose, `.env.example`.
7. Tests: URL normalization incl. percent-encoding canonicalization and the 1024-byte cap (unit); ingest / toggle / top-N / blocked / pending cap / PATCH validation (integration).
8. (Separately, in the Astro project) TS client `src/lib/events.ts` (types generated from `openapi/public.json`) and a React reactions component: `visitorId` in localStorage, honeypot field in forms.

## 15. v1 acceptance criteria

- `cargo clippy -- -D warnings` and `cargo test` pass.
- An unknown topic is created as `pending`, its events are stored, and its summary returns `404`.
- After `PATCH` to `approved` + `toggle` + `keyField=type` + `public`, the summary correctly counts historical events.
- A `blocked` topic accepts no new events.
- The admin API is unreachable via the public port and nginx.
- Repeating a toggle request with the same state doesn't change the count.
- Historical toggle events whose key value is `null` or not a string are not counted, and the summary doesn't fail.
- `/блог`, `/%d0%b1%d0%bb%d0%be%d0%b3` and `/%D0%B1%D0%BB%D0%BE%D0%B3/` map to the same `resource_key`.
- A URL whose normalized path exceeds 1024 bytes returns `400`, not `500`.
- `openapi/public.json` and `openapi/admin.json` are committed, valid OpenAPI 3.1, cover every route on their port, and the drift test passes.
lets