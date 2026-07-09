# ONEE SWEEPER

一個智能的文件清理工具，自動掃描並清理過期檔案。採用 **daemon + UI 雙執行檔架構**，背景服務常駐系統托盤，圖形介面按需開啟。

---

## 功能特點

- 🔄 **雙模式掃描**：快速掃描（增量）與完整掃描（全量遍歷）
- 🎯 **精確匹配**：使用 glob 模式精準指定目標檔案
- 🗑️ **安全刪除**：可選移入垃圾桶或徹底刪除
- 📊 **高效資料庫**：使用 `rkyv` 二進位序列化，低開銷記錄檔案狀態
- 🛡️ **多重保護**：防止誤刪任務根目錄，支援排除模式
- ⏰ **排程時間窗**：可限制僅在特定時段內掃描
- 🖥️ **圖形化管理**：egui 所見即所得設定面板，無需手寫 TOML
- 🔔 **系統通知**：狀態變更與掃描結果即時桌面通知
- 📝 **完整日誌**：掃描記錄與刪除審計雙日誌

---

## 系統架構

```
┌─────────────────────┐    檔案級 IPC      ┌─────────────────────┐
│  onee_sweeper_daemon │ ◄─── signal ─────► │  onee_sweeper_ui    │
│  (背景常駐程式)       │   config.toml      │  (圖形設定面板)      │
│                     │ ◄── 共用設定 ────► │                     │
│  ・系統托盤          │                    │  ・egui 視窗         │
│  ・檔案監控          │                    │  ・表單編輯          │
│  ・排程掃描          │                    │  ・任務管理          │
│  ・閒置偵測          │                    │  ・日誌檢視          │
│  ・自動刪除          │                    │  ・操作面板          │
└─────────────────────┘                    └─────────────────────┘
```

兩個執行檔**必須放在同一目錄**，透過共用 `config.toml` 與信號檔案（`command.signal`、`config_reload.signal`）通訊。

---

## 系統要求

- Windows 10 或更高版本（64 位元）
- 約 10 MB 磁碟空間（含字型）

---

## 安裝

### 下載預編譯版本

從 [Releases](https://github.com/XiaoYao-Www/onee_sweeper/releases) 頁面下載最新 `.zip`，解壓縮後：
- `onee_sweeper_daemon.exe` — 背景服務（常駐）
- `onee_sweeper_ui.exe` — 設定面板（按需開啟）
- `fonts/jf-openhuninn-2.1.ttf` — 字型（UI 使用）

### 從原始碼編譯

```bash
git clone <repository-url>
cd onee_sweeper

# 編譯發布版本（workspace 自動編譯 daemon + UI + core）
cargo build --release

# 執行檔位於:
#   target/release/onee_sweeper_daemon.exe
#   target/release/onee_sweeper_ui.exe
```

---

## 快速開始

### 1. 啟動 daemon

雙擊 `onee_sweeper_daemon.exe`，系統托盤會出現圖示：
- 🔴 **紅色圖示**：配置不存在或不合法，程式暫停
- 🟢 **黃色圖示**：配置正確，正常運行

首次啟動會彈出「配置錯誤」通知，這是正常的——因為還沒有設定。

### 2. 開啟設定面板

右鍵托盤圖示 → **「開啟設定面板 (UI)」**，或直接執行 `onee_sweeper_ui.exe`。

### 3. 填寫設定

UI 分為四個分頁：

| 分頁 | 功能 |
|------|------|
| **設定編輯** | 編輯應用設定與任務列表，所見即所得 |
| **任務狀態** | 檢視當前配置與資料庫狀態 |
| **系統日誌** | 檢視 run.log 與 delete_audit.log 最後 100 行 |
| **操作面板** | 立即掃描、清除資料庫、啟動 daemon 等 |

### 4. 儲存

按下 **「儲存」** 按鈕 → daemon 自動收到信號重新載入配置 → 圖示切換為黃色 → 開始正常運行。

---

## 配置說明

### ├─ 應用設定 (`[app_setting]`)

| 欄位 | 型態 | 預設值 | 說明 |
|------|------|--------|------|
| `small_scan_interval` | 整數 | **必填** | 快速掃描間隔（分鐘），建議 15–60，不得小於 5 |
| `complete_scan_interval` | 整數 | **必填** | 完整掃描間隔（分鐘），建議 60–240，不得小於 15 |
| `test_mode` | 布林 | `false` | 測試模式：`true` 時只記錄不實際刪除 |
| `log_max_size_mb` | 整數 | `10` | 日誌檔上限（MB），超過後備份為 `.log.old` |
| `idle_threshold_min` | 整數 | `5` | 使用者閒置 N 分鐘後才執行掃描，避免干擾操作 |
| `max_memory_mb` | 整數 | `50` | 記憶體上限（MB），超過則強制釋放 |
| `notification_level` | 字串 | `"summary"` | 通知詳細程度：`"none"`（關閉）、`"summary"`（摘要）、`"verbose"`（詳細） |
| `scan_on_startup` | 布林 | `true` | 啟動時立刻執行一次掃描 |

### ├─ 任務 (`[[tasks]]`)

每個任務定義一個要清理的目錄。可重複多個 `[[tasks]]` 區塊。

| 欄位 | 型態 | 預設值 | 說明 |
|------|------|--------|------|
| `folder_path` | 字串 | **必填** | 目標根目錄絕對路徑 |
| `target` | 字串陣列 | `無`（全部清理） | glob 模式，只刪除匹配的檔案／資料夾 |
| `exclude` | 字串陣列 | `無` | glob 排除模式，永遠不刪除匹配項目 |
| `really_delete` | 布林 | `false` | `true` = 徹底刪除（不進垃圾桶）；`false` = 移入垃圾桶 |
| `threshold` | 物件 | **必填** | 時間閾值（詳見下方） |
| `enabled` | 布林 | `true` | `false` 時暫時停用此任務 |
| `scan_mode` | 字串 | `"both"` | 掃描模式：`"both"`（兩者）、`"small_only"`（僅快速）、`"complete_only"`（僅完整） |
| `follow_symlinks` | 布林 | `false` | 是否跟隨符號連結 |
| `schedule_window` | 物件 | `無`（不限時段） | 僅在此時間窗內掃描（可選） |

### ├─ 時間閾值 (`[tasks.threshold]`)

| 欄位 | 型態 | 範圍 | 說明 |
|------|------|------|------|
| `day` | 整數 | 0–255 | 天數 |
| `hour` | 整數 | 0–23 | 小時 |
| `minute` | 整數 | 0–59 | 分鐘 |

**至少一個值大於 0**（總和 ≥ 3600 秒較安全，防止誤刪）。檔案最後修改時間超過此閾值即被判定為過期。

### ├─ 時間窗 (`[tasks.schedule_window]`)

| 欄位 | 型態 | 格式 | 說明 |
|------|------|------|------|
| `start` | 字串 | `"HH:MM"` (24h) | 可開始掃描的時間 |
| `end` | 字串 | `"HH:MM"` (24h) | 結束掃描的時間 |

範例：`02:00`–`06:00` 表示只在凌晨 2 點到 6 點間掃描。

---

## 配置範例

### 範例 1：基本 — 清理下載資料夾（測試模式）

```toml
[app_setting]
small_scan_interval = 30
complete_scan_interval = 120
test_mode = true

[[tasks]]
folder_path = "C:/Users/YourName/Downloads"
really_delete = false

[tasks.threshold]
day = 7
hour = 0
minute = 0
```

### 範例 2：多任務 — 清理快取 + 暫存檔

```toml
[app_setting]
small_scan_interval = 15
complete_scan_interval = 60
notification_level = "summary"
scan_on_startup = true

[[tasks]]
folder_path = "C:/Temp"
really_delete = true
target = ["*.tmp", "*.log", "temp_*"]

[tasks.threshold]
day = 1
hour = 0
minute = 0

[[tasks]]
folder_path = "D:/Projects"
really_delete = false
target = ["**/target/**", "**/node_modules/**"]
exclude = ["**/important/**"]

[tasks.threshold]
day = 30
hour = 0
minute = 0
```

### 範例 3：進階 — 時間窗 + 掃描模式限制

```toml
[app_setting]
small_scan_interval = 30
complete_scan_interval = 180
idle_threshold_min = 10
max_memory_mb = 100
notification_level = "verbose"

[[tasks]]
folder_path = "C:/Logs"
really_delete = true
scan_mode = "complete_only"  # 完整掃描時才處理

[tasks.threshold]
day = 14
hour = 0
minute = 0

[tasks.schedule_window]
start = "02:00"
end = "05:00"

[[tasks]]
folder_path = "C:/Users/Public/Downloads"
really_delete = false
scan_mode = "both"
follow_symlinks = true
enabled = true

[tasks.threshold]
day = 3
hour = 0
minute = 0
```

---

## 掃描機制

### 雙模式

| | 快速掃描 (Small Scan) | 完整掃描 (Complete Scan) |
|---|---|---|
| 範圍 | 資料庫中已記錄的檔案 | 完整遍歷目錄樹 |
| 速度 | 快（秒級） | 較慢（取決於檔案數） |
| 新檔案 | 不發現 | 發現並記錄 |
| 已刪除檔案 | 不清理 | 清理資料庫記錄 |
| 適合 | 頻繁執行 | 定時全量檢查 |

### 時間判定

1. **檔案**：使用檔案系統的「最後修改時間」
2. **資料夾**：使用資料夾內最新檔案的修改時間
3. **首次發現**：記錄當前時間為基準
4. **更新檢測**：檔案修改時間改變 → 重置計時

### 閒置偵測

daemon 會偵測使用者是否點擊托盤選單。在設定 `idle_threshold_min` 分鐘內有活動時，會推遲掃描以避免干擾。

### 安全機制

| 機制 | 說明 |
|------|------|
| 根目錄保護 | 永遠不會刪除 `folder_path` 本身 |
| 路徑驗證 | 確認刪除目標在任務目錄範圍內 |
| target 過濾 | 有設定 target 時，只刪除 glob 匹配項目 |
| exclude 排除 | 永遠不刪除排除模式匹配的檔案 |
| 測試模式 | `test_mode = true` 時只記錄不刪除 |
| 垃圾桶 | `really_delete = false` 時可從垃圾桶還原 |
| 最短閾值 | `threshold < 3600 秒` 會發出警告 |

---

## 托盤選單

右鍵系統托盤圖示：

| 選單項目 | 說明 |
|----------|------|
| 開啟設定面板 (UI) | 啟動 `onee_sweeper_ui.exe` |
| 創建開機啟動 | 在 Windows 啟動資料夾建立捷徑 |
| 移除開機啟動 | 刪除開機啟動捷徑 |
| 退出 | 結束 daemon 程式 |

---

## UI 設定面板

執行 `onee_sweeper_ui.exe` 後的四個分頁：

### 設定編輯
- 所有應用設定與任務欄位的表單控制項
- 支援 RAW TOML 模式（進階使用者）
- 新增／刪除任務
- 展開收合任務詳細設定

### 任務狀態
- 顯示當前配置摘要
- 資料庫檔案大小與更新時間

### 系統日誌
- `run.log` — 執行日誌（最後 100 行）
- `delete_audit.log` — 刪除審計日誌（最後 100 行）

### 操作面板
- 立即掃描（向 daemon 發送 `scan_now` 信號）
- 清除資料庫 (`temp.bin`)
- 啟動 daemon

---

## 日誌與資料

所有檔案均位於執行檔同目錄：

| 檔案 | 說明 |
|------|------|
| `config.toml` | 設定檔（由 UI 寫入，daemon 讀取） |
| `run.log` | 執行日誌（自動輪替，上限由 `log_max_size_mb` 控制） |
| `run.log.old` | 舊日誌備份 |
| `delete_audit.log` | 刪除審計日誌 |
| `temp.bin` | 掃描資料庫（`rkyv` 二進位格式） |
| `daemon.pid` | daemon 程序 ID（供 UI 判斷是否運行） |
| `command.signal` | UI 發送給 daemon 的即時命令信號 |
| `config_reload.signal` | UI 通知 daemon 重新載入設定的信號 |

---

## 常見問題

### 程式會誤刪重要檔案嗎？

多重保護機制確保安全：
1. 預設移入垃圾桶，可還原
2. 根目錄保護
3. 排除模式
4. 測試模式可先試跑
5. 不足 1 小時的閾值會警告

### 如何確認配置行為正確？

1. 設定 `test_mode = true`
2. 儲存後觀察日誌，會顯示「[測試模式] 將刪除: ...」
3. 確認無誤後關閉測試模式

### 快速掃描與完整掃描有何不同？

快速掃描只檢查已記錄的檔案（增量），完整掃描遍歷整個目錄樹（全量）。建議快速 15–30 分鐘、完整 2–4 小時。

### 不設定 target 會怎樣？

不設 `target` 時，該任務會清理 `folder_path` 內**所有**超過閾值的檔案與子資料夾（但不會刪除根目錄本身）。

### 修改配置後需要重啟程式嗎？

不需要。在 UI 點擊「儲存」後，daemon 會自動重新載入配置。

### 程式佔用多少資源？

- 記憶體：通常 < 20 MB（設定上限 50 MB）
- CPU：掃描時短暫使用，其餘時間接近 0
- 磁碟：純 I/O 操作

### 程式崩潰會遺失資料嗎？

不會。掃描資料庫使用原子寫入，損壞時自動從備份恢復。

---

## 開發

### 專案結構

```
onee_sweeper/
├── daemon/           # 背景服務（常駐）
│   └── src/
│       ├── main.rs   # 事件迴圈、托盤、掃描排程
│       ├── scanner.rs # 掃描引擎、資料庫
│       └── audit_log.rs # 審計日誌
├── ui/               # 圖形設定面板
│   └── src/
│       └── main.rs   # egui 視窗、表單編輯
├── core/             # 共用核心
│   └── src/
│       ├── config.rs     # 設定檔讀寫、版本遷移、校驗
│       ├── type_define.rs # 資料結構定義
│       └── lib.rs
├── assets/           # 資源
│   ├── icon_run.ico   # 運行圖示
│   ├── icon_stop.ico  # 停止圖示
│   └── jf-openhuninn-2.1.ttf  # 字型
├── Cargo.toml        # workspace 定義
└── README.md
```

### 編譯指令

```bash
# 開發版本
cargo build

# 發布版本（全 LTO、panic=abort、strip）
cargo build --release

# 只編譯特定元件
cargo build -p onee_sweeper_daemon
cargo build -p onee_sweeper_ui

# 執行測試
cargo test
```

### 使用的技術

- **winit** — 事件迴圈
- **tray-icon** — 系統托盤
- **egui / eframe** — 圖形介面
- **rkyv** — 二進位序列化（零拷貝 deserialize）
- **serde / toml** — 設定解析
- **notify** — 檔案系統監控
- **notify-rust** — 桌面通知
- **mimalloc** — 高效記憶體分配器

---

## 授權與致謝

### 主授權

本專案（ONEE SWEEPER）原始碼採用 **GNU General Public License v3.0** 授權。完整條款請見 [LICENSE](./LICENSE) 檔案。

### 字型授權

本軟體分發時包含 **jf open huninn 粉圓體 v2.1**，授權於 SIL Open Font License v1.1。

- Copyright © 2020–2024 **justfont Co., Ltd.**
- Reserved Font Names: 'open huninn', 'huninn'
- 中文字元部分衍生自 **Kosugi Maru**（Apache-2.0, © 2010 MOTOYA CO.,LTD.）
- 拉丁／希伯來字母部分衍生自 **Varela Round**（SIL OFL v1.1, © 2011–2016 The Varela Round Project Authors）

詳情請參閱 [LICENSE](./LICENSE) 中的「第三方程式元件授權聲明」一節。

---

*如有問題或建議，請通過 GitHub Issues 聯絡。*
