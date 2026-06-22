# WaterMelon Browser — Architecture & Operational Flows

> Deep analysis of every major operational flow in the app. References use `file:line`.
> Generated from a full read-through of the Rust backend (`src-tauri/`), the Next.js
> frontend (`src/`), and the NestJS sync server (`watermelon-sync/`). Where a finding is a
> likely bug or a non-obvious quirk it is called out explicitly in [§11](#11-notable-findings-bugs--quirks).

## Table of contents

1. [Architecture overview](#1-architecture-overview)
2. [App startup](#2-app-startup)
3. [Profile lifecycle (create → launch → kill)](#3-profile-lifecycle)
4. [Browser engine management (download / extract / version)](#4-browser-engine-management)
5. [Fingerprint engine](#5-fingerprint-engine)
6. [Proxy + VPN networking](#6-proxy--vpn-networking)
7. [Cloud sync + E2E + auth](#7-cloud-sync--e2e--auth)
8. [Automation surfaces (REST API + MCP)](#8-automation-surfaces-rest-api--mcp)
9. [Cookie / extension / group management](#9-cookie--extension--group-management)
10. [Frontend architecture](#10-frontend-architecture)
11. [Notable findings (bugs & quirks)](#11-notable-findings-bugs--quirks)

---

## 1. Architecture overview

WaterMelon is a Tauri anti-detect browser:

- **Frontend** — Next.js 16 (App Router) + React 19 under `src/`. Event-driven; a single
  "god orchestrator" component `src/app/page.tsx` (~2000 lines) owns nearly all dialog state
  and action handlers.
- **Backend** — Rust under `src-tauri/`. **179 Tauri commands** registered in one
  `generate_handler!` block (`lib.rs:2154-2340`). **No Tauri managed state** — all shared
  state is module-global (`lazy_static`/`OnceLock`) accessed via `::instance()`.
- **Sidecar processes** — detached, survive GUI shutdown: `watermelon-proxy` (proxy worker)
  and VPN workers.
- **Three control surfaces share the same singleton managers**: the GUI (Tauri `invoke`), a
  **REST API** (axum, port 10108), and an **MCP server** (HTTP, port 51080). All three call
  the same managers and emit the same Tauri events, so the GUI updates reactively regardless
  of which surface mutated state.
- **Two browser engines**: **Cloak** (Chromium; fingerprint derived from a numeric
  `--fingerprint=<seed>` launch flag) and **Camoufox** (Firefox; fingerprint passed via
  `CAMOU_CONFIG_*` environment variables).

> ⚠️ **Incomplete rebrand**: the product was renamed from "Watermelon Browser". Many internal
> identifiers still use the old name (`com.watermelonbrowser`, the vault password, the clap program
> name). Two of these are **load-bearing bugs** — see [§11](#11-notable-findings-bugs--quirks).

### Global singletons (no `.manage()`)

`SETTINGS_MANAGER`, `PROXY_MANAGER`, `BROWSER_RUNNER`, `ProfileManager`, `BrowserFactory`,
`BrowserVersionManager`, `CLOUD_AUTH`, `DEFAULT_BROWSER`, `MCP_SERVER`, `API_SERVER`,
`GROUP_MANAGER`, `EXTENSION_MANAGER`, `VPN_STORAGE`, downloader/extractor, the sync
`SyncScheduler`, plus process-tracking maps (`PROXY_PROCESSES`, `EPHEMERAL_DIRS`,
`DOWNLOADING_BROWSERS`, …) and `GLOBAL_EMITTER` (`OnceLock`, set in the setup hook). The only
"state" threaded through commands is the `AppHandle`.

---

## 2. App startup

Entry point `run()` (`lib.rs:1318-1353`).

**Phase A — pre-builder** (`lib.rs:1320-1341`): scan `argv` for the first `http*` argument →
push to `PENDING_URLS` (the default-browser cold-start handoff). Resolve the log dir from
`app_dirs::app_name()` = `WaterMelonBrowser` (release) / `WaterMelonBrowserDev` (debug).

**Phase B — plugin registration** (`lib.rs:1343-1390`, in order): log → **single-instance** →
**deep-link** → fs / opener / shell / dialog / macos-permissions / clipboard. Single-instance
is registered before deep-link (correct ordering so the first instance owns the channel).

**Phase C — setup hook** (`lib.rs:1391-2153`): recover ephemeral dirs → extract extension
icons → **create the `main` window** (880×500, non-resizable; on Windows
`decorations(false)` for a custom titlebar) → system tray (best-effort) → **close interceptor**
(emit `close-confirm-requested`; the window does not quit unless `QUIT_CONFIRMED`) → init the
global event emitter → register deep links at runtime.

**Background tasks spawned** (~18 `tokio` tasks): version updater, MCP auto-start (if
`mcp_enabled`), stale-PID cleanup, orphan proxy/VPN worker reaping, app auto-update (3h), DNS
blocklist refresh (12h), Camoufox dead-instance cleanup (60s), GeoIP DB fetch, **dead-browser
proxy cleanup (30s)**, the **running-status broadcaster** (adaptive 5s/30s, emits
`profile-running-changed`), API server start, **sync scheduler + subscription**, cloud-auth
token refresh.

**Build-time** (`build.rs`): injects `BUILD_VERSION` and `WATERMELON_BROWSER_VAULT_PASSWORD`
(default `watermelonbrowser-api-vault-password`, the Argon2 input for at-rest token encryption),
generates tray icons from `icons/tray-icon.svg`, embeds the Windows manifest, and only invokes
`tauri_build::build()` if the `watermelon-proxy` sidecar exists in `binaries/`.

**Deep links & default browser**: three paths funnel into `handle_url_open` (`lib.rs:196-218`)
— CLI arg at cold start, the deep-link plugin `on_open_url`, and a single-instance relaunch.
The app shows a profile picker (`show-profile-selector` event) and opens the URL via
`open_url_with_profile`. Registers as handler for `http`/`https` (it is a full browser, not a
custom scheme). Default-browser registration is per-OS (`default_browser.rs`): macOS
`LSSetDefaultHandlerForURLScheme`, Windows HKCU `RegisteredApplications`/ProgID, Linux
`xdg-mime`.

---

## 3. Profile lifecycle

### Data model

`BrowserProfile` (`profile/types.rs:25-87`): each profile is a UUID-named directory under
`profiles_dir/` containing `metadata.json` + a `profile/` subdir (browser data). Key fields:
`browser`/`version`, `proxy_id` ⊕ `vpn_id` (**mutually exclusive**, enforced at create/update),
`process_id` (live PID — the on-disk running marker; `None` ⇒ not running),
`camoufox_config` (**holds the fingerprint** at `config.fingerprint`) / `cloak_config`
(**holds the numeric** `seed`),
`group_id`, `extension_group_id`, `ephemeral`, `password_protected`, `host_os`, and
`updated_at` (last-write-wins source of truth for metadata sync).

### Create (`manager.rs:73-414`)

Reject proxy+VPN both set; validate launch hook URL; case-insensitive name uniqueness; create
the UUID dir. **Fingerprint is generated at creation time**: the upstream proxy URL is
temporarily injected into the engine config so the fingerprint can geo-match, then **cleared**
(`manager.rs:227`,`:330`) — at launch the browser always points at the local proxy, never the
upstream directly. Writes `metadata.json` atomically (temp + fsync + rename), emits
`profiles-changed`.

### Launch — the heart of the system (`browser_runner.rs:194-749`)

1. Cross-OS guard (`is_cross_os`), team lock, scheduler marks running.
2. **Re-resolve the profile from disk by ID** to avoid stale `proxy_id`/`browser`.
3. Resolve proxy/VPN (`resolve_launch_proxy`): fire the launch hook (5s-timeout GET), refresh
   cloud creds, inject a per-profile sticky `sid`. If no proxy but a `vpn_id` exists, start a
   VPN worker and use its local socks5 port as the upstream.
4. **A local proxy is ALWAYS started** (`PROXY_MANAGER.start_proxy`, temp PID 0) — even for
   DIRECT — so traffic monitoring, the DNS blocklist, and bypass rules apply uniformly. The
   `http://127.0.0.1:port` URL is written into the engine config.
5. **Fingerprint applied**: stored fingerprint used as-is, unless
   `randomize_fingerprint_on_launch == Some(true)` → generate fresh and **persist it back**.
6. **Data dir**: password-protected → decrypt into a RAM ephemeral dir; ephemeral → RAM dir;
   otherwise `{uuid}/profile`.
7. Install extensions (Firefox XPIs for Camoufox; unpacked dirs as `--load-extension` for
   Cloak).
8. **Spawn** via `camoufox_manager`/`cloak_manager`, store `processId`, **remap proxy temp
   PID 0 → real PID** (`update_proxy_pid`), save, emit `profiles-changed` / `profile-updated` /
   `profile-running-changed`.

### Kill (`browser_runner.rs:1020-1494`)

**Stop the local proxy first** (by profile id) → find the process by data-dir → graceful stop →
**verify death** (sleep 500ms + `sysinfo` check); if alive → **force-kill** per platform; if
still not confirmed dead → **return an error and leave `process_id` intact**. On confirmed
death: clear `process_id`, apply any pending browser update, then teardown encrypted (await
re-encryption so the queued sync uploads the new ciphertext) / ephemeral (wipe the RAM dir).

### Running-state & events

The authoritative marker is `process_id`; liveness is verified on demand by
`check_browser_status`. Events: `profiles-changed` (reload list), `profile-updated` (single
profile), `profile-running-changed {id, is_running}` (lightweight toggle).

---

## 4. Browser engine management

Binaries live at `<data>/binaries/<browser>/<version>/`; registry at
`<data>/data/downloaded_browsers.json`.

- **Sources**: Cloak from **GitHub releases** `CloakHQ/cloakbrowser` (asset
  `cloakbrowser-{os}-{arch}.{zip|tar.gz}`). Camoufox from **GitHub releases**
  `daijro/camoufox` (**page 1 only** = first 100 releases; beta releases are classified as
  *stable*).
- **Download** (`downloader.rs:565`): re-resolve the real version → duplicate-download guard →
  **resume via `Range` header** + 3 retries with `2^n` backoff → append-mode write through an
  8 MiB `BufWriter` (cuts Defender/NTFS overhead) → 60s per-chunk idle timeout (keeps the
  partial file for resume) → emit `download-progress` every 100ms
  (downloading → extracting → verifying → completed).
- **Extraction** (`extraction.rs:129`): content-first format detection via magic bytes
  (ZIP/XZ/GZIP/BZ2/ELF/PE/OLE; DMG/MSI by extension). Cloak-on-Linux is `tar.gz`. macOS mounts the DMG and copies
  with `cp -RX` (avoids the Sequoia App-Management TCC prompt). Path-traversal-safe via
  `enclosed_name()`. Flattens single-wrapper-dir archives; locates the executable per-OS.
- **Versioning**: a version is added to the registry only **after extract + verify succeed**.
  Camoufox/Cloak version caches **never expire**. After a successful download,
  `AutoUpdater::update_profiles_to_latest_installed` bumps **non-running** profiles to the
  newest installed version (stable→stable, nightly→nightly only). Cleanup never deletes the
  last version of a browser, an in-progress download, or a pending update.
- **Per-engine launch**: **Cloak** = Chromium + a long hardening flag set; fingerprint
  derived from a numeric `--fingerprint=<seed>` flag (plus a few `--fingerprint-*` flags); the
  binary auto-generates GPU/screen/hardware from the seed — no runtime CDP injection. **Camoufox** =
  Firefox; fingerprint via **`CAMOU_CONFIG_*` env vars**; rewrites `user.js` on every launch
  (restores back/forward, disables QUIC because QUIC bypasses the proxy, enables unsigned
  XPIs); stderr → `$TMPDIR/camoufox-stderr-<id>.log`. Camoufox also needs the
  **GeoLite2-City.mmdb** (auto-fetched from `P3TERX/GeoLite.mmdb`).

---

## 5. Fingerprint engine

A fingerprint is a coherent bundle: screen geometry, navigator props, WebGL vendor/renderer,
fonts, canvas anti-aliasing offset, battery, locale/timezone/geolocation, and HTTP headers +
ordering (`camoufox/fingerprint/types.rs`).

- **Primary source = real presets** (`fingerprint-presets-v135.json` / `-v150.json` — harvested
  real fingerprints, selected by Firefox major version). The **Bayesian network is only a
  fallback** synthesizer when no preset exists (`config.rs:318-377`).
- **Bayesian model**: three embedded networks (fingerprint/input/header, from browserforge).
  Sampling uses **depth-first backtracking** (`bayesian_network.rs:94`) to guarantee internal
  consistency (a Windows UA won't be paired with a macOS platform). Pipeline: input sample →
  headers (extract UA) → pin UA → fingerprint sample → `transform_sample`.
- **Application**: assembled into a flat `HashMap` keyed by Camoufox property paths
  (`navigator.userAgent`, `screen.width`, …), geolocation overlaid (fetch public IP through the
  proxy → GeoLite2 → locale/timezone/lat-long/WebRTC IP), serialized to JSON, then **chunked
  into `CAMOU_CONFIG_1..N`** (2047 bytes on Windows, 32767 on Unix) and set as env vars on
  launch.
- **Determinism**: there is **no seed** — determinism comes from **persisting the generated
  output JSON**. The same config yields the same fingerprint each launch; it is only
  regenerated when `randomize_fingerprint_on_launch` is set.
- WebGL data comes from an embedded SQLite DB (OS-weighted random selection); canvas uses a
  random `aaOffset`; `window.history.length` spoofing was **deliberately removed** (newer
  Camoufox clamps session history and breaks back/forward).

---

## 6. Proxy + VPN networking

- **Supported upstreams**: HTTP / HTTPS / SOCKS4 / SOCKS5 **+ Shadowsocks**. Each `StoredProxy`
  is one JSON file. Conflict resolution uses `updated_at` (last-write-wins).
- **Cloud / dynamic proxies**: one base "Included Proxy"; geo-specific children are derived by
  encoding geo into the username (`{user}-country-xx-region-…`). **Sticky sessions**: the
  profile UUID is hashed to an 11-char base36 `sid` appended as `-sid-…-ttl-1440m`.
- **Local worker** (`watermelon-proxy`): the browser always speaks plain HTTP to
  `127.0.0.1:port`; the worker applies the real upstream (CONNECT tunnel, Basic auth, SOCKS,
  SS), counts bytes per domain, and applies the DNS blocklist + bypass rules. The worker is
  **double-detached** (setsid / `DETACHED_PROCESS`), priority-raised, and logs to
  `$TMPDIR/watermelon-proxy-<id>.log`.
- **Lifecycle**: keyed by `active_proxies[browser_pid]`. Dead-browser cleanup runs every 30s
  with a **2-strike debounce** (~60s) so a single `sysinfo` blip won't kill a healthy worker;
  it logs `"Cleanup: browser PID X is dead, stopping proxy worker …"`.
- **Validity check** (`check_proxy_validity`): spawns a temporary worker → fetches the public IP
  *through it* (the exact path the browser uses) → geolocates via ip-api.com → caches the
  result and a classified error (refused/timeout/407/402/…).
- **VPN = WireGuard only, fully userspace** (boringtun + smoltcp; **no TUN device or admin
  needed**). Configs are **AES-256-GCM encrypted at rest** (key in `.vpn_key`, chmod 600). The
  worker runs a **WireGuard→SOCKS5 bridge**: Noise handshake over UDP, smoltcp routes TCP,
  listens as a no-auth SOCKS5 server locally. IPv6 inside the tunnel is not implemented.
- **Precedence**: **proxy > VPN** — mutually exclusive at launch, never layered. The chosen
  upstream (proxy / VPN-socks5 / DIRECT) always becomes the *upstream* of the always-present
  local worker.

---

## 7. Cloud sync + E2E + auth

Two mechanisms through one `SyncEngine`:

- **(a) Profile files** — content-hash manifest (blake3) + diff; only changed files transfer.
  Excludes caches/WAL/Session Storage. `metadata.json` is hashed after stripping
  `last_sync`/`process_id`/`last_launch` (prevents sync loops). Direction is decided by the
  manifest `updated_at` (max mtime) — **whole-profile last-write-wins** — with an "empty-local →
  always download" guard against data loss.
- **(b) Config entities** (proxy/VPN/group/extension/profile-metadata) — one JSON blob each;
  conflict by **`updated_at` last-write-wins**. Optimized: **HEAD-stat reads
  `x-amz-meta-updated-at`** (no body download); falls back to a body GET for older servers.
  `last_sync` is bookkeeping only and never decides direction.

**Engine cycle** (`sync_profile`): guards (cross-OS / running / locked) → derive E2E key
(Argon2id) → checkpoint SQLite WAL → local + remote manifests → diff → upload then download
(semaphore 32, 3 retries, **a critical-file failure aborts the whole sync**, resume state every
50 files) → deletions → **upload the manifest last** (atomicity) → cascade-sync the profile's
proxy/group/vpn → merge remote metadata.

**Scheduler**: event-driven (SSE work items / profile-stop / explicit requests) with a 2s drain
tick; running profiles are deferred. **Subscription**: SSE `/v1/objects/subscribe`; the
self-hostable server **polls every 5s** (HEAD a `.watermelon-sync-manifest` marker, LIST only when
the ETag changes).

**E2E**: AES-256-GCM per-file plus a per-blob `EncryptedEnvelope` (own salt) for config
entities. Keys derived with Argon2id and cached by `(sha256(password), salt)`. The user's E2E
password lives in `e2e_password.dat`, itself encrypted under the compile-time vault password.
On Team plans only the owner may change the password.

**Auth**: cloud (JWT device-code exchange + proof-of-work; tokens encrypted at rest; refresh
loop every 600s) or self-hosted (a static `SYNC_TOKEN`). Team key prefix `teams/{id}/`. The
server scopes keys by user/team prefix, validates access, and uses tombstones for cross-device
deletes.

---

## 8. Automation surfaces (REST API + MCP)

Both are thin axum shells calling the **same singleton managers** as the GUI.

- **REST** (port 10108, localhost; `api_server.rs`): Bearer-token auth.
  OpenAPI/utoipa served unauthenticated at `/openapi.json`. Endpoints: profiles
  (run/kill/open-url/cookies), groups, tags, proxies, vpns, extensions, browsers.
  `run_profile` uses the same `launch_browser_profile_impl` as the GUI. Pro gates are mostly
  **removed**; the survivor is editing `camoufox_config` → 402 if no active subscription.
- **MCP** (HTTP streamable, port 51080; `mcp_server.rs`): protocol `2025-11-25`, sessions via
  the `mcp-session-id` header. ~50 tools: CRUD for profiles/groups/proxies/vpns/extensions plus
  **live browser control over CDP**: `navigate`, `screenshot`, `evaluate_javascript`,
  `click`/`type`, and `get_interactive_elements` + `click_by_index`/`type_by_index` (human-like
  typing). `require_paid_subscription` is currently a **no-op**.

---

## 9. Cookie / extension / group management

- **Cookies** (`cookie_manager.rs`): per-engine store (Chromium `Network/Cookies`, Firefox
  `cookies.sqlite`); an empty browser-compatible DB is created if the profile was never
  launched. Chromium decryption uses PBKDF2-HMAC-SHA1 (1003 iterations on macOS, 1 elsewhere).
  Writes are **always plaintext** into the `value` column (avoids os_crypt key mismatch). Import
  auto-detects JSON (Puppeteer/EditThisCookie) or Netscape `cookies.txt`; refuses while the
  browser is running.
- **Extensions** (`extension_manager.rs`): stored per-extension (not per-profile); compatibility
  by extension (`xpi`→firefox, `crx`→chromium, `zip`→both). Firefox **requires the file be named
  `<gecko_id>.xpi`** to sideload. Groups are assigned via `profile.extension_group_id` with a
  compatibility check. At launch: copy XPIs / unpack CRX → `--load-extension`.
- **Groups** (`group_manager.rs`): simple records; **membership is stored on the profile**
  (`group_id`), not on the group. The "Default" group is not materialized. Bulk operations
  delegate to `ProfileManager`. The REST `profile_count` is hardcoded to 0 (see §11).

---

## 10. Frontend architecture

- **`page.tsx`** owns nearly all dialog state and action handlers; the rail, command palette,
  and global keydown all route through `handleRailNavigate` / `runShortcut`.
- **Backend interaction**: direct `invoke()` (no central wrapper) for actions/reads, and
  `listen()` for push events. The dominant pattern is **mutate via command → backend emits
  `*-changed` → the subscribing hook re-fetches the full list** (never optimistic). The one
  exception is running-state, which uses the incremental `profile-running-changed` event
  (replacing a `sysinfo` poll that saturated the runtime at hundreds of profiles).
- **Genuine polling** only: version update (60s / 30min), commercial trial (60s), and
  permissions **on macOS only** (5s).
- **Shortcuts** (`lib/shortcuts.ts`): the `SHORTCUTS` table is the single source of truth;
  `matchesShortcut` does an exact, platform-correct modifier match. The Mod+K palette uses cmdk
  with a token-AND `fuzzyFilter`.
- **i18n**: 9 locales, default `vi`, locale-level fallback `en`. The **no-`t(key, "fallback")`
  rule** is a project policy (not a config flag). Structured backend errors `{code, params}` are
  localized via `translateBackendError` (`lib/backend-errors.ts`).
- **Sub-page Dialog**: a `subPage` prop turns a `Dialog` from a centered modal into an in-flow
  full-area page (`modal={false}` + inline-style overrides).

---

## 11. Notable findings (bugs & quirks)

| # | Finding | Severity |
|---|---------|----------|
| 1 | **Sidecar name mismatch — FIXED ✅.** `proxy_manager.rs` previously called `shell().sidecar("watermelon-proxy")` in three places (`start_proxy`, `stop_proxy`, `stop_proxy_by_profile_id`) while the externalBin is `watermelon-proxy` (`tauri.conf.json:22`). The stale name matched no on-disk binary, so `start_proxy` failed and **aborted every profile launch** (a local proxy is always started). Introduced by rename commit `31022d9`. Now renamed to `watermelon-proxy` and **verified end-to-end**: launching a Cloak profile spawned the proxy worker (bound `127.0.0.1:53307`, tunneled `CONNECT ipinfo.io:443`) and the browser launched with its fingerprint applied. | ✅ Fixed & verified |
| 2 | `vpn_worker_runner.rs:173` was also updated from `watermelon-proxy` to `watermelon-proxy` in the same rename sweep. Not treated as a separate defect. | ✅ Renamed |
| 3 | macOS default-browser code hardcodes `com.watermelonbrowser` (`default_browser.rs:57,79`), but `tauri.conf.json:5` declares `com.watermelonbrowser`. | 🟡 Medium |
| 4 | REST `ApiGroupResponse.profile_count` is hardcoded to 0 (`api_server.rs:1036,1076,1112,1149`); `get_extensions`/`get_extension_groups`/`export_vpn` are live routes but absent from the OpenAPI `paths(...)`. | 🟡 Low |
| 5 | Camoufox release discovery reads GitHub page 1 only (versions beyond the first 100 are invisible). | 🟡 Low |
| 6 | Default UI language is hardcoded to `"vi"` in `AppSettings::default()` (`settings_manager.rs:96`). | ℹ️ Info |

> Finding #1 was the load-bearing rebrand miss: it is fixed and confirmed working at runtime
> (proxy worker spawns, browser reaches the network through it). The proxy worker logs live at
> `%TEMP%\watermelon-proxy-*.log` and the GUI log at
> `%LOCALAPPDATA%\com.watermelonbrowser\logs\WaterMelonBrowserDev.log` (dev build).

---

*This document was generated by static analysis. Treat `file:line` references as of the commit
at generation time; verify before relying on them for changes.*
