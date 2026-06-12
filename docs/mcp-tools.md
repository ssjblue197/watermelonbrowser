# MCP Tools Reference — WaterMelon Browser

Danh sách đầy đủ các tool mà MCP server (`src-tauri/src/mcp_server.rs`, hàm `get_tools()`)
expose để quản lý & tự động hóa trình duyệt.

## Kết nối
- **Endpoint:** `POST http://127.0.0.1:<port>/mcp/<token>` (mặc định port `51080`),
  hoặc header `Authorization: Bearer <token>`.
- **Protocol:** MCP Streamable HTTP, version `2025-11-25`, server name `donut-browser`.
- **Lấy token:** Settings → Integrations → tab MCP (lưu mã hoá ở `mcp_token.dat`).
- **Điều kiện:** bật MCP trong Settings.

## Ghi chú quan trọng
- Mô tả gốc của một số tool ghi *"Requires an active Pro subscription"* nhưng **gate đó đã được gỡ**
  (`require_paid_subscription` luôn trả `Ok`) — dùng được trên free tier.
- Nhóm **Page automation** cần profile **đang chạy**. Cloak → CDP (Chromium); Camoufox → WebDriver BiDi (từ 0.0.8).
- Tool kết quả trả dạng MCP content (`text`/`image`).

---

## A. Profile management

### `list_profiles`
List all Camoufox and Cloak browser profiles. — *không tham số.*

### `get_profile`
Get details of a specific browser profile.
| Param | Type | Required | Description |
|---|---|---|---|
| profile_id | string | ✅ | UUID của profile |

### `run_profile`
Launch a browser profile with an optional URL.
| Param | Type | Required | Description |
|---|---|---|---|
| profile_id | string | ✅ | UUID profile cần mở |
| url | string | | URL mở sẵn |
| headless | boolean | | Chạy headless |

### `kill_profile`
Stop a running browser profile.
| Param | Type | Required | Description |
|---|---|---|---|
| profile_id | string | ✅ | UUID profile cần tắt |

### `create_profile`
Create a new browser profile.
| Param | Type | Required | Description |
|---|---|---|---|
| name | string | ✅ | Tên profile |
| browser | string `enum: camoufox\|cloak` | ✅ | Engine |
| proxy_id | string | | Proxy UUID gán kèm |
| launch_hook | string | | URL HTTP(S) gọi trước khi launch (override proxy tạm thời) |
| group_id | string | | Group UUID |
| tags | string[] | | Tags |

### `update_profile`
Update an existing browser profile's settings.
| Param | Type | Required | Description |
|---|---|---|---|
| profile_id | string | ✅ | UUID profile |
| name | string | | Tên mới |
| proxy_id | string | | Proxy UUID (chuỗi rỗng để gỡ) |
| launch_hook | string | | Launch hook URL (rỗng để gỡ) |
| group_id | string | | Group UUID (rỗng để gỡ) |
| tags | string[] | | Tags (thay toàn bộ) |
| extension_group_id | string | | Extension group UUID (rỗng để gỡ) |
| proxy_bypass_rules | string[] | | Bypass rules (thay toàn bộ) |

### `delete_profile`
Delete a browser profile and all its data.
| Param | Type | Required | Description |
|---|---|---|---|
| profile_id | string | ✅ | UUID profile cần xoá |

### `get_profile_status`
Check if a browser profile is currently running.
| Param | Type | Required | Description |
|---|---|---|---|
| profile_id | string | ✅ | UUID profile |

### `list_tags`
List all tags used across profiles. — *không tham số.*

---

## B. Group management

### `list_groups`
List all profile groups. — *không tham số.*

### `get_group`
| Param | Type | Required | Description |
|---|---|---|---|
| group_id | string | ✅ | UUID group |

### `create_group`
| Param | Type | Required | Description |
|---|---|---|---|
| name | string | ✅ | Tên group |

### `update_group`
| Param | Type | Required | Description |
|---|---|---|---|
| group_id | string | ✅ | UUID group |
| name | string | ✅ | Tên mới |

### `delete_group`
| Param | Type | Required | Description |
|---|---|---|---|
| group_id | string | ✅ | UUID group |

### `assign_profiles_to_group`
Assign one or more profiles to a group.
| Param | Type | Required | Description |
|---|---|---|---|
| profile_ids | string[] | ✅ | Danh sách UUID profile |
| group_id | string | | UUID group (null để gỡ khỏi group) |

---

## C. Proxy management

### `list_proxies`
List all configured proxies. — *không tham số.*

### `get_proxy`
| Param | Type | Required | Description |
|---|---|---|---|
| proxy_id | string | ✅ | UUID proxy |

### `create_proxy`
| Param | Type | Required | Description |
|---|---|---|---|
| name | string | ✅ | Tên proxy |
| proxy_type | string `enum: http\|https\|socks4\|socks5` | ✅ | Loại proxy |
| host | string | ✅ | Host |
| port | integer | ✅ | Port |
| username | string | | User xác thực |
| password | string | | Mật khẩu xác thực |

### `update_proxy`
| Param | Type | Required | Description |
|---|---|---|---|
| proxy_id | string | ✅ | UUID proxy |
| name | string | | Tên mới |
| proxy_type | string `enum: http\|https\|socks4\|socks5` | | Loại |
| host | string | | Host |
| port | integer | | Port |
| username | string | | User |
| password | string | | Mật khẩu |

### `delete_proxy`
| Param | Type | Required | Description |
|---|---|---|---|
| proxy_id | string | ✅ | UUID proxy |

### `export_proxies`
| Param | Type | Required | Description |
|---|---|---|---|
| format | string `enum: json\|txt` | ✅ | Định dạng xuất |

### `import_proxies`
| Param | Type | Required | Description |
|---|---|---|---|
| content | string | ✅ | Nội dung proxy |
| format | string `enum: json\|txt` | ✅ | Định dạng nhập |
| name_prefix | string | | Tiền tố tên (mặc định 'Imported') |

---

## D. VPN (WireGuard)

### `import_vpn`
| Param | Type | Required | Description |
|---|---|---|---|
| content | string | ✅ | Nội dung file WireGuard |
| filename | string | ✅ | Tên file gốc (.conf) |
| name | string | | Tên hiển thị |

### `list_vpn_configs`
List all stored VPN configurations. — *không tham số.*

### `delete_vpn` / `connect_vpn` / `disconnect_vpn` / `get_vpn_status`
| Param | Type | Required | Description |
|---|---|---|---|
| vpn_id | string | ✅ | UUID VPN config |

---

## E. Fingerprint & proxy bypass

### `get_profile_fingerprint`
Get the fingerprint configuration for a Camoufox or Cloak profile.
| Param | Type | Required | Description |
|---|---|---|---|
| profile_id | string | ✅ | UUID profile |

### `update_profile_fingerprint`
Update the fingerprint configuration for a Camoufox or Cloak profile.
| Param | Type | Required | Description |
|---|---|---|---|
| profile_id | string | ✅ | UUID profile |
| fingerprint | string | | Chuỗi JSON fingerprint, hoặc null để xoá |
| os | string `enum: windows\|macos\|linux` | | OS cho việc sinh fingerprint |
| randomize_fingerprint_on_launch | boolean | | Sinh fingerprint mới mỗi lần launch |

### `update_profile_proxy_bypass_rules`
Requests matching these rules connect directly, bypassing the proxy.
| Param | Type | Required | Description |
|---|---|---|---|
| profile_id | string | ✅ | UUID profile |
| rules | string[] | ✅ | Hostname / IP / regex |

---

## F. DNS blocklist

### `update_profile_dns_blocklist`
Block ads/trackers/malware domains at the proxy level.
| Param | Type | Required | Description |
|---|---|---|---|
| profile_id | string | ✅ | UUID profile |
| level | string `enum: none\|light\|normal\|pro\|pro_plus\|ultimate` | ✅ | Mức chặn ('none' = tắt) |

### `get_dns_blocklist_status`
Cache status của các tier blocklist (số entry + độ mới). — *không tham số.*

---

## G. Extensions

### `list_extensions` / `list_extension_groups`
*không tham số.*

### `create_extension_group`
| Param | Type | Required | Description |
|---|---|---|---|
| name | string | ✅ | Tên extension group |

### `delete_extension`
| Param | Type | Required | Description |
|---|---|---|---|
| extension_id | string | ✅ | ID extension |

### `delete_extension_group`
| Param | Type | Required | Description |
|---|---|---|---|
| group_id | string | ✅ | ID extension group |

### `assign_extension_group_to_profile`
| Param | Type | Required | Description |
|---|---|---|---|
| profile_id | string | ✅ | UUID profile |
| extension_group_id | string | | ID extension group (rỗng để gỡ) |

---

## H. Cookie

### `import_profile_cookies`
Import cookie vào profile từ JSON array (Puppeteer/EditThisCookie) hoặc Netscape cookies.txt (tự nhận dạng). Browser không được chạy.
| Param | Type | Required | Description |
|---|---|---|---|
| profile_id | string | ✅ | UUID profile đích |
| content | string | ✅ | Nội dung cookie thô |

---

## I. Team lock (cần team plan)

### `get_team_locks`
List all active team profile locks. — *không tham số.*

### `get_team_lock_status`
| Param | Type | Required | Description |
|---|---|---|---|
| profile_id | string | ✅ | UUID profile |

---

## J. Page automation — điều khiển trình duyệt (cần profile đang chạy)

### `navigate`
Điều hướng tới URL; chờ trang load xong trước khi trả về.
| Param | Type | Required | Description |
|---|---|---|---|
| profile_id | string | ✅ | UUID profile đang chạy |
| url | string | ✅ | URL |

### `screenshot`
Chụp màn hình trang hiện tại; trả ảnh base64.
| Param | Type | Required | Description |
|---|---|---|---|
| profile_id | string | ✅ | UUID profile |
| format | string `enum: png\|jpeg\|webp` | | Định dạng (mặc định png) |
| quality | integer | | Chất lượng 0–100 cho jpeg/webp (mặc định 80) |
| full_page | boolean | | Chụp toàn trang cuộn (mặc định false) |

### `evaluate_javascript`
Chạy JS trong context trang, trả kết quả.
| Param | Type | Required | Description |
|---|---|---|---|
| profile_id | string | ✅ | UUID profile |
| expression | string | ✅ | Biểu thức JS |
| await_promise | boolean | | Chờ Promise nếu kết quả là Promise (mặc định false) |
| wait_for_load | boolean | | Chờ load sau khi chạy (dùng khi script gây điều hướng, vd form.submit()) (mặc định false) |

### `click_element`
Click element theo CSS selector; nếu gây điều hướng thì chờ trang mới load.
| Param | Type | Required | Description |
|---|---|---|---|
| profile_id | string | ✅ | UUID profile |
| selector | string | ✅ | CSS selector |

### `type_text`
Focus element theo selector và gõ chữ. Mặc định gõ kiểu người (tốc độ biến thiên, lỗi/tự sửa). Chỉ đặt `instant=true` khi chắc chắn target không có bot-detection.
| Param | Type | Required | Description |
|---|---|---|---|
| profile_id | string | ✅ | UUID profile |
| selector | string | ✅ | CSS selector |
| text | string | ✅ | Nội dung gõ |
| clear_first | boolean | | Xoá nội dung trước khi gõ (mặc định true) |
| instant | boolean | | Dán nguyên cụm (WARNING: chỉ dùng nơi không có bot-detection) |
| wpm | number | | Tốc độ gõ (mặc định 80 wpm) |

### `get_page_content`
Lấy nội dung trang (HTML hoặc text).
| Param | Type | Required | Description |
|---|---|---|---|
| profile_id | string | ✅ | UUID profile |
| format | string `enum: html\|text` | | 'html' = full HTML, 'text' = text hiển thị (mặc định text) |
| selector | string | | CSS selector lấy 1 element thay vì cả trang |

### `get_page_info`
Metadata trang: URL, title, readyState.
| Param | Type | Required | Description |
|---|---|---|---|
| profile_id | string | ✅ | UUID profile |

### `get_interactive_elements`
Liệt kê element tương tác (button/link/input...) dạng index gọn; index dùng cho click_by_index/type_by_index. Gọi lại sau mỗi navigation/đổi DOM lớn. Rẻ token hơn get_page_content.
| Param | Type | Required | Description |
|---|---|---|---|
| profile_id | string | ✅ | UUID profile |
| max_chars | integer | | Giới hạn độ dài output (mặc định 40000); response có cờ `truncated` |

### `click_by_index`
Click element theo index từ `get_interactive_elements`.
| Param | Type | Required | Description |
|---|---|---|---|
| profile_id | string | ✅ | UUID profile |
| index | integer | ✅ | Index 0-based |

### `type_by_index`
Focus element theo index và gõ (cùng quy ước human-typing như `type_text`).
| Param | Type | Required | Description |
|---|---|---|---|
| profile_id | string | ✅ | UUID profile |
| index | integer | ✅ | Index 0-based |
| text | string | ✅ | Nội dung gõ |
| clear_first | boolean | | Xoá trước khi gõ (mặc định true) |
| instant | boolean | | Dán nguyên cụm (chỉ nơi không có bot-detection) |
| wpm | number | | Tốc độ gõ (mặc định 80 wpm) |

---

*Nguồn: `src-tauri/src/mcp_server.rs` → `get_tools()` / `dispatch_tool_call()`. Cập nhật khi thêm/sửa tool.*
