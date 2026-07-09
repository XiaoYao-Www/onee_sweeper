use std::fs;
use std::io::{self, Write, Read};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use log::info;

// ─── AuditEntry ──────────────────────────────────────────────────────────────

/// 單筆審計記錄
#[derive(Debug, Clone)]
pub struct AuditEntry {
    /// 操作時間（ISO 8601 格式）
    pub timestamp: String,
    /// 操作類型
    pub operation: String,
    /// 檔案絕對路徑
    pub file_path: PathBuf,
    /// 檔案大小（位元組）
    pub file_size: u64,
    /// Blake3 hash（前 8KB 的摘要，十六進位）
    pub hash: String,
    /// 操作結果
    pub result: String,
}

impl AuditEntry {
    /// 格式化為日誌行
    pub fn to_log_line(&self) -> String {
        format!(
            "[{}] [{}] [{}] [{}] [{}] [{}]\n",
            self.timestamp,
            self.operation,
            self.file_path.display(),
            self.file_size,
            self.hash,
            self.result,
        )
    }
}

// ─── AuditLog ────────────────────────────────────────────────────────────────

/// 審計日誌 — append-only 寫入
///
/// 日誌檔案無須載入記憶體，每次新增記錄直接 append。
pub struct AuditLog {
    /// 日誌檔案路徑
    path: PathBuf,
    /// 最大檔案大小（MB），超過則歸零（重新開始）
    max_size_mb: u64,
}

impl AuditLog {
    /// 建立審計日誌
    ///
    /// - log_dir: 日誌目錄
    /// - max_size_mb: 最大大小（MB），0 = 不限制
    pub fn new(log_dir: &Path, max_size_mb: u64) -> Self {
        let path = log_dir.join("delete_audit.log");
        Self { path, max_size_mb }
    }

    /// ### 寫入一筆審計記錄（append-only）
    pub fn append(&self, entry: &AuditEntry) -> io::Result<()> {
        // 檢查日誌大小，避免無限增長
        if self.max_size_mb > 0 {
            self.rotate_if_needed()?;
        }

        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;

        file.write_all(entry.to_log_line().as_bytes())?;
        file.flush()?;

        Ok(())
    }

    /// 如果日誌檔案超過大小限制，歸零（更名舊檔）
    fn rotate_if_needed(&self) -> io::Result<()> {
        if !self.path.exists() {
            return Ok(());
        }

        let metadata = fs::metadata(&self.path)?;
        let max_bytes = self.max_size_mb.saturating_mul(1024 * 1024);

        if metadata.len() > max_bytes {
            // 更名舊日誌
            let backup_path = self.path.with_extension("log.old");
            let _ = fs::rename(&self.path, &backup_path);
            info!("審計日誌已達上限，歸零備份: {:?}", backup_path);
        }

        Ok(())
    }

}

// ─── 輔助函數 ────────────────────────────────────────────────────────────────

/// 計算檔案前 8KB 的 blake3 hash（十六進位字串）
pub fn compute_file_hash(path: &Path) -> io::Result<String> {
    let file = fs::File::open(path)?;
    let mut reader = io::BufReader::with_capacity(8192, file);
    let mut hasher = blake3::Hasher::new();

    // 只讀取前 8KB
    let mut buffer = [0u8; 8192];
    let n = reader.read(&mut buffer)?;
    hasher.update(&buffer[..n]);

    Ok(hasher.finalize().to_hex().to_string())
}

/// 取得目前時間的 ISO 8601 字串
pub fn current_timestamp() -> String {
    match systemtime_to_rfc3339() {
        Some(ts) => ts,
        None => {
            // fallback：使用自紀元秒數
            format!(
                "{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            )
        }
    }
}

/// 將 SystemTime 轉為 RFC 3339 / ISO 8601 格式
fn systemtime_to_rfc3339() -> Option<String> {
    let duration = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?;

    let secs = duration.as_secs();

    // 簡單的 UTC 時間計算
    const SECS_PER_MIN: u64 = 60;
    const SECS_PER_HOUR: u64 = 3600;
    const SECS_PER_DAY: u64 = 86400;

    // 計算日期（從 1970-01-01 開始）
    let days = secs / SECS_PER_DAY;
    let time_secs = secs % SECS_PER_DAY;

    let hours = time_secs / SECS_PER_HOUR;
    let mins = (time_secs % SECS_PER_HOUR) / SECS_PER_MIN;
    let secs_remain = time_secs % SECS_PER_MIN;

    // 使用簡單的格里曆轉換
    let (year, month, day) = days_to_date(days);

    Some(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, mins, secs_remain
    ))
}

/// 將自紀元天數轉換為 (年, 月, 日)
fn days_to_date(days: u64) -> (u64, u64, u64) {
    // 從 Unix 紀元 (1970-01-01) 開始
    let mut y = 1970i64;
    let mut d = days as i64;

    loop {
        let days_in_year = if is_leap(y) { 366 } else { 365 };
        if d < days_in_year {
            break;
        }
        d -= days_in_year;
        y += 1;
    }

    let leap = is_leap(y);
    let months_days = if leap {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut m = 0u64;
    for (i, &md) in months_days.iter().enumerate() {
        if d < md {
            m = (i + 1) as u64;
            break;
        }
        d -= md;
    }

    (y as u64, m, (d + 1) as u64) // day is 1-based
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_empty_file() {
        let dir = std::env::temp_dir().join("onee_sweeper_test_hash");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("empty.test");
        fs::write(&path, b"").unwrap();

        let hash = compute_file_hash(&path).unwrap();
        // blake3 的空檔案的已知 hash
        assert_eq!(hash.len(), 64);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_timestamp_format() {
        let ts = current_timestamp();
        // 應匹配 ISO 8601 基本格式：YYYY-MM-DDTHH:MM:SSZ
        assert!(ts.len() >= 20);
        assert!(ts.contains('T'));
        assert!(ts.ends_with('Z'));
    }

    #[test]
    fn test_audit_entry_format() {
        let entry = AuditEntry {
            timestamp: "2025-01-15T10:30:00Z".to_string(),
            operation: "DELETE".to_string(),
            file_path: PathBuf::from("C:/test/file.txt"),
            file_size: 1024,
            hash: "abc123".to_string(),
            result: "SUCCESS".to_string(),
        };
        let line = entry.to_log_line();
        assert!(line.contains("2025-01-15T10:30:00Z"));
        assert!(line.contains("DELETE"));
        assert!(line.contains("C:/test/file.txt"));
        assert!(line.contains("1024"));
        assert!(line.contains("abc123"));
        assert!(line.contains("SUCCESS"));
    }
}
