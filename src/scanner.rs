use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{ Path, PathBuf };
use log::{ info, error };
use rkyv::{ Archive, Deserialize, Serialize };
use rkyv::rancor::Error as RkyvError;

// 路徑以 Vec<u8> 儲存：跨平台（Windows WTFH-8 / Linux 原始 bytes），且可被 rkyv 序列化。
// 轉換輔助：PathBuf <-> Vec<u8>

#[cfg(windows)]
fn path_to_bytes(p: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    // 將 Windows OsStr 的 UTF-16 wide chars 轉為 little-endian bytes
    p.as_os_str()
        .encode_wide()
        .flat_map(|w| w.to_le_bytes())
        .collect()
}

#[cfg(not(windows))]
fn path_to_bytes(p: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    p.as_os_str().as_bytes().to_vec()
}

#[cfg(windows)]
pub fn bytes_to_path(b: &[u8]) -> PathBuf {
    use std::os::windows::ffi::OsStringExt;
    use std::ffi::OsString;
    let wide: Vec<u16> = b
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    PathBuf::from(OsString::from_wide(&wide))
}

#[cfg(not(windows))]
pub fn bytes_to_path(b: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    use std::ffi::OsStr;
    PathBuf::from(OsStr::from_bytes(b))
}

// ─── ScanFolderData ──────────────────────────────────────────────────────────

/// 單個目標任務資料夾的掃描資料。
#[derive(Archive, Deserialize, Serialize, Debug)]
pub struct ScanFolderData {
    /// 此任務資料夾的絕對路徑（bytes，跨平台）
    pub folder_path: Vec<u8>,
    /// 所有子項目：相對路徑 bytes -> last_modified
    pub entries: HashMap<Vec<u8>, u64>,
}

impl ScanFolderData {
    pub fn new(folder_path: &Path) -> Self {
        Self {
            folder_path: path_to_bytes(folder_path),
            entries: HashMap::new(),
        }
    }

    /// ### 取得目標資料夾的 PathBuf。
    pub fn folder_path_buf(&self) -> PathBuf {
        bytes_to_path(&self.folder_path)
    }

    /// ### 插入路徑與最後修改時間。
    /// 
    /// 如果存在會取代。
    pub fn upsert(&mut self, relative_path: &Path, last_modified: u64) {
        self.entries.insert(path_to_bytes(relative_path), last_modified);
    }

    /// ### 取得路徑的最後修改時間。
    pub fn get(&self, relative_path: &Path) -> Option<u64> {
        self.entries.get(&path_to_bytes(relative_path)).copied()
    }

    /// ### 移除路徑的最後修改時間。
    pub fn remove(&mut self, relative_path: &Path) -> Option<u64> {
        self.entries.remove(&path_to_bytes(relative_path))
    }

    /// ### 取得所有舊於等於目標日期的項目（含等於）
    pub fn older_than(&self, threshold: u64) -> impl Iterator<Item = (&Vec<u8>, &u64)> {
        self.entries.iter().filter(move |(_, &t)| t <= threshold)
    }
}

// ─── ScanDatabase ────────────────────────────────────────────────────────────

/// 主要序列化結構，儲存多個目標資料夾的 ScanFolderData。
#[derive(Archive, Deserialize, Serialize, Debug)]
pub struct ScanDatabase {
    // 資料夾絕對路徑 -> 任務資料夾結構
    pub folders: HashMap<Vec<u8>, ScanFolderData>,
}

impl ScanDatabase {
    pub fn new() -> Self {
        Self { folders: HashMap::new() }
    }

    // ── 載入 / 儲存 ──────────────────────────────────────────────────────────

    /// ### 從指定文件讀取
    ///
    /// 從指定文件讀取 ScanDatabase，可以抉擇是否允許創建新的資料庫。
    ///
    /// - path 路徑
    /// - allow_create 是否允許創建
    pub fn load_from_file(path: &Path, allow_create: bool) -> io::Result<Self> {
        if !path.exists() {
            info!("temp.bin 不存在，創建新資料庫");
            return Ok(Self::new());
        }
        let bytes: Vec<u8> = fs::read(path)?;
        if bytes.is_empty() {
            info!("temp.bin 為空，創建新資料庫");
            return Ok(Self::new());
        };
        // 將 rkyv 錯誤轉換為 io::Result
        rkyv::from_bytes::<ScanDatabase, RkyvError>(&bytes)
            .map(|db: ScanDatabase| {
                let total_entries: usize = db.folders
                    .values()
                    .map(|f| f.entries.len())
                    .sum();
                info!("成功載入 {} 個目標資料夾， {} 個路徑紀錄", db.folders.len(), total_entries);
                db
            })
            .or_else(|e: RkyvError| {
                // 如果解析失敗
                if allow_create {
                    error!("載入 temp.bin 失敗 ({})，創建新資料庫", e);
                    // 備份損壞的文件
                    if let Some(parent) = path.parent() {
                        let backup_path = parent.join("temp.bin.corrupted");
                        let _ = fs::rename(path, &backup_path);
                        info!("已備份損壞的資料庫到: {:?}", backup_path);
                    }
                    Ok(Self::new())
                } else {
                    error!("載入 temp.bin 失敗: {}", e);
                    Err(io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
                }
            })
    }

    /// 儲存到 temp.bin（使用原子寫入避免資料損毀）
    pub fn save_to_file(&self, path: &Path) -> io::Result<()> {
        let bytes: rkyv::util::AlignedVec = rkyv
            ::to_bytes::<RkyvError>(self)
            .map_err(|e: RkyvError|
                io::Error::new(io::ErrorKind::Other, format!("序列化失敗: {:?}", e))
            )?;

        // 原子寫入：先寫入暫存檔，再重新命名，避免寫入中途崩潰導致資料損毀
        let tmp_path = path.with_extension("tmp");
        fs::write(&tmp_path, bytes.as_ref())?;
        fs::rename(&tmp_path, path)?;

        let total_entries: usize = self.folders
            .values()
            .map(|f: &ScanFolderData| f.entries.len())
            .sum();
        info!(
            "已儲存 {} 個目標資料夾，共 {} 個路徑紀錄到 temp.bin",
            self.folders.len(),
            total_entries
        );
        Ok(())
    }

    // ── 資料夾層級操作 ───────────────────────────────────────────────────────

    /// ### 取得目標資料夾(自動創建)
    pub fn get_or_create_folder(&mut self, folder_path: &Path) -> &mut ScanFolderData {
        let key: Vec<u8> = path_to_bytes(folder_path);
        self.folders.entry(key).or_insert_with(|| ScanFolderData::new(folder_path))
    }

    /// ### 取得目標資料夾
    pub fn get_folder(&self, folder_path: &Path) -> Option<&ScanFolderData> {
        self.folders.get(&path_to_bytes(folder_path))
    }

    /// ### 取得目標資料夾的可變引用
    pub fn get_folder_mut(&mut self, folder_path: &Path) -> Option<&mut ScanFolderData> {
        self.folders.get_mut(&path_to_bytes(folder_path))
    }

    /// ### 移除目標資料夾
    pub fn remove_folder(&mut self, folder_path: &Path) -> Option<ScanFolderData> {
        self.folders.remove(&path_to_bytes(folder_path))
    }

    // ── 項目層級操作 ─────────────────────────────────────────────────────────

    /// ### 在資料夾插入路徑
    /// 
    /// 如果資料夾不存在，會創建。
    /// 如果路徑存在，會取代。
    pub fn upsert(&mut self, folder_path: &Path, relative_path: &Path, last_modified: u64) {
        self.get_or_create_folder(folder_path).upsert(relative_path, last_modified);
    }

    /// ### 取得資料夾的路徑
    pub fn get(&self, folder_path: &Path, relative_path: &Path) -> Option<u64> {
        self.get_folder(folder_path)?.get(relative_path)
    }

    /// ### 移除資料夾的路徑
    pub fn remove(&mut self, folder_path: &Path, relative_path: &Path) -> Option<u64> {
        self.get_folder_mut(folder_path)?.remove(relative_path)
    }

    // ── 查詢 ─────────────────────────────────────────────────────────────────

    /// ### 查詢資料夾中舊於等於某時間的路徑
    pub fn entries_older_than<'a>(
        &'a self,
        folder_path: &Path,
        threshold: u64
    ) -> impl Iterator<Item = (&'a Vec<u8>, &'a u64)> {
        self.get_folder(folder_path)
            .into_iter()
            .flat_map(move |f| f.older_than(threshold))
    }

    /// ### 清理不存在的路徑記錄
    ///
    /// 遍歷資料庫，移除檔案系統中已不存在的路徑，防止資料庫無限增長
    /// 優化：批量處理，減少內存分配
    pub fn cleanup_nonexistent_entries(&mut self) {
        let mut total_removed = 0;
        let mut empty_folders = Vec::new();
        
        for (folder_key, folder_data) in self.folders.iter_mut() {
            let folder_path = bytes_to_path(&folder_data.folder_path);
            
            // 檢查資料夾本身是否存在
            if !folder_path.exists() {
                empty_folders.push(folder_key.clone());
                total_removed += folder_data.entries.len();
                continue;
            }
            
            // 批量收集要刪除的路徑
            let to_remove: Vec<Vec<u8>> = folder_data.entries
                .iter()
                .filter_map(|(path_bytes, _)| {
                    let relative_path = bytes_to_path(path_bytes);
                    let full_path = folder_path.join(&relative_path);
                    
                    if !full_path.exists() {
                        Some(path_bytes.clone())
                    } else {
                        None
                    }
                })
                .collect();
            
            // 批量刪除
            for path_bytes in to_remove {
                folder_data.entries.remove(&path_bytes);
                total_removed += 1;
            }
        }
        
        // 移除空的資料夾記錄
        for folder_key in empty_folders {
            self.folders.remove(&folder_key);
        }
        
        if total_removed > 0 {
            info!("清理了 {} 個不存在的路徑記錄", total_removed);
        }
    }

    /// ### 獲取資料庫統計信息
    pub fn get_stats(&self) -> (usize, usize) {
        let folder_count = self.folders.len();
        let entry_count: usize = self.folders.values().map(|f| f.entries.len()).sum();
        (folder_count, entry_count)
    }
}
