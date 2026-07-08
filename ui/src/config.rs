use std::fs;
use std::io;
use std::path::Path;
use log::{info, warn, error};
use crate::type_define::Config;

/// 當前支援的設定檔版本
const CURRENT_CONFIG_VERSION: u32 = 2;
#[allow(dead_code)]

/// ### 載入配置文件
///
/// 嘗試讀取配置文件，如果不存在返回None。
/// 自動備份 config.toml → config.toml.backup 並支援版本遷移。
pub fn read_config(path: &Path) -> Option<Config> {
    // 1. 讀取文件內容
    let content: String = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            // 檔案不存在是正常情況，只記錄錯誤
            if e.kind() != io::ErrorKind::NotFound {
                error!("無法讀取配置文件: {}", e);
            } else {
                info!("配置文件不存在: {}", path.display());
            }
            return None;
        }
    };

    // 2. 解析 TOML
    let cfg: Config = match toml::from_str::<Config>(&content) {
        Ok(c) => c,
        Err(e) => {
            error!("配置解析失敗: {}", e);
            // 嘗試從備份恢復
            return try_restore_from_backup(path);
        }
    };

    // 3. 自動備份（每次成功載入都備份）
    backup_config(path, &content);

    // 4. 版本遷移
    let cfg = match migrate_config(cfg) {
        Ok(c) => c,
        Err(e) => {
            error!("配置遷移失敗: {}", e);
            return try_restore_from_backup(path);
        }
    };

    // 5. 校驗
    let errors = cfg.validate();
    if errors.is_empty() {
        info!("配置載入成功 (v{})", cfg.config_version.unwrap_or(1));
        Some(cfg)
    } else {
        for e in &errors {
            error!("配置驗證失敗: {}", e);
        }
        // 校驗失敗時嘗試從備份恢復
        try_restore_from_backup(path)
    }
}

/// ### 配置備份
///
/// 每次成功載入後，自動備份 config.toml → config.toml.backup
fn backup_config(path: &Path, content: &str) {
    let backup_path = path.with_extension("toml.backup");
    match fs::write(&backup_path, content) {
        Ok(_) => info!("配置已備份到: {:?}", backup_path),
        Err(e) => warn!("配置備份失敗: {}", e),
    }
}

/// ### 從備份恢復
///
/// 當配置檔損壞或校驗失敗時，嘗試從備份恢復。
fn try_restore_from_backup(path: &Path) -> Option<Config> {
    let backup_path = path.with_extension("toml.backup");
    if !backup_path.exists() {
        warn!("找不到備份檔案，無法恢復");
        return None;
    }

    info!("嘗試從備份恢復配置: {:?}", backup_path);
    let content: String = fs::read_to_string(&backup_path).ok()?;
    let cfg: Config = toml::from_str::<Config>(&content).ok()?;
    let errors = cfg.validate();
    if errors.is_empty() {
        // 恢復成功，覆蓋損壞的配置
        if let Err(e) = fs::write(path, &content) {
            warn!("恢復配置時寫入失敗: {}", e);
        }
        info!("已從備份恢復配置");
        Some(cfg)
    } else {
        warn!("備份檔案也無效，無法恢復");
        None
    }
}

/// ### 配置版本遷移
///
/// 將舊版配置自動升級到新版格式。
fn migrate_config(mut cfg: Config) -> Result<Config, String> {
    let version = cfg.config_version.unwrap_or(1);

    match version {
        1 => {
            // v1 → v2：新增欄位都為 Option，向後相容，無需轉換
            info!("配置版本 v1 → v2（向後相容，無需轉換）");
            cfg.config_version = Some(2);
            Ok(cfg)
        }
        2 => {
            // 最新版本
            if cfg.config_version.is_none() {
                cfg.config_version = Some(CURRENT_CONFIG_VERSION);
            }
            Ok(cfg)
        }
        v if v > CURRENT_CONFIG_VERSION => {
            // 來自未來版本的配置，嘗試繼續使用
            warn!("配置版本 {} 高於目前支援的版本 {}，可能包含不相容的設定", v, CURRENT_CONFIG_VERSION);
            Ok(cfg)
        }
        _ => {
            Err(format!("不支援的配置版本: {:?}", cfg.config_version))
        }
    }
}

/// ### 讀取配置文件（簡易版，無備份/遷移）
///
/// 用於不需要備份機制的場合。
#[allow(dead_code)]
pub fn read_config_simple(path: &Path) -> Option<Config> {
    let content: String = fs::read_to_string(path)
        .map_err(|e: io::Error| error!("無法讀取配置文件: {}", e))
        .ok()?;

    toml::from_str::<Config>(&content)
        .map_err(|e: toml::de::Error| error!("配置解析失敗: {}", e))
        .ok()
        .and_then(|cfg: Config| {
            let errors = cfg.validate();
            if errors.is_empty() {
                Some(cfg)
            } else {
                for e in &errors {
                    error!("配置驗證失敗: {}", e);
                }
                None
            }
        })
        .inspect(|_| info!("配置載入成功"))
}
