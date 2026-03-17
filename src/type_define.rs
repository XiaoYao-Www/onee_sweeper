use std::path::PathBuf;

use serde::Deserialize;

#[derive(Deserialize, Debug)]
pub struct Config {
    pub app_setting: AppSettings,
    pub tasks: Vec<FolderTask>
}

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

        // 驗證每個任務
        for (i, task) in self.tasks.iter().enumerate() {
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

#[derive(Deserialize, Debug)]
pub struct AppSettings {
    pub small_scan_interval: u64, // 分鐘
    pub complete_scan_interval: u64, // 分鐘
    pub test_mode: Option<bool>,
    pub log_max_size_mb: Option<u64> // 日誌最大大小（MB），超過則清空
}

#[derive(Deserialize, Debug)]
pub struct FolderTask {
    pub folder_path: PathBuf,
    pub target: Option<Vec<String>>, // 目標檔案或資料夾(相對路徑)
    pub really_delete: Option<bool>,
    pub threshold: Threshold
}

#[derive(Deserialize, Debug)]
pub struct Threshold {
    pub day: u8,
    pub hour: u8,
    pub minute: u8
}