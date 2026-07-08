use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 應用設定
#[derive(Deserialize, Serialize, Debug)]
#[allow(dead_code)]
pub struct AppSettings {
    pub small_scan_interval: u64, // 分鐘
    pub complete_scan_interval: u64, // 分鐘
    pub test_mode: Option<bool>,
    pub log_max_size_mb: Option<u64>, // 日誌最大大小（MB），超過則清空

    // === 新增欄位（可選，向後相容） ===
    /// 閒置 N 分鐘後才掃描（不影響使用者操作），預設 5 分鐘
    pub idle_threshold_min: Option<u64>,
    /// 記憶體上限（MB），超過則暫停掃描，預設 50 MB
    pub max_memory_mb: Option<u64>,
    /// 通知詳細程度："none" | "summary" | "verbose"，預設 "summary"
    pub notification_level: Option<String>,
    /// 啟動時立刻掃描一次，預設 true
    pub scan_on_startup: Option<bool>,
}

/// 時間視窗（用於排程掃描時間）
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ScheduleWindow {
    pub start: Option<String>, // "HH:MM" 格式
    pub end: Option<String>,   // "HH:MM" 格式
}

/// 單一任務設定
/// 
/// 部分欄位為 serde 反序列化用，由 config 模組消費，平時不直接讀取。
#[derive(Deserialize, Serialize, Debug)]
#[allow(dead_code)]
pub struct FolderTask {
    pub folder_path: PathBuf,
    pub target: Option<Vec<String>>, // 目標檔案或資料夾(相對路徑)
    pub really_delete: Option<bool>,
    pub threshold: Threshold,

    // === 新增欄位（可選，向後相容） ===
    /// 排除模式（glob），永遠不刪除匹配的檔案
    pub exclude: Option<Vec<String>>,
    /// 最小檔案大小（bytes），小於此值不刪除
    pub min_size_bytes: Option<u64>,
    /// 最大檔案大小（bytes），大於此值不刪除
    pub max_size_bytes: Option<u64>,
    /// 是否跟隨符號連結，預設 false
    pub follow_symlinks: Option<bool>,
    /// 首次發現後至少 N 天才能刪除（防止刪除剛下載的檔案）
    pub min_age_before_delete: Option<u64>,
    /// 是否啟用此任務，預設 true
    pub enabled: Option<bool>,
    /// 掃描模式："both" | "small_only" | "complete_only"，預設 "both"
    pub scan_mode: Option<String>,
    /// 僅在此時間窗內掃描（可選）
    pub schedule_window: Option<ScheduleWindow>,
}

/// 時間閾值
#[derive(Deserialize, Serialize, Debug)]
pub struct Threshold {
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
}

/// 頂層 Config
#[derive(Deserialize, Serialize, Debug)]
#[allow(dead_code)]
pub struct Config {
    /// 設定檔版本（用於自動遷移）
    pub config_version: Option<u32>,
    pub app_setting: AppSettings,
    pub tasks: Vec<FolderTask>,
}

// ── 預設值輔助 ──────────────────────────────────────────────────────────────

#[allow(dead_code)]
impl AppSettings {
    /// 取得有效的 idle_threshold_min（預設 5）
    pub fn idle_threshold_min_effective(&self) -> u64 {
        self.idle_threshold_min.unwrap_or(5)
    }
    /// 取得有效的 max_memory_mb（預設 50）
    pub fn max_memory_mb_effective(&self) -> u64 {
        self.max_memory_mb.unwrap_or(50)
    }
    /// 取得有效的 scan_on_startup（預設 true）
    pub fn scan_on_startup_effective(&self) -> bool {
        self.scan_on_startup.unwrap_or(true)
    }
}

#[allow(dead_code)]
impl FolderTask {
    /// 是否啟用（預設 true）
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
    /// 是否跟隨符號連結（預設 false）
    pub fn follow_symlinks_effective(&self) -> bool {
        self.follow_symlinks.unwrap_or(false)
    }
    /// 最小檔案大小（預設 0）
    pub fn min_size_bytes_effective(&self) -> u64 {
        self.min_size_bytes.unwrap_or(0)
    }
    /// 最大檔案大小（預設 u64::MAX，不限制）
    pub fn max_size_bytes_effective(&self) -> u64 {
        self.max_size_bytes.unwrap_or(u64::MAX)
    }
}

// ── 校驗 ────────────────────────────────────────────────────────────────────

impl Config {
    /// ### 驗證配置是否合法
    ///
    /// 回傳錯誤訊息列表，空代表合法。
    pub fn validate(&self) -> Vec<String> {
        let mut errors: Vec<String> = Vec::new();

        // 驗證任務列表
        if self.tasks.is_empty() {
            errors.push("tasks 不能為空".to_string());
        }

        // 驗證掃描間隔
        if self.app_setting.small_scan_interval == 0 {
            errors.push("small_scan_interval 不能為 0".to_string());
        }

        if self.app_setting.complete_scan_interval == 0 {
            errors.push("complete_scan_interval 不能為 0".to_string());
        }

        if self.app_setting.complete_scan_interval < self.app_setting.small_scan_interval {
            errors.push("complete_scan_interval 不應小於 small_scan_interval".to_string());
        }
        
        // 驗證掃描間隔不要過短（防止資源浪費）
        if self.app_setting.small_scan_interval < 5 {
            errors.push("small_scan_interval 不應小於 5 分鐘（防止資源浪費）".to_string());
        }
        
        if self.app_setting.complete_scan_interval < 15 {
            errors.push("complete_scan_interval 不應小於 15 分鐘（防止資源浪費）".to_string());
        }

        // 驗證 notification_level 合法值
        if let Some(ref level) = self.app_setting.notification_level {
            match level.as_str() {
                "none" | "summary" | "verbose" => {}
                _ => errors.push(format!("notification_level 無效: {}（必須為 none/summary/verbose）", level)),
            }
        }

        // 驗證 max_memory_mb
        if let Some(mem) = self.app_setting.max_memory_mb {
            if mem < 10 {
                errors.push(format!("max_memory_mb 過小（{} MB），不應小於 10 MB", mem));
            }
        }

        // 驗證每個任務
        for (i, task) in self.tasks.iter().enumerate() {
            // 跳過已停用的任務的嚴格局部驗證（但仍檢查路徑）
            if !task.is_enabled() {
                if !task.folder_path.exists() {
                    errors.push(format!("tasks[{}] folder_path 不存在（已停用）: {}", i, task.folder_path.display()));
                }
                continue;
            }

            // 驗證資料夾路徑
            if !task.folder_path.exists() {
                errors.push(format!("tasks[{}] folder_path 不存在: {}", i, task.folder_path.display()));
            } else if !task.folder_path.is_dir() {
                errors.push(format!("tasks[{}] folder_path 不是資料夾: {}", i, task.folder_path.display()));
            }
            
            // 驗證 target glob 模式
            if let Some(targets) = &task.target {
                for (j, pattern) in targets.iter().enumerate() {
                    if let Err(e) = globset::Glob::new(pattern) {
                        errors.push(format!("tasks[{}].target[{}] glob 模式無效: {} - {}", i, j, pattern, e));
                    }
                }
            }

            // 驗證 exclude glob 模式
            if let Some(excludes) = &task.exclude {
                for (j, pattern) in excludes.iter().enumerate() {
                    if let Err(e) = globset::Glob::new(pattern) {
                        errors.push(format!("tasks[{}].exclude[{}] glob 模式無效: {} - {}", i, j, pattern, e));
                    }
                }
            }

            // 驗證 min/max 檔案大小
            if let Some(min_size) = task.min_size_bytes {
                if let Some(max_size) = task.max_size_bytes {
                    if max_size > 0 && min_size > max_size {
                        errors.push(format!("tasks[{}] min_size_bytes ({}) 大於 max_size_bytes ({})", i, min_size, max_size));
                    }
                }
            }

            // 驗證 scan_mode
            if let Some(ref mode) = task.scan_mode {
                match mode.as_str() {
                    "both" | "small_only" | "complete_only" => {}
                    _ => errors.push(format!("tasks[{}].scan_mode 無效: {}（必須為 both/small_only/complete_only）", i, mode)),
                }
            }

            // 驗證 schedule_window 時間格式
            if let Some(ref sw) = task.schedule_window {
                if let Some(ref start) = sw.start {
                    if !is_valid_time_format(start) {
                        errors.push(format!("tasks[{}].schedule_window.start 格式無效: {}（須為 HH:MM）", i, start));
                    }
                }
                if let Some(ref end) = sw.end {
                    if !is_valid_time_format(end) {
                        errors.push(format!("tasks[{}].schedule_window.end 格式無效: {}（須為 HH:MM）", i, end));
                    }
                }
            }
            
            // 驗證閾值
            let threshold_secs: u64 =
                (task.threshold.day as u64).saturating_mul(86400)
                .saturating_add((task.threshold.hour as u64).saturating_mul(3600))
                .saturating_add((task.threshold.minute as u64).saturating_mul(60));

            if threshold_secs == 0 {
                errors.push(format!("tasks[{}] threshold 不能全為 0", i));
            }
            
            // 警告過短的閾值（小於 1 小時）
            if threshold_secs < 3600 {
                errors.push(format!("tasks[{}] threshold 過短（小於 1 小時），可能導致誤刪", i));
            }
        }

        errors
    }
}

/// 輔助：驗證時間格式是否為 HH:MM（24小時制）
fn is_valid_time_format(s: &str) -> bool {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 2 {
        return false;
    }
    if let (Ok(h), Ok(m)) = (parts[0].parse::<u8>(), parts[1].parse::<u8>()) {
        h < 24 && m < 60
    } else {
        false
    }
}
