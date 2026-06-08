# Wayfern Engine — Phân tích kiến trúc & fingerprint

> Tài liệu phân tích nội bộ về nhân **Wayfern** (Chromium fork chống phát hiện) trong
> WaterMelon Browser: luồng hoạt động end-to-end, các tính năng liên quan, luồng
> fingerprint qua CDP, và so sánh field-level với Camoufox.
>
> Các trích dẫn `file:line` theo trạng thái code tại thời điểm phân tích (sau release
> `v0.0.8`). Số dòng có thể trôi khi code thay đổi — dùng tên hàm/symbol để định vị lại.

---

## 1. Wayfern là gì

Wayfern là **nhân Chromium (v148) đã fork + vá** để chống phát hiện, là 1 trong 2 engine
của app (cái còn lại là **Camoufox** — nền Firefox/Playwright). Điểm khác biệt cốt lõi:
Wayfern nhúng một **CDP domain tuỳ biến `Wayfern.*`**:

- `Wayfern.refreshFingerprint` — browser tự sinh fingerprint nội bộ-nhất-quán cho 1 OS
- `Wayfern.getFingerprint` — đọc fingerprint hiện tại (trả `{ fingerprint: {...} }`)
- `Wayfern.setFingerprint` — áp fingerprint vào page (echo lại bản đã dùng, có thể nâng cấp)
- `Wayfern.enableInputCapture` + sự kiện `Wayfern.inputCaptured` — nền tảng cho Synchronizer

Điều khiển qua **CDP chuẩn Chromium** (`http://127.0.0.1:<port>/json`), khác hẳn Camoufox
(WebDriver BiDi).

---

## 2. Luồng hoạt động end-to-end (Wayfern)

```
①  TẢI BINARY
    downloader.rs:158 → ApiClient.get_wayfern_download_url() theo version+platform
    → binaries/wayfern/<version>/, ghi DownloadedBrowsersRegistry
    (browser_version_manager.rs:77 kiểm tra platform hỗ trợ)

②  CHẤP NHẬN ĐIỀU KHOẢN (chỉ Wayfern)
    wayfern_terms.rs — chạy binary với --accept-terms-and-conditions
    → tạo license file (%APPDATA%\Wayfern\license-accepted)
    → điều kiện tiên quyết để bật MCP server (mcp_server.rs)

③  TẠO PROFILE
    profile/manager.rs:235 + lib.rs:1189 — browser=="wayfern" → gắn WayfernConfig
    (fingerprint, os, screen, block_webrtc/webgl, randomize_fingerprint_on_launch...)

④  LAUNCH (browser_runner.rs:470-746 điều phối)
    a. Lấy/khởi tạo WayfernConfig
    b. Phân giải proxy upstream hoặc VPN; không có proxy → khởi VPN worker (481-507)
    c. Khởi LOCAL PROXY (watermelon-proxy): giám sát traffic + DNS blocklist (521-536)
    d. Randomize fingerprint nếu bật (549-592) — xem Flow 3 mục 4
    e. Cài extension (.crx/.zip) nếu có extension group (612-631)
    f. Gọi wayfern_manager.launch_wayfern() (636-653)

⑤  launch_wayfern() (wayfern_manager.rs:493-885)
    - find_free_port() cấp cổng CDP ngẫu nhiên
    - Spawn Chromium: --remote-debugging-port=<port> --user-data-dir=<profile>
      --proxy-pac-url=<local proxy> + các --disable-*
    - wait_for_cdp_ready(): poll /json/version tới 120×500ms (chịu Gatekeeper/AV)
    - get_cdp_targets() qua /json → page target
    - ÁP FINGERPRINT QUA CDP: Wayfern.setFingerprint vào page target (792)
      → echo lại "used_fingerprint" (có thể NÂNG CẤP)
    - reset Emulation.* (clear device metrics, focus, emulated media)

⑥  PERSIST
    browser_runner.rs:659-675 — lưu fingerprint đã nâng cấp + process_id/cdp_port

⑦  SỬ DỤNG / ĐIỀU KHIỂN
    - Mở URL tab mới: /json/new (wayfern_manager.rs:918)
    - MCP automation: get_cdp_port_for_profile → WayfernManager.get_cdp_port
      (mcp_server.rs:3710) rồi nói CDP chuẩn — KHÁC camoufox (BiDi)
    - Synchronizer: Wayfern.enableInputCapture → nhận inputCaptured

⑧  KILL
    wayfern_manager.rs:887 stop_wayfern() → SIGTERM (Unix) / taskkill (Windows);
    find_wayfern_process_by_profile() quét tiến trình theo --user-data-dir để dọn
```

---

## 3. Wayfern liên quan đến những tính năng gì

### Chỉ Wayfern (không có ở Camoufox)
| Tính năng | Cơ chế | Vị trí |
|---|---|---|
| Fingerprint injection runtime | CDP `Wayfern.setFingerprint/getFingerprint/refreshFingerprint` | `wayfern_manager.rs:371,380,792` |
| Cross-OS fingerprint (giả OS khác) | `wayfernToken` từ cloud_auth gửi kèm setFingerprint | `wayfern_manager.rs:360-368,780-786` |
| Synchronizer (leader→follower) | `Wayfern.enableInputCapture` + sự kiện `inputCaptured` native; camoufox bị từ chối | `synchronizer.rs:113-115,423,595` |
| Terms/License | cờ `--accept-terms-and-conditions` + license file | `wayfern_terms.rs` |
| MCP automation qua CDP | `/json` discovery + CDP commands | `mcp_server.rs:3710` (camoufox đi BiDi) |

### Cả Wayfern và Camoufox (engine-agnostic — đi qua hạ tầng chung)
| Tính năng | Ghi chú |
|---|---|
| Proxy (http/socks) + VPN (WireGuard) | đều route qua **local proxy** `watermelon-proxy`; Wayfern dùng `--proxy-pac-url` |
| DNS blocklist | áp ở tầng local proxy (`browser_runner.rs:521`) |
| Traffic monitoring | local proxy |
| Extension groups | Wayfern: `.crx/.zip` qua `--load-extension`; camoufox: `.xpi` |
| Cookie import | `cookie_manager` chung |
| Password-protected / ephemeral profile | chung |
| MCP management tools (list/run/kill/proxy/group/vpn...) | chung |

### Điểm mấu chốt kiến trúc
- **`watermelon-proxy` (local proxy) là trục chung**: mọi traffic của cả 2 engine đi qua
  đây → proxy upstream/VPN, DNS blocklist, đo lưu lượng đều độc lập với engine.
- **Fingerprint là nơi 2 engine khác nhau nhất**: Wayfern áp **động sau launch qua CDP**
  (có thể nâng cấp + cross-OS); Camoufox **nướng sẵn vào binary/config lúc launch** (patch C++).
- **Synchronizer phụ thuộc 100% vào Wayfern** vì cần `Wayfern.inputCaptured` native.

---

## 4. Luồng fingerprint Wayfern qua CDP

**Ai sở hữu cái gì:** binary Wayfern **tự sinh fingerprint nhất quán** qua `Wayfern.*`;
app (Rust) (1) điều phối sinh bằng instance headless tách biệt, (2) phủ thêm
**geolocation** (timezone/locale/lat-long từ IP), (3) **lưu** chuỗi JSON trong
`profile.wayfern_config.fingerprint` (`wayfern_manager.rs:17-19`).

### Flow 1 — SINH (`generate_fingerprint_config`, `wayfern_manager.rs:230-490`)
```
1. Spawn 1 Wayfern HEADLESS RIÊNG trong temp dir, cổng CDP ngẫu nhiên (--headless=new)
   → KHÔNG đụng profile thật; sinh xong kill + xoá temp (cleanup, 281-300)
2. wait_for_cdp_ready: poll /json/version tới 120×500ms (=60s) — chịu Gatekeeper/AV
3. get_cdp_targets → ws của page target
4. Xác định OS: config.os hoặc OS máy
5. Lấy wayfernToken từ cloud_auth (có → cross-OS, paid) (361-368)
6. CDP Wayfern.refreshFingerprint { operatingSystem, wayfernToken? } (371)
7. CDP Wayfern.getFingerprint {} → { fingerprint: {...} } (380)
8. PHỦ GEOLOCATION (391-454): IP công khai (qua proxy nếu có) hoặc geoip config
   → chèn timezone, timezoneOffset, latitude, longitude, language, languages
   → lỗi geo: fallback America/New_York (offset 300)
9. Serialize → chuỗi JSON, lưu vào profile.wayfern_config.fingerprint
```

### Flow 2 — ÁP + NÂNG CẤP lúc launch (`launch_wayfern`, `wayfern_manager.rs:715-825`)
```
1. Đọc fingerprint lưu; tương thích ngược {fingerprint:{...}} (cũ) lẫn object trần (mới)
2. Vá profile cũ: thêm timezone/timezoneOffset mặc định nếu thiếu (737-746)
3. Denormalize + chuẩn hoá languages (chuỗi "a,b" → mảng) (749-757)
4. Thêm wayfernToken nếu có (779-786)
5. Mỗi page target: CDP Wayfern.setFingerprint { ...fields, wayfernToken? } (792)
6. ⭐ NÂNG CẤP: setFingerprint echo lại fingerprint browser THỰC SỰ dùng — có thể được
   nâng cấp (fingerprint lưu nhắm version cũ → browser bump). Bắt từ target đầu tiên
   thành công → used_fingerprint (800-817)
7. Reset emulation: clearDeviceMetricsOverride, setFocusEmulationEnabled(false),
   setEmulatedMedia("") (843-862)
```

### Flow 3 — RANDOMIZE mỗi launch (`browser_runner.rs:549-592`)
```
Nếu randomize_fingerprint_on_launch == true:
  - TRƯỚC launch: gọi Flow 1 với fingerprint=None (ép sinh mới)
  - Gán vào config dùng cho lần launch này + LƯU NGAY vào profile (giữ cờ randomize + os)
```

### Flow 4 — PERSIST nâng cấp (`browser_runner.rs:659-675`)
```
Sau launch, nếu used_fingerprint != bản lưu → ghi đè profile.wayfern_config.fingerprint
→ lần launch sau bắt đầu từ bản đã nâng cấp
```

### Sơ đồ vòng đời
```
[UI/tạo profile/randomize]
        │ Flow 1 (headless tách biệt)
        ▼ refreshFingerprint → getFingerprint → +geolocation(IP) → JSON
        │ lưu vào profile.wayfern_config.fingerprint
        ▼
[LAUNCH profile thật]  Flow 2
 setFingerprint(stored + token) ──► browser áp + echo "used_fingerprint" (có thể upgrade)
        │ Flow 4: nếu used ≠ stored ──► ghi đè lại vào profile
```

### Chi tiết kỹ thuật
- **2 "hình dạng" CDP**: get/setFingerprint bọc `{ fingerprint: {...} }`; lưu trữ là object
  trần → có `normalize`/`denormalize` (hiện no-op, `wayfern_manager.rs:127-143`) làm điểm móc.
- Field như fonts/webglParameters là **chuỗi JSON lồng** — giữ nguyên giữa storage ↔ CDP.
- **Geolocation tách khỏi fingerprint engine**: browser lo nhất quán; app lo timezone/locale
  theo IP proxy. `persona_locale_locked` (45) khoá locale theo persona (chỉ timezone/geo theo proxy mới).
- **wayfernToken = cross-OS (paid)**: giả fingerprint OS khác máy thật; `CLOUD_AUTH.get_wayfern_token()` (361,780).
- **Khác camoufox**: camoufox nướng fingerprint vào binary/config lúc launch (patch C++);
  Wayfern áp **động qua CDP sau khi mở** + cơ chế **echo-upgrade** giữ fingerprint hợp lệ qua bump version.

---

## 5. So sánh field-level: Wayfern `setFingerprint` vs Camoufox

### 5.1 Hai mô hình khác nhau từ gốc
| | **Wayfern (Chromium)** | **Camoufox (Firefox)** |
|---|---|---|
| Cách field tới engine | **CDP runtime**: `Wayfern.setFingerprint(obj)` vào page target sau khi CDP ready (`wayfern_manager.rs:792`) | **Env var lúc spawn**: JSON → map `browserforge.yml` → chunk `CAMOU_CONFIG_1..N` (`camoufox/env_vars.rs:28-47`) |
| Thời điểm áp | Sau load, có thể re-apply | Một lần, "nướng" trước trang đầu |
| Ai sinh fingerprint | Binary tự sinh (`refreshFingerprint`) | App tự sinh (Bayesian network/preset, `camoufox/fingerprint/`) |
| Echo-upgrade? | Có | Không |
| Namespace key | camelCase phẳng (`screenWidth`) | dotted + colon (`screen.width`, `webGl:vendor`, `battery:charging`) |
| Field phức tạp lưu dạng | **chuỗi JSON lồng** (fonts/plugins/webglParameters là string) | **cấu trúc** (mảng/object thật trong struct Rust) |

### 5.2 Bản đồ field tương ứng
| Nhóm | Wayfern (key phẳng) | Camoufox (dotted/colon) |
|---|---|---|
| User agent | `userAgent` | `navigator.userAgent` |
| Platform | `platform` | `navigator.platform` |
| OS CPU | — | `navigator.oscpu` *(FF-only)* |
| Vendor | `vendor`, `vendorSub`, `productSub` | `navigator.vendor`, `vendorSub`, `productSub` |
| CPU/Mem | `hardwareConcurrency`, (deviceMemory) | `navigator.hardwareConcurrency`, `navigator.deviceMemory` |
| Touch | (maxTouchPoints) | `navigator.maxTouchPoints` |
| DNT | (doNotTrack) | `navigator.doNotTrack` |
| Screen | `screenWidth/Height`, `screenAvailWidth/Height`, `screenColorDepth` | `screen.width/height`, `screen.availWidth/Height`, `screen.colorDepth/pixelDepth` |
| Window | `windowOuterWidth/Height`, `windowInnerWidth/Height`, `screenX/Y` | `window.outerWidth/Height`, `window.innerWidth/Height`, `window.screenX/Y` |
| WebGL | `webglVendor`, `webglRenderer`, `webglParameters`(JSON str), `webgl2Parameters` | `webGl:vendor`, `webGl:renderer`, `webGl:parameters`, `webGl2:parameters`, `webGl:shaderPrecisionFormats` |
| Fonts | `fonts` (**chuỗi JSON**) | `fonts` (**mảng**) + `fonts:spacing_seed` |
| Battery | `batteryCharging`, ... | `battery:charging/chargingTime/dischargingTime/level` |
| Timezone | `timezone` **+ `timezoneOffset`** (tính sẵn ở Rust) | `timezone` (FF tự suy offset từ IANA) |
| Geo | `latitude`, `longitude` (phẳng) | `geolocation:latitude/longitude`, `geolocation:accuracy` |
| Locale | `language` **+ `languages`(mảng)** | `locale:language/region/script` |

### 5.3 Chỉ Wayfern có (vì là Chromium)
- **UA Client Hints**: `platformVersion`, `brand`, `brandVersion` — Chromium expose
  `navigator.userAgentData`; Firefox/Camoufox **không có UA-CH** nên thiếu hẳn nhóm này.
- **Media queries**: `prefersReducedMotion`, `prefersDarkMode`, `prefersContrast`, `prefersReducedData`.
- **Color/HDR**: `colorGamutSrgb/P3/Rec2020`, `hdrSupport`.
- **Audio**: `audioSampleRate`, `audioMaxChannelCount`.
- **Storage toggles**: `localStorage`, `sessionStorage`, `indexedDb`.
- **Canvas**: `canvasNoiseSeed`.
- **Plugins kiểu Chrome**: `plugins`, `mimeTypes`, `voices` (đều **chuỗi JSON**).
- **OS**: thêm `android`, `ios` (Camoufox chỉ windows/macos/linux).

### 5.4 Chỉ Camoufox có (vì là Firefox/browserforge)
- `navigator.oscpu`, `navigator.buildID` — đặc trưng Firefox.
- **Codecs**: `video_codecs`, `audio_codecs`.
- **Devices**: `multimedia_devices`, `plugins_data`.
- `mock_web_rtc`, `slim`, `vendor_flavors`, `is_bluetooth_supported`, `pdf_viewer_enabled`, `installed_apps`.
- **Anti-fp mức seed**: `canvas:aaOffset`, `canvas:aaCapOffset`, `fonts:spacing_seed`.
- Toggle chặn trong chính fingerprint config: `block_images/block_webrtc/block_webgl`.

### 5.5 Kiểu dữ liệu & nhất quán
- **Camoufox**: struct Rust mạnh kiểu (`Fingerprint`, `ScreenFingerprint`, `NavigatorFingerprint`...)
  sinh bằng **Bayesian network** → các field "ăn khớp" (UA ↔ platform ↔ webgl), khó sửa lệch.
- **Wayfern**: object phẳng, nhiều field là **chuỗi JSON double-encoded**
  (`fonts`, `plugins`, `mimeTypes`, `voices`, `webglParameters`) — dễ chỉnh tay, nhất quán
  do **binary đảm bảo** (refresh/upgrade), không phải app.

### 5.6 Hệ quả thực tế
- **Sửa thủ công**: Wayfern dễ hơn (object phẳng) nhưng dễ tạo bộ không nhất quán; Camoufox
  khắt khe hơn (struct + browserforge) → an toàn nhưng kém linh hoạt.
- **Bề mặt che giấu**: Wayfern phủ thêm UA-CH/media-query/color-gamut/audio (đặc thù Chrome);
  Camoufox phủ codecs/devices/oscpu/buildID (đặc thù Firefox).
- **Geo**: Wayfern tính sẵn `timezoneOffset` + `languages[]`; Camoufox để FF suy offset và
  thêm **WebRTC IP spoof** kèm geo (`camoufox/config.rs`). Cả hai lấy locale/timezone từ IP proxy.
- **Nâng version browser**: Wayfern có **echo-upgrade** giữ fingerprint hợp lệ; Camoufox phải
  **regenerate** (đổi preset/version).

---

## Tham chiếu file chính
- `src-tauri/src/wayfern_manager.rs` — core Wayfern (launch, CDP, fingerprint gen/set/upgrade)
- `src-tauri/src/browser_runner.rs` — điều phối launch (proxy/VPN/extension/randomize/persist)
- `src-tauri/src/synchronizer.rs` — leader/follower qua `Wayfern.inputCaptured`
- `src-tauri/src/wayfern_terms.rs` — license/terms
- `src-tauri/src/mcp_server.rs` — MCP automation (CDP cho Wayfern, BiDi cho Camoufox)
- `src-tauri/src/camoufox/` — fingerprint (browserforge), config, geolocation, env_vars
- `src/components/wayfern-config-form.tsx`, `src/components/shared-camoufox-config-form.tsx` — form UI
