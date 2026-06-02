# WaterMelon Browser — Đặc tả tính năng & luồng hoạt động (SPEC)

> Tài liệu phân tích chi tiết hiện trạng ứng dụng **WaterMelon Browser** — một anti-detect browser
> mã nguồn mở (AGPL-3.0). Mô tả kiến trúc, danh sách tính năng và các luồng vận hành thực tế
> của code base tại thời điểm phân tích (version `0.25.0`).

---

## 1. Tổng quan

**WaterMelon Browser** là trình duyệt chống phát hiện (anti-detect) cho phép tạo **không giới hạn**
các browser profile, mỗi profile được cô lập hoàn toàn (fingerprint, cookies, extensions, dữ liệu
riêng). Mục tiêu: multi-accounting, automation, bảo vệ quyền riêng tư mà không bị fingerprinting.

### Stack công nghệ

| Lớp | Công nghệ |
|---|---|
| **Desktop shell** | Tauri 2 (Rust) |
| **Frontend** | Next.js 16 + React 19 + TypeScript, Tailwind v4, shadcn/Radix UI, framer-motion |
| **Backend** | Rust (`src-tauri/`) — ~80 module, **179 Tauri command** |
| **Proxy worker** | Binary Rust riêng `donut-proxy` (tiến trình detached) |
| **Sync server** | NestJS (`watermelon-sync/`) — self-hostable, lưu trên S3-compatible storage |
| **Browser engines** | Wayfern (Chromium, qua CDP) + Camoufox (Firefox-based) |
| **i18n** | 9 ngôn ngữ: en, es, fr, ja, ko, pt, ru, vi, zh |
| **Nền tảng** | macOS (Intel/ARM), Windows x64, Linux (deb/rpm/AppImage) |

### Cấu trúc thư mục chính

```
donutbrowser/
├── src/                  # Frontend Next.js (app/, components/ 50+, hooks/, i18n/, lib/, types.ts)
├── src-tauri/src/        # Backend Rust (lib.rs đăng ký command, các manager, camoufox/, sync/, vpn/)
├── watermelon-sync/           # Server đồng bộ NestJS (self-host)
├── docs/                 # Hướng dẫn self-hosting
└── scripts/              # CI, publish repo, test harness
```

---

## 2. Mô hình dữ liệu cốt lõi

### BrowserProfile (đối tượng trung tâm)
Mỗi profile lưu: `id` (UUID), `name`, `browser` (`wayfern`|`camoufox`), `version`, `release_type`
(`stable`|`nightly`), `proxy_id`, `vpn_id`, `group_id`, `extension_group_id`, `tags[]`, `note`,
`launch_hook`, `process_id` (khi đang chạy), `last_launch`, cấu hình fingerprint
(`camoufox_config`/`wayfern_config`), `dns_blocklist`, `proxy_bypass_rules[]`, trạng thái sync
(`sync_mode`, `encryption_salt`, `last_sync`), bảo vệ mật khẩu (`password_protected`),
`ephemeral` (profile dùng RAM, tự xoá), `host_os`, và metadata team (`created_by_id/email`).

### Các thực thể đồng bộ độc lập
`StoredProxy`, `VpnConfig`, `ProfileGroup`, `Extension`, `ExtensionGroup` — mỗi thực thể là một
JSON nhỏ, có cờ `sync_enabled`, `updated_at` (last-write-wins) và `last_sync`.

---

## 3. Bố cục giao diện (UI)

Ứng dụng là **single-page app**, cửa sổ mặc định 880×500, không resize (vì kích thước cửa sổ ảnh
hưởng fingerprint). Có cảnh báo một lần khi resize trình duyệt fingerprinted.

- **Rail nav** (thanh dọc 40px bên trái): logo Donut (easter egg: click 5 lần), 6 mục —
  **Profiles, Proxies/VPN, Extensions, Groups, Integrations (API/MCP), Account** — cộng menu "More"
  (Import Profile, Keyboard Shortcuts) và nút **Settings** ở dưới cùng.
- **Header** (40px): vùng kéo cửa sổ, tab lọc theo nhóm (cuộn ngang), ô tìm kiếm (theo tên/note/tags),
  nút **+ New** tạo profile.
- **Khu vực chính**: bảng profile ảo hoá (TanStack Virtual) hiển thị browser, tên, trạng thái chạy,
  proxy, VPN, group, DNS blocklist, sync, extensions, và các action. Các trang khác mở dưới dạng dialog
  hoặc sub-page (Account/Settings/Proxy/Extension dùng chế độ sub-page — không có overlay modal).

### Onboarding (lần chạy đầu)
1. **Welcome Dialog** 4 bước: Intro (feature grid) → License (chấp nhận điều khoản thương mại Wayfern/
   Camoufox) → Permissions (xin quyền micro/camera, có thể bỏ qua) → Setup (tải browser + tạo profile đầu tiên).
2. **Product Tour** (Onborda) nếu chưa có profile: hướng dẫn tạo profile → cấu hình DNS blocking →
   modal "Thank You" khi hoàn tất.

### Bàn phím & Command Palette
- **Mod+K**: command palette (tìm kiếm command, jump nhóm, launch/stop/info profile — fuzzy filter token-AND).
- **Mod+/**: trang phím tắt; **Mod+O**: import; **Mod+P/N/E/G/I/A**: chuyển trang (nhấn lại để lật tab
  con, ví dụ Proxies↔VPN); **Mod+,**: Settings; **Mod+1..9**: nhảy tới nhóm theo chỉ số.
- Toàn bộ shortcut định nghĩa trong `src/lib/shortcuts.ts`, dispatch trong `src/app/page.tsx`.

### Hệ thống tray + đóng cửa sổ
Đóng cửa sổ → dialog xác nhận "Minimize to tray" hay "Quit". Tray icon cho phép Show/Quit; click trái
khôi phục cửa sổ (macOS/Windows). Single-instance: mở lại app sẽ focus cửa sổ hiện có.

---

## 4. Quản lý Profile (CRUD)

| Tính năng | Mô tả |
|---|---|
| **Tạo profile** | Chọn browser (Wayfern/Camoufox) + version + release type, gán proxy/VPN, group, extension group, DNS blocklist, đặt mật khẩu, launch hook. Trước khi tạo, hệ thống **validate proxy/VPN thực sự hoạt động** (`validate_profile_network`) — proxy chết hoặc hết hạn (402) sẽ chặn tạo. |
| **Clone profile** | Nhân bản profile hiện có. |
| **Launch / Kill** | Mở/đóng cửa sổ trình duyệt; trạng thái chạy theo dõi qua `process_id`. |
| **Rename / Delete** | Sửa tên inline; xoá đơn lẻ hoặc hàng loạt (có xác nhận). |
| **Profile Info** | Xem/sửa DNS blocklist, bypass rules, launch hook, note. |
| **Tags & Note** | Gắn thẻ tự do và ghi chú cho profile. |
| **Launch hook** | Lệnh/script chạy khi launch profile. |
| **Ephemeral profiles** | Profile lưu trong RAM (tmpfs/ramdisk), tự xoá; khôi phục mapping khi khởi động. |
| **Mật khẩu bảo vệ** | Mã hoá toàn bộ thư mục profile (xem §10). |

---

## 5. Hai browser engine & luồng launch

### 5.1 Luồng launch end-to-end
Khi user launch một profile (`browser_runner::launch_browser_internal`):

1. **Kiểm tra binary** — xác minh trình duyệt đã tải tại `binaries/<browser>/<version>/`; nếu thiếu thì
   tải qua `DownloadedBrowsersRegistry`/`downloader.rs`.
2. **Spawn proxy worker** — `donut-proxy` detached, cấp phát port `127.0.0.1:0`, ghi `ProxyConfig`
   xuống đĩa, chờ worker bind & cập nhật `local_url` (timeout ~4s). Detach bằng `setsid()` (Unix) /
   `DETACHED_PROCESS` (Windows), set priority cao.
3. **VPN worker** (nếu profile có `vpn_id`) — spawn `donut-proxy vpn-worker`, dựng tunnel WireGuard →
   SOCKS5 cục bộ; donut-proxy chain qua SOCKS5 này.
4. **Sinh fingerprint** — tuỳ engine (xem dưới).
5. **Spawn process trình duyệt** — kèm cờ proxy, user-data-dir, extensions; chờ sẵn sàng (CDP với
   Wayfern, stderr log với Camoufox).
6. **Theo dõi** — lưu `process_id` + `last_launch`, khởi tạo traffic tracker, phát event
   `profiles-changed`/`profile-running-changed`.

**Kill**: dừng proxy trước → kill browser (TERM rồi force) → dọn ephemeral dir → mã hoá lại profile có
mật khẩu → kiểm tra update đang chờ. Khi thoát app: `stop_all_proxy_processes` + `stop_all_vpn_workers`.

### 5.2 Wayfern (Chromium) — `wayfern_manager.rs`
- Điều khiển qua **CDP** (`--remote-debugging-port`), chờ CDP ready (tối đa ~60s).
- Fingerprint áp dụng runtime qua `Wayfern.setFingerprint`; trả về fingerprint đã "upgrade" theo phiên
  bản Chromium hiện tại và lưu ngược lại profile.
- `randomize_fingerprint_on_launch`: launch headless trên CDP riêng, gọi `Wayfern.refreshFingerprint`.
- Proxy truyền qua PAC URL (tránh QUIC bypass). Extensions qua `--load-extension`.
- **Wayfern là tính năng thương mại**: cần chấp nhận điều khoản (Wayfern Terms) + token cloud (chỉ user
  trả phí, tự refresh mỗi ~10h).

### 5.3 Camoufox (Firefox) — `camoufox_manager.rs` + `camoufox/`
- Fingerprint inject qua **biến môi trường** `CAMOU_CONFIG_1/2/...` (chunked) trước khi spawn.
- Sinh fingerprint **thực tế** bằng **mạng Bayesian** 3 tầng (input → headers → fingerprint), tối đa 10
  vòng retry thoả ràng buộc; hoặc dùng **preset** harvested thật (bundle `v135` legacy & `newer` cho
  Firefox ≥149), fallback Bayesian nếu không có preset cho OS.
- Extensions sideload `.xpi` (cần khớp `gecko_id`). Proxy qua pref Firefox (`network.proxy.*`).

### 5.4 Thuộc tính fingerprint bị giả mạo
Navigator (UA, platform, languages, hardwareConcurrency, deviceMemory, touch, UA-data, DNT), Screen
(kích thước, colorDepth, devicePixelRatio, inner/outer), **WebGL** (vendor/renderer lấy mẫu theo OS),
**Fonts** (theo OS + seed spacing), Audio codecs/sampleRate, Plugins/MIME, Battery API, **WebRTC**
(IPv4/IPv6 theo GeoIP), **Timezone + Geolocation** (lat/long/accuracy, locale theo GeoIP), Canvas noise
(aaOffset). Hỗ trợ **OS spoofing** (windows/macos/linux; Wayfern thêm android/ios).

### 5.5 Quản lý version & tải binary
`browser_version_manager.rs` + `downloader.rs` + `extraction.rs`: fetch danh sách version (cache-first),
phân loại stable/nightly, tải & giải nén (zip/tar/dmg/msi), registry theo dõi binary đã tải, kiểm tra
thiếu binary và tự tải lại cho profile đang dùng. `cancel_download` để huỷ.

---

## 6. Proxy

### Quản lý proxy lưu trữ (`proxy_manager.rs`, `proxy_storage.rs`)
- **Loại upstream hỗ trợ**: HTTP, HTTPS, SOCKS4, SOCKS5, **Shadowsocks (ss)**.
- CRUD proxy (`StoredProxy`), gán cho profile (đơn lẻ/hàng loạt).
- **Import/Export**: JSON và TXT (parser tự nhận dạng format, báo dòng ambiguous/invalid; có prefix tên).
- **Kiểm tra proxy** (`check_proxy_validity`): fetch IP public + geolocation (city/country), cache kết quả.
- **Dynamic proxy URL** và bypass rules (regex/exact theo hostname → kết nối trực tiếp).

### Proxy worker cục bộ `donut-proxy` (`proxy_server.rs`, `bin/proxy_server.rs`)
- Mỗi profile launch spawn một worker **detached riêng** (1 file log/worker tại `$TMPDIR/donut-proxy-<id>.log`).
- Xử lý song song: **CONNECT** (HTTPS tunnel) và **HTTP** plain; chuyển tiếp qua upstream tương ứng
  (reqwest cho HTTP/HTTPS, `async-socks5` cho SOCKS5, tự cài SOCKS4, crate `shadowsocks` cho SS).
- **Đếm traffic** (CountingStream) → feed `traffic_stats`. **DNS blocklist** chặn domain (403) trước khi
  kết nối. Task nền: snapshot stats mỗi 2s (real-time UI) + flush đĩa thích ứng 5–30s.

### Proxy cloud & theo vị trí (`cloud_auth.rs`)
User trả phí có thể dùng **cloud-included proxy** (`cloud-included-proxy`) và tạo proxy theo vị trí
(`create_cloud_location_proxy`) chọn theo **country → region → city → ISP** (các API `cloud_get_*`).
Theo dõi bandwidth đã dùng/giới hạn.

---

## 7. VPN (WireGuard)

- **Import** file `.conf` WireGuard hoặc nhập tay (`import_vpn_config`/`create_vpn_config_manual`); config
  lưu mã hoá trên đĩa (`vpn/storage.rs`).
- **Tunnel userspace** bằng `boringtun` + `smoltcp` (`vpn/wireguard.rs`); dựng **SOCKS5 server cục bộ**
  (`vpn/socks5_server.rs`) trên port localhost.
- **Chuỗi**: Browser → donut-proxy (port A) → SOCKS5 của VPN worker (port B) → tunnel WireGuard.
- VPN worker là tiến trình detached (sống sót khi GUI tắt). Lệnh: `connect_vpn`, `disconnect_vpn`,
  `get_vpn_status`, `list_active_vpn_connections`, `check_vpn_validity` (start worker tạm → fetch IP để
  xác minh hoạt động + geolocation).

---

## 8. Cookies, Extensions, Groups, Tags, Import

### Cookies (`cookie_manager.rs`)
- Đọc cookie từ DB Chromium (giải mã AES-128-CBC, key PBKDF2 theo OS; xử lý prefix v10/v11 + integrity SHA-256).
- Mô hình `UnifiedCookie` gom theo domain. **Copy cookie** từ profile nguồn sang nhiều profile đích (theo
  domain+name), **import** từ file (JSON/Netscape/Puppeteer), **export**, và **thống kê** nhanh (không giải mã).

### Extensions (`extension_manager.rs`)
- Hỗ trợ `.xpi` (Firefox), `.crx` (Chromium, quét offset ZIP), `.zip` (cả hai). Trích metadata từ
  `manifest.json` (name, version, description, author, homepage, `gecko_id`).
- **Extension Groups**: gom nhiều extension để gán hàng loạt cho profile.

### Groups & Tags (`group_manager.rs`, `tag_manager.rs`)
- **Profile Groups**: nhóm profile (lưu `groups.json`), lọc theo nhóm ở header, đếm số profile/nhóm,
  gán hàng loạt, áp cài đặt bulk.
- **Tags**: mảng string phẳng, rebuild từ tags của các profile (BTreeSet dedup/sort).

### Import profile từ trình duyệt khác (`profile_importer.rs`)
Tự phát hiện và import từ **Firefox, Firefox Developer, Zen** (→ camoufox) và **Chrome, Chromium, Brave,
Edge** (→ wayfern). Import tên, đường dẫn, cookies (giải mã nếu cần).

---

## 9. DNS AdBlocker & GeoIP

- **DNS blocklist** (`dns_blocklist.rs`): chặn quảng cáo/tracker theo **từng profile**; blocklist cache,
  refresh được (`refresh_dns_blocklists`, `get_dns_blocklist_cache_status`); matching theo suffix domain;
  thực thi tại donut-proxy (trả 403).
- **GeoIP** (`geoip_downloader.rs`): tải MaxMind GeoLite2-City `.mmdb`, cache, kiểm tra cũ (>7 ngày) tự
  tải lại. Dùng cho geolocation/timezone/locale của fingerprint và hiển thị vị trí proxy/VPN.

---

## 10. Bảo mật profile theo mật khẩu

- **Mã hoá file-level** (`profile/encryption.rs`): mỗi file mã hoá AES-256-GCM; tên file = HMAC-SHA256(key,
  relpath)[..32] (xác định, cùng mật khẩu → cùng tên); key từ **Argon2id(password, salt)**, cache trong
  phiên để tránh derive lại (~80–150ms).
- **Unlock khi launch** (`profile/password.rs`): file `.donut-pw-verify` kiểm tra mật khẩu trước khi chạm
  dữ liệu thật; **rate-limit**: 4 lần đầu miễn phí, sau đó backoff luỹ thừa (1m→5m→…→24h), lưu
  `.unlock-attempts.json`.
- Lệnh: `set/change/remove/verify_profile_password`, `lock/unlock_profile`, `is_profile_locked`.
- Tuỳ chọn `keep_decrypted_profiles_in_ram` giữ bản giải mã trong RAM giữa các lần launch để khởi động nhanh.

---

## 11. Cloud Account & License thương mại

### Đăng nhập cloud (`cloud_auth.rs`, `api_client.rs`)
- **Device code flow** (kèm proof-of-work chống lạm dụng) → nhận access/refresh token + đối tượng
  `CloudUser`. Token tự refresh (loop nền ~10 phút, refresh khi 401).
- **CloudUser/plan**: `plan` (free/pro/…), `subscriptionStatus`, `profileLimit`/`cloudProfilesUsed`,
  `proxyBandwidthLimit/Used/ExtraMb`, thông tin team (`teamId/Name/Role`), và **vị trí thiết bị**
  (`deviceOrdinal`, `deviceCount`, `isPrimaryDevice`) — **chỉ thiết bị primary (ordinal 1) được chạy
  automation/MCP**.

### License / Trial (`commercial_license.rs`)
- **Trial 2 tuần** tính từ lần chạy đầu (`first_launch_timestamp`); chỉ hiển thị thông báo hết hạn
  (`commercial-trial-modal`), không khoá tính năng local.
- **Miễn phí**: profile local không giới hạn, mọi browser, proxy/VPN cơ bản, groups, extensions, cookies,
  tags, DNS block, launch hook, mật khẩu, **self-hosted sync**.
- **Trả phí (Pro/Team)**: cloud sync (mirror đa thiết bị), team profile locking, **Wayfern automation +
  MCP**, cloud proxy & chọn vị trí, tăng giới hạn profile.

### Team (`team_lock.rs`)
- **Khoá profile** chống chỉnh sửa đồng thời giữa các thiết bị trong team: acquire lock trước launch,
  **heartbeat 30s** giữ lock, tự hết hạn nếu thiết bị offline; release sau khi quit. Hiển thị email người
  đang giữ lock. (`get_team_lock_status`, `get_team_locks`).

---

## 12. Automation: REST API & MCP Server

### REST API cục bộ (`api_server.rs`)
- Axum + tài liệu OpenAPI (utoipa). Bật/tắt qua Settings (`api_enabled`, `api_port`), xác thực **Bearer
  token**. Điều khiển profile (list/run/kill/CRUD), groups, proxies, vpns, tags, tải browser/version,
  import cookies.

### MCP Server (`mcp_server.rs`, `mcp_integrations.rs`)
- Server **Model Context Protocol** over HTTP (token qua path `/mcp/{token}` hoặc header Bearer), auto-start
  nếu đã bật trong Settings → Integrations.
- **Tích hợp Claude Desktop**: tự sinh extension (.mcpb) + bridge Node (stdio→HTTP) ghi vào thư mục
  Claude Extensions; cũng hỗ trợ các agent khác (generic install). Lệnh: `list_mcp_agents`,
  `add/remove_mcp_to_agent`.
- **Công cụ automation** (một số free, một số yêu cầu trả phí + primary device):
  - Điều hướng/quan sát (free): `navigate`, `get_page_info`, `get_dom_tree`, `screenshot`.
  - Tương tác (paid): `enumerate_interactive_elements`, `click_by_index`, `type_by_index`.
  - Quản lý profile/proxy qua MCP (paid).
- Gõ phím "người thật" (`human_typing.rs`): mô phỏng MarkovTyper — tỷ lệ lỗi/sửa, timing theo bigram/từ
  phổ biến/độ phức tạp, fatigue, phân phối chuẩn (Box-Muller); ký tự non-QWERTY (CJK/Cyrillic) bỏ qua mô
  phỏng lỗi.

---

## 13. Cloud Sync & đồng bộ thời gian thực

### Sync engine (`sync/`)
- **Hai cơ chế** lưu lên S3-compatible (Donut cloud hoặc `watermelon-sync` self-host):
  1. **File profile trình duyệt**: manifest theo content-hash (`manifest.rs`), chỉ truyền file thay đổi.
  2. **Config JSON đơn** (proxies, VPNs, groups, extensions, extension groups, **metadata profile**): mỗi
     thực thể một blob nhỏ, sync nguyên khối.
- **Conflict resolution — last-write-wins theo `updated_at`** (unix giây): chỉ bump khi user sửa thật;
  reconcile bằng HEAD object đọc `x-amz-meta-updated-at` (không tải body nếu không đổi), fallback GET body
  cho server cũ. `last_sync` chỉ để hiển thị, **không** quyết định hướng sync.
- **Sync mode**: `Disabled` / `Regular` (plaintext) / `Encrypted` (E2E).
- **E2E encryption** (`sync/encryption.rs`): key từ mật khẩu (Argon2id), AES-256-GCM per-file, tên file =
  HMAC; sai mật khẩu fail ngay ở auth tag. Hỗ trợ **rollover** (đổi mật khẩu → re-encrypt toàn bộ).
- **Subscription real-time (SSE)**: client mở `/v1/objects/subscribe`, server đẩy event khi S3 thay đổi →
  scheduler đưa vào hàng đợi và sync từng thực thể. Có thể **bật sync cho tất cả** (`enable_sync_for_all_entities`),
  đếm thực thể chưa sync, kiểm tra proxy/VPN/group đang được profile synced dùng.

### Synchronizer thời gian thực (`synchronizer.rs`)
- **Leader/Follower**: capture input (click/type/scroll/keyboard) trên một profile **Wayfern leader** qua
  CDP (`Wayfern.inputCaptured`) và **replay** lên các follower; theo dõi `failed_at_url` khi lệch. Lệnh:
  `start/stop_sync_session`, `get_sync_sessions`, `remove_sync_follower`. (Dùng cho điều khiển nhiều
  profile đồng thời.)

### Server self-host (`watermelon-sync/` NestJS)
- Lưu trên S3-compatible (AWS S3, MinIO…); xác thực JWT (public key từ cloud) hoặc token đơn giản; scope
  user theo prefix S3. Endpoints: `POST /v1/objects/{stat,presign-upload,presign-download,delete,list}` +
  SSE `subscribe`. `presignUpload` ký metadata vào `x-amz-meta-*` và echo lại để client gửi đúng header.

---

## 14. Cập nhật & nền tảng

- **Auto-update app** (`app_auto_updater.rs`): kiểm tra version mới (stable/nightly), tải & chuẩn bị,
  restart; có thể tắt (`disable_auto_updates`). Hỗ trợ repo Linux (deb/rpm tại `repo.donutbrowser.com`).
- **Cập nhật browser** (`auto_updater.rs`, `version_updater.rs`): kiểm tra version mới của Wayfern/Camoufox,
  cập nhật profile, dọn version cũ; background task định kỳ, có thể trigger thủ công.
- **Default browser** (`default_browser.rs`): đặt Donut làm trình duyệt mặc định; khi click link →
  **profile selector** chọn profile mở link (deep-link/startup URL → event `show-profile-selector`).
- **Logs**: GUI log tại thư mục log của OS (rotate 5MB, KeepAll); worker log tại `$TMPDIR`; Camoufox stderr
  riêng. App data: `DonutBrowser` (release) / `DonutBrowserDev` (debug), override bằng `DONUTBROWSER_DATA_ROOT`.
- **Traffic stats** (`traffic_stats.rs`): theo dõi bandwidth gửi/nhận, số request, domain truy cập, IP duy
  nhất theo profile/proxy; snapshot real-time + lịch sử lọc theo khoảng thời gian; biểu đồ (recharts).
- **Zero telemetry**: không tracking/fingerprint thiết bị người dùng.

---

## 15. Cài đặt ứng dụng (App Settings)

`settings_manager.rs` (`app_settings.json`): theme (light/dark/system + custom CSS vars), ngôn ngữ,
`set_as_default_browser`, `disable_auto_updates`, API (`api_enabled/port/token`), MCP (`mcp_enabled/port/
token`), sync (`sync_server_url`), trial (`first_launch_timestamp`, `commercial_trial_acknowledged`),
`keep_decrypted_profiles_in_ram`, sắp xếp bảng (`TableSortingSettings`), trạng thái onboarding & cảnh báo
resize. Lệnh đọc/ghi: `get/save_app_settings`, `get/save_sync_settings`, `get/save_table_sorting_settings`,
`open_log_directory`, `read_log_files`, `get_system_info`, `get_system_language`.

---

## 16. Tóm tắt nhóm tính năng

| Nhóm | Trạng thái |
|---|---|
| Profiles không giới hạn, cô lập hoàn toàn | ✅ Free |
| 2 engine: Wayfern (Chromium) / Camoufox (Firefox) | ✅ (Wayfern automation: Pro) |
| Anti-detect fingerprint (Bayesian + preset, OS spoof) | ✅ Free |
| Proxy HTTP/HTTPS/SOCKS4/SOCKS5/Shadowsocks + bypass + import/export | ✅ Free |
| VPN WireGuard per-profile | ✅ Free |
| Cloud proxy & chọn vị trí (country/region/city/ISP) | 💲 Pro |
| DNS AdBlocker per-profile | ✅ Free |
| Groups, Tags, Extension groups | ✅ Free |
| Cookie import/export/copy | ✅ Free |
| Import profile từ Chrome/Firefox/Edge/Brave/Chromium/Zen | ✅ Free |
| Mật khẩu + mã hoá profile | ✅ Free |
| Ephemeral profiles (RAM) | ✅ Free |
| Default browser + profile selector | ✅ Free |
| REST API cục bộ | ✅ Free |
| MCP server (Claude Desktop/Code…) | ✅ một phần free, tương tác/automation Pro + primary device |
| Cloud sync (mirror đa thiết bị) + E2E | 💲 Pro |
| Self-hosted sync (watermelon-sync) | ✅ Free |
| Team profile locking | 💲 Pro |
| Real-time leader/follower synchronizer | ✅ (Wayfern) |
| Auto-update app & browser | ✅ Free |
| i18n 9 ngôn ngữ, command palette, phím tắt | ✅ Free |
| Zero telemetry | ✅ |
```
