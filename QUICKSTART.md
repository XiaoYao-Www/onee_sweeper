# ONEE SWEEPER 快速入門指南

## 5 分鐘快速開始

### 步驟 1：下載並運行程序

1. 下載 `onee_sweeper.exe`
2. 雙擊運行
3. 程序會在系統托盤顯示一個紅色圖標（表示未配置）

### 步驟 2：創建配置文件

1. 右鍵點擊托盤圖標
2. 選擇「開啟配置」
3. 系統會自動創建並打開 `config.toml` 文件

### 步驟 3：基礎配置

複製以下內容到 `config.toml`：

```toml
[app_setting]
small_scan_interval = 30
complete_scan_interval = 120
test_mode = true  # 重要：先用測試模式
log_max_size_mb = 10

[[tasks]]
folder_path = "C:/Users/你的用戶名/Downloads"  # 修改為你的下載資料夾
really_delete = false

target = [
    "*.tmp",
    "*.temp",
]

[tasks.threshold]
day = 7
hour = 0
minute = 0
```

**重要修改**：
- 將 `folder_path` 改為你的實際路徑
- 保持 `test_mode = true`（測試模式）

### 步驟 4：刷新配置

1. 保存配置文件
2. 右鍵托盤圖標
3. 選擇「刷新配置」
4. 圖標變為綠色表示配置成功

### 步驟 5：查看測試結果

1. 等待幾分鐘（或手動觸發掃描）
2. 右鍵托盤圖標
3. 選擇「查看日誌」
4. 查看日誌中的 `[測試模式] 將刪除: ...` 記錄

### 步驟 6：正式啟用

確認測試結果無誤後：

1. 打開配置文件
2. 將 `test_mode = true` 改為 `test_mode = false`
3. 刷新配置
4. 程序開始實際清理

## 常見配置場景

### 場景 1：清理下載資料夾

```toml
[[tasks]]
folder_path = "C:/Users/YourName/Downloads"
really_delete = false
target = ["*.tmp", "*.temp", "Temp_*"]

[tasks.threshold]
day = 7
hour = 0
minute = 0
```

### 場景 2：清理項目構建緩存

```toml
[[tasks]]
folder_path = "D:/Projects"
really_delete = true
target = ["**/target/**", "**/node_modules/**"]

[tasks.threshold]
day = 30
hour = 0
minute = 0
```

### 場景 3：清理整個臨時資料夾

```toml
[[tasks]]
folder_path = "C:/Temp"
really_delete = false
# 不設置 target，清理所有文件

[tasks.threshold]
day = 1
hour = 0
minute = 0
```

## 安全提示

✅ **推薦做法**：
- 首次使用開啟測試模式
- 使用垃圾桶模式（`really_delete = false`）
- 設置合理的閾值（至少 1 天）
- 定期查看日誌

❌ **避免做法**：
- 不測試就直接使用
- 對重要資料夾使用徹底刪除
- 設置過短的閾值
- 不檢查日誌

## 故障排除

### 問題：圖標一直是紅色

**原因**：配置文件有錯誤

**解決**：
1. 查看日誌文件
2. 檢查配置文件語法
3. 確認路徑存在

### 問題：沒有刪除任何文件

**可能原因**：
1. 測試模式開啟（正常）
2. 沒有文件超過閾值
3. target 模式不匹配

**解決**：
1. 檢查 `test_mode` 設置
2. 查看日誌確認掃描結果
3. 調整 target 或閾值

### 問題：刪除了不該刪的文件

**預防措施**：
1. 使用測試模式
2. 使用垃圾桶模式
3. 設置合理閾值
4. 正確配置 target

**恢復方法**：
- 如果使用垃圾桶模式，從垃圾桶恢復
- 如果徹底刪除，無法恢復（需要備份）

## 進階使用

### 多任務配置

```toml
# 任務 1：清理下載
[[tasks]]
folder_path = "C:/Users/YourName/Downloads"
# ... 配置 ...

# 任務 2：清理臨時文件
[[tasks]]
folder_path = "C:/Temp"
# ... 配置 ...

# 任務 3：清理項目緩存
[[tasks]]
folder_path = "D:/Projects"
# ... 配置 ...
```

### Glob 模式技巧

```toml
target = [
    "*.tmp",              # 所有 .tmp 文件
    "temp_*",             # 以 temp_ 開頭
    "cache/**",           # cache 資料夾及其內容
    "**/node_modules/**", # 所有 node_modules
    "log_202[0-3]*.txt",  # log_2020*.txt 到 log_2023*.txt
]
```

## 獲取幫助

- 查看完整文檔：README.md
- 查看配置示例：config.example.toml
- 查看更新日誌：CHANGELOG.md

## 下一步

1. 閱讀完整的 README.md 了解所有功能
2. 查看 config.example.toml 學習更多配置選項
3. 根據需求自定義配置
4. 定期檢查日誌確保正常運行
