use std::fs;
use std::io;
use std::path::Path;
use log::{info, error};
use crate::type_define::Config;

/// ### 載入配置文件
///
/// 嘗試讀取配置文件,如果不存在返回None。
pub fn read_config(path: &Path) -> Option<Config> {
    // 讀取文件並處理錯誤日誌
    let content: String = fs
        ::read_to_string(path)
        .map_err(|e: io::Error| error!("無法讀取配置文件: {}", e))
        .ok()?;

    // 解析 TOML 並處理錯誤日誌
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
