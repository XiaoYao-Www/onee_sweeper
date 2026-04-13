#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod type_define;
mod scanner;
mod config;

use log::{ info, warn, error, debug };
use simplelog::*;
use winit::{
    application::ApplicationHandler,
    event::{ self, WindowEvent },
    event_loop::{ ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy },
};
use tray_icon::{
    Icon,
    TrayIcon,
    TrayIconBuilder,
    menu::{ Menu, MenuItem, MenuEvent, PredefinedMenuItem },
};
use image::{ ImageBuffer, Rgba };
use std::{
    fs::{ self, File },
    path::{ Path, PathBuf },
    io::{ self },
    env,
    time::{ Instant, Duration, SystemTime, UNIX_EPOCH },
    thread,
    collections::HashSet,
};
use globset::{ Glob, GlobSetBuilder };
use mslnk::ShellLink;
use notify_rust::Notification;
use notify::{ RecommendedWatcher, RecursiveMode, Watcher, EventKind };
use crossbeam_channel::{ unbounded, select };

use type_define::Config;

use crate::type_define::AppSettings;

const CONFIG_TOML_PATH: &str = "config.toml";
const LOG_FILE_NAME: &str = "run.log";
const TEMP_BIN_PATH: &str = "temp.bin";

// 檔案系統監控事件
enum FileEvent {
    ConfigChanged, // 配置文件變更
    FileChanged(PathBuf), // 檔案變更 ( 非配置文件 )
}

// 監控系統命令
enum WatchCommand {
    Watch(PathBuf), // 監控路徑
    Unwatch(PathBuf), // 取消監控路徑
    UnwatchAll, // 取消所有監控路徑
    ReplaceAll(Vec<PathBuf>), // 替換所有路徑
    Stop, // 停止監控
}

fn start_watcher(proxy: EventLoopProxy<FileEvent>) -> crossbeam_channel::Sender<WatchCommand> {
    let (cmd_tx, cmd_rx) = unbounded::<WatchCommand>();

    thread::spawn(move || {
        let (event_tx, event_rx) = unbounded();

        let mut watcher: notify::ReadDirectoryChangesWatcher = RecommendedWatcher::new(
            move |res: Result<notify::Event, notify::Error>| {
                let _ = event_tx.send(res);
            },
            notify::Config::default()
        ).unwrap();

        let mut watched_paths: HashSet<PathBuf> = HashSet::new();

        loop {
            select! {
                // 🟢 notify event
                recv(event_rx) -> res => {
                    if let Ok(Ok(event)) = res {
                        match event.kind {
                            EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_) => {
                                for path in event.paths {
                                    let _ = proxy.send_event(
                                        FileEvent::FileChanged(path)
                                    );
                                }
                            }
                            _ => {}
                        }
                    }
                }

                // 🔵 command
                recv(cmd_rx) -> cmd => {
                    match cmd {
                        Ok(WatchCommand::Watch(path)) => {
                            let _ = watcher.watch(&path, RecursiveMode::Recursive);
                            watched_paths.insert(path);
                        }
                        Ok(WatchCommand::Unwatch(path)) => {
                            let _ = watcher.unwatch(&path);
                            watched_paths.remove(&path);
                        }
                        Ok(WatchCommand::Stop) | Err(_) => break,
                        Ok(WatchCommand::UnwatchAll) => {
                            for path in watched_paths.drain() {
                                let _ = watcher.unwatch(&path);
                            }
                        },
                        Ok(WatchCommand::ReplaceAll(paths)) => {
                            for old_path in watched_paths.drain() {
                                let _ = watcher.unwatch(&old_path);
                            }
                            for new_path in paths {
                                let _ = watcher.watch(&new_path, RecursiveMode::Recursive);
                                watched_paths.insert(new_path);
                            }
                        },
                    }
                }
            }
        }
    });

    cmd_tx
}

/// ### 初始化日志系統
///
/// 設置 simplelog 日誌系統，將日誌輸出到 run.log 文件和控制台
fn init_logging() -> io::Result<()> {
    let log_path: PathBuf = get_file_path(LOG_FILE_NAME)?;

    let log_file: File = File::create(log_path)?;

    let _ = CombinedLogger::init(
        vec![
            TermLogger::new(
                LevelFilter::Debug,
                simplelog::Config::default(),
                TerminalMode::Mixed,
                ColorChoice::Auto
            ),
            WriteLogger::new(LevelFilter::Info, simplelog::Config::default(), log_file)
        ]
    ).map_err(|e: log::SetLoggerError|
        io::Error::new(io::ErrorKind::Other, format!("創建日誌文件失敗: {}", e))
    );

    Ok(())
}
/// ### 清理舊日誌
///
/// 當日誌文件超過指定大小時清空內容
fn cleanup_old_logs(max_size_mb: u64) -> io::Result<()> {
    let log_path: PathBuf = get_file_path(LOG_FILE_NAME)?;

    if log_path.exists() {
        if let Ok(metadata) = fs::metadata(&log_path) {
            let size_mb: u64 = metadata.len() / (1024 * 1024);
            if size_mb > max_size_mb {
                // 備份舊日誌
                let backup_path = log_path.with_extension("log.old");
                if backup_path.exists() {
                    let _ = fs::remove_file(&backup_path);
                }
                let _ = fs::rename(&log_path, &backup_path);

                // 創建新日誌文件
                File::create(&log_path)?;
                info!("已清理日誌文件 (大小: {} MB)，舊日誌已備份", size_mb);
            }
        }
    }
    Ok(())
}

/// ### 獲取檔案位置
///
/// 取得基於當前執行檔 (.exe) 的檔案位置。
///
/// - file_path 相對位置
fn get_file_path(file_path: &str) -> io::Result<PathBuf> {
    // 獲取當前執行檔 (.exe) 的完整路徑
    let mut path: PathBuf = env::current_exe()?;

    // 移除檔名，只保留資料夾路徑
    path.pop();

    // 加入你的目標檔名
    path.push(file_path);
    Ok(path)
}

/// ### 載入圖標
///
/// - raba_bytes 圖標的二進制字節串
fn load_icon(rgba_bytes: &[u8]) -> Result<Icon, Box<dyn std::error::Error>> {
    // 從記憶體載入圖片數據
    let image: ImageBuffer<Rgba<u8>, Vec<u8>> = image::load_from_memory(rgba_bytes)?.into_rgba8();

    let (width, height): (u32, u32) = image.dimensions();
    let rgba: Vec<u8> = image.into_raw();

    Ok(Icon::from_rgba(rgba, width, height)?)
}

/// ### 打開日誌文件
///
/// 使用系統預設編輯器打開日誌文件
fn open_log_file() -> Result<(), Box<dyn std::error::Error>> {
    let exe_path = env::current_exe()?;
    let exe_dir = exe_path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "無法取得執行檔目錄"))?;
    let log_path = exe_dir.join(LOG_FILE_NAME);

    if log_path.exists() {
        edit::edit_file(log_path)?;
    } else {
        warn!("找不到日誌檔案");
    }
    Ok(())
}

/// ### 打開配置文件
///
/// 打開 toml 配置文件，如果不存在就創建一個。
///
/// - path 指定路徑
fn open_or_create_toml(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let path: &Path = &get_file_path(path)?;

    // 如果檔案不存在，先創建它
    if !path.exists() {
        info!("檔案不存在，正在創建預設 TOML...");
        File::create(path)?;
    }

    // 使用系統預設編輯器開啟
    info!("正在開啟編輯器: {}", path.display());
    edit::edit_file(path)?;

    Ok(())
}

/// ### 載入配置文件
///
/// 嘗試讀取配置文件,如果不存在返回None。
fn read_config() -> Option<Config> {
    let path: PathBuf = get_file_path(CONFIG_TOML_PATH).ok()?;
    config::read_config(&path)
}

/// ### 創建開機啟動
///
/// 創建開機啟動連結。
fn create_startup_link() -> io::Result<()> {
    let exe_path: PathBuf = env::current_exe()?; // 獲取執行檔的路徑

    // 獲取開機啟動目錄
    let mut startup_path: PathBuf = PathBuf::from(
        env::var("APPDATA").map_err(|e: env::VarError| io::Error::new(io::ErrorKind::NotFound, e))?
    );
    startup_path.push(r"Microsoft\Windows\Start Menu\Programs\Startup");

    let link_path: PathBuf = startup_path.join("Onee Sweeper.lnk"); // 創建啟動連結名稱

    let sl: ShellLink = ShellLink::new(&exe_path).map_err(|e: mslnk::MSLinkError|
        io::Error::new(io::ErrorKind::NotFound, e)
    )?;
    sl
        .create_lnk(link_path)
        .map_err(|e: mslnk::MSLinkError| io::Error::new(io::ErrorKind::NotFound, e))?;

    Ok(())
}

/// ### 移除開機啟動
///
/// 刪除位於啟動資料夾中的快捷方式。
///
/// 回傳是否有移除連結
fn remove_startup_link() -> io::Result<bool> {
    // 1. 獲取開機啟動目錄
    let mut startup_path: PathBuf = PathBuf::from(
        env::var("APPDATA").map_err(|e: env::VarError| io::Error::new(io::ErrorKind::NotFound, e))?
    );
    startup_path.push(r"Microsoft\Windows\Start Menu\Programs\Startup");

    // 2. 指定要刪除的連結名稱 (需與創建時一致)
    let link_path: PathBuf = startup_path.join("Onee Sweeper.lnk");

    // 3. 檢查檔案是否存在並執行刪除
    if link_path.exists() {
        fs::remove_file(link_path)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// ### 應用程序結構
struct App {
    proxy: EventLoopProxy<FileEvent>, // 文件事件代理
    watcher_cmd: crossbeam_channel::Sender<WatchCommand>, // 監控命令發射器
    pending_paths: HashSet<PathBuf>, // 等待處理路徑
    last_process_watcher_path: Instant, // 最後一次處理監控路徑的時間戳

    tray_icon: Option<TrayIcon>, // 圖標
    open_config: MenuItem, // 打開配置
    open_log: MenuItem, // 打開日誌
    refresh_config: MenuItem, // 刷新配置
    creat_startup_link: MenuItem, // 創建開機啟動連結
    remove_startup_link: MenuItem, // 移除開機啟動連結
    quit_item: MenuItem, // 退出選項

    config: Option<Config>, // 配置文件
    last_small_scan: Instant, // 上次小掃描時間
    last_complete_scan: Instant, // 上次完整掃描時間
}

// ########## 應用功能 ##########
impl App {
    /// ### 切換小圖示圖標
    ///
    /// 根據狀態切換小圖示圖標，有合法配置時視為執行
    fn change_icon(&mut self) {
        if let Some(tray) = self.tray_icon.as_mut() {
            let icon_result = if self.config.is_some() {
                load_icon(include_bytes!("../assets/icon_run.ico"))
            } else {
                load_icon(include_bytes!("../assets/icon_stop.ico"))
            };

            match icon_result {
                Ok(icon) => {
                    let _ = tray.set_icon(Some(icon));
                }
                Err(e) => error!("載入圖標失敗: {}", e),
            }
        }
    }

    /// ### 重新載入配置
    fn reload_config(&mut self) {
        let new_config: Option<Config> = read_config();

        // 只有成功載入新配置才更新
        match new_config {
            Some(cfg) => {
                // 刷新任務計時
                let now_instant: Instant = Instant::now();
                self.last_complete_scan = now_instant;
                self.last_small_scan = now_instant;

                // 註冊檔案監測
                self.watcher_cmd
                    .send(
                        WatchCommand::ReplaceAll(
                            cfg.tasks
                                .iter()
                                .map(|t: &type_define::FolderTask| t.folder_path.clone())
                                .collect()
                        )
                    )
                    .unwrap();

                self.config = Some(cfg);

                info!("配置更新成功");
                Notification::new()
                    .appname("ONEE SWEEPER")
                    .summary("更新成功")
                    .body("配置更新成功。")
                    .timeout(5000)
                    .show()
                    .unwrap();
            }
            None => {
                warn!("配置更新失敗，保持原有配置");
                // 不更新 self.config，保持原有配置繼續運行
                if self.config.is_none() {
                    Notification::new()
                        .appname("ONEE SWEEPER")
                        .summary("更新失敗")
                        .body("配置更新失敗，程序未運行，詳情請查閱日誌文件。")
                        .timeout(5000)
                        .show()
                        .unwrap();
                } else {
                    Notification::new()
                        .appname("ONEE SWEEPER")
                        .summary("更新失敗")
                        .body("配置更新失敗，保持原有配置，詳情請查閱日誌文件。")
                        .timeout(5000)
                        .show()
                        .unwrap();
                }
            }
        }

        self.change_icon();
    }

    /// ### 執行掃描
    ///
    /// 根據 is_complete 參數決定執行完整掃描還是快速掃描，並且會抓取刪除目標
    ///
    /// - is_complete 是否執行完整掃描
    fn perform_scan(&self, is_complete: bool) {
        let label: &str = if is_complete { "完整掃描" } else { "快速掃描" };
        info!("====================");
        info!("正在執行: {}", label);

        if let Some(cfg) = &self.config {
            // 載入資料庫
            let temp_bin_path: PathBuf = match get_file_path(TEMP_BIN_PATH) {
                Ok(p) => p,
                Err(e) => {
                    error!("解析暫存路徑失敗: {}", e);
                    return;
                }
            };
            let mut db: scanner::ScanDatabase = match
                scanner::ScanDatabase::load_from_file(&temp_bin_path, true)
            {
                Ok(db) => db,
                Err(e) => {
                    error!("載入資料庫失敗: {}", e);
                    return;
                }
            };

            let mut task_errors = 0;
            let mut task_success = 0;

            for task in &cfg.tasks {
                info!("檢查資料夾: {}", task.folder_path.to_string_lossy());

                if !task.folder_path.exists() {
                    warn!("  路徑不存在，跳過任務");
                    task_errors += 1;
                    continue;
                }

                if !task.folder_path.is_dir() {
                    warn!("  不是資料夾，跳過任務");
                    task_errors += 1;
                    continue;
                }

                // 計算閾值時間（秒）
                let threshold_secs: u64 = (task.threshold.day as u64)
                    .saturating_mul(86400)
                    .saturating_add((task.threshold.hour as u64).saturating_mul(3600))
                    .saturating_add((task.threshold.minute as u64).saturating_mul(60));

                if threshold_secs == 0 {
                    warn!("  閾值為 0，跳過任務（避免刪除所有檔案）");
                    task_errors += 1;
                    continue;
                }

                let result = if is_complete {
                    // 大掃描：真實掃描資料夾
                    self.perform_complete_scan(
                        &mut db,
                        &task.folder_path, // 絕對路徑
                        task.target.as_ref(),
                        threshold_secs,
                        task.really_delete.unwrap_or(false),
                        cfg.app_setting.test_mode.unwrap_or(false)
                    )
                } else {
                    // 小掃描：僅讀取記錄判斷
                    self.perform_small_scan(
                        &mut db,
                        &task.folder_path,
                        task.target.as_ref(),
                        threshold_secs,
                        task.really_delete.unwrap_or(false),
                        cfg.app_setting.test_mode.unwrap_or(false)
                    )
                };

                match result {
                    Ok(_) => {
                        task_success += 1;
                    }
                    Err(e) => {
                        error!("  掃描失敗: {}", e);
                        task_errors += 1;
                    }
                }
            }

            info!("任務結果: 成功 {} 個，失敗 {} 個", task_success, task_errors);

            // 定期清理不存在的條目（每次完整掃描後）
            if is_complete {
                db.cleanup_nonexistent_entries();
            }

            // 儲存資料庫
            if let Err(e) = db.save_to_file(&temp_bin_path) {
                error!("儲存資料庫失敗: {}", e);
            }

            info!("====================");
        }
    }

    /// ### 大掃描：真實掃描資料夾
    ///
    /// 執行大掃描，會實際掃盤。
    ///
    /// - db 資料庫
    /// - folder_path 資料夾路徑(絕對路徑)
    /// - target 塞選目標
    /// - threshold_secs 閾值時間（秒）
    /// - really_delete 是否測底刪除
    /// - test_mode 是否測試模式
    fn perform_complete_scan(
        &self,
        db: &mut scanner::ScanDatabase,
        folder_path: &Path,
        target: Option<&Vec<String>>,
        threshold_secs: u64,
        really_delete: bool,
        test_mode: bool
    ) -> io::Result<()> {
        let now: u64 = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_secs(),
            Err(e) => {
                error!("系統時間錯誤: {}", e);
                return Err(io::Error::new(io::ErrorKind::Other, "系統時間錯誤"));
            }
        };

        let mut to_delete: Vec<PathBuf> = Vec::new(); // 絕對路徑

        // 創建匹配器
        let set: Option<globset::GlobSet> = if let Some(some_target) = target {
            let mut builder: GlobSetBuilder = GlobSetBuilder::new();
            for p in some_target {
                builder.add(
                    Glob::new(&p).map_err(|e: globset::Error|
                        io::Error::new(io::ErrorKind::Other, e)
                    )?
                );
            }
            Some(
                builder
                    .build()
                    .map_err(|e: globset::Error| io::Error::new(io::ErrorKind::Other, e))?
            )
        } else {
            None
        };

        // 遞迴掃描資料夾
        self.scan_directory_recursive(
            folder_path,
            folder_path,
            &set,
            false,
            threshold_secs,
            now,
            db,
            &mut to_delete
        )?;

        // 執行刪除（優化後）
        if !to_delete.is_empty() {
            let optimized: Vec<PathBuf> = self.optimize_delete_paths(&to_delete, folder_path, db);
            self.execute_deletions(&optimized, folder_path, really_delete, test_mode, db);
        }

        Ok(())
    }

    /// ### 遞迴掃描目錄
    ///
    /// - path 目標掃描目錄(絕對路徑)
    /// - root 根目錄(任務目錄)
    /// - target 目標(可選)
    /// - in_target_folder 是否在目標目錄中
    /// - threshold_secs 閥值(秒)
    /// - now 現在時間(秒)
    /// - db 資料庫
    /// - to_delete 要刪除的目錄列表(絕對路徑)
    fn scan_directory_recursive(
        &self,
        path: &Path,
        root: &Path,
        target: &Option<globset::GlobSet>,
        in_target_folder: bool,
        threshold_secs: u64,
        now: u64,
        db: &mut scanner::ScanDatabase,
        to_delete: &mut Vec<PathBuf>
    ) -> io::Result<()> {
        let entries: fs::ReadDir = fs::read_dir(path)?;
        let mut max_child_modified: u64 = 0u64; // 追蹤子項目的最大修改時間

        for entry in entries {
            /*
                基本資訊取得
             */
            let entry: fs::DirEntry = entry?; // 處理errors
            let entry_path: PathBuf = entry.path(); // 取得路徑(絕對)
            let rela_path: PathBuf = // 取得相對路徑
                entry_path
                    .strip_prefix(root)
                    .map_err(|e: std::path::StripPrefixError|
                        io::Error::new(io::ErrorKind::Other, e.to_string())
                    )?
                    .to_path_buf();
            let metadata: fs::Metadata = entry.metadata()?; // 取得原數據

            /*
                匹配驗證 - 修正：確保有target時只刪除匹配的項目
             */
            let is_match: bool = match target {
                // 沒有target：所有項目都匹配
                None => true,
                // 有target：必須匹配pattern或在已匹配的父資料夾內
                Some(match_set) => {
                    // 檢查當前路徑是否匹配任何pattern
                    let path_matches = match_set.is_match(&rela_path);
                    // 或者父資料夾已經匹配
                    path_matches || in_target_folder
                }
            };

            /*
                掃描開始
             */

            if metadata.is_dir() {
                // 關鍵修正：不掃描和刪除任務根目錄
                if entry_path == root {
                    warn!("  警告：跳過任務根目錄: {}", root.display());
                    continue;
                }

                // 資料夾：先遞迴掃描子目錄
                self.scan_directory_recursive(
                    &entry_path,
                    root,
                    target,
                    is_match, // 如果當前資料夾匹配，子項目都在目標內
                    threshold_secs,
                    now,
                    db,
                    to_delete
                )?;

                if !is_match {
                    // 不匹配 => 跳過處理
                    continue;
                }

                // 檢查資料夾記錄
                let recorded_time: u64 = if let Some(recorded) = db.get(root, &rela_path) {
                    recorded
                } else {
                    // 第一次發現，記錄當前時間
                    db.upsert(root, &rela_path, now);
                    now
                };

                // 更新父資料夾追蹤
                if recorded_time > max_child_modified {
                    max_child_modified = recorded_time;
                }

                // 判斷資料夾是否超過閾值
                if let Some(age) = now.checked_sub(recorded_time) {
                    if age >= threshold_secs {
                        to_delete.push(entry_path);
                    }
                } else {
                    warn!("  資料夾時間計算溢出，跳過: {}", rela_path.display());
                }

                continue; // 跳過檔案處理邏輯
            }

            if !is_match {
                // 文件不匹配 => 跳過
                continue;
            }

            // 處理匹配的文件
            if let Ok(modified) = metadata.modified() {
                let file_modified: u64 = match modified.duration_since(UNIX_EPOCH) {
                    Ok(d) => d.as_secs(),
                    Err(_) => {
                        warn!("  檔案時間異常，跳過: {}", rela_path.display());
                        continue;
                    }
                };

                // 檢查資料庫記錄
                let recorded_time: u64 = if let Some(recorded) = db.get(root, &rela_path) {
                    // 已有記錄：比較時間
                    if file_modified > recorded {
                        // 檔案更新了，更新記錄
                        db.upsert(root, &rela_path, file_modified);
                        file_modified
                    } else {
                        // 使用記錄時間
                        recorded
                    }
                } else {
                    // 第一次發現，記錄當前時間
                    db.upsert(root, &rela_path, now);
                    now
                };

                // 更新父資料夾追蹤
                if recorded_time > max_child_modified {
                    max_child_modified = recorded_time;
                }

                // 判斷是否超過閾值
                if let Some(age) = now.checked_sub(recorded_time) {
                    if age >= threshold_secs {
                        to_delete.push(entry_path);
                    }
                } else {
                    warn!("  時間計算溢出，跳過: {}", rela_path.display());
                }
            }
        }

        /*
            更新當前資料夾的記錄時間
            使用子項目的最大修改時間
         */
        if max_child_modified > 0 {
            let rela_path: &Path = path
                .strip_prefix(root)
                .map_err(|e: std::path::StripPrefixError|
                    io::Error::new(io::ErrorKind::Other, e.to_string())
                )?;

            // 只有非根目錄才更新
            if rela_path.as_os_str() != "" {
                if let Some(current_recorded) = db.get(root, rela_path) {
                    if max_child_modified > current_recorded {
                        db.upsert(root, rela_path, max_child_modified);
                    }
                } else {
                    // 當前資料夾沒有記錄，創建記錄
                    db.upsert(root, rela_path, max_child_modified);
                }
            }
        }

        Ok(())
    }

    /// ### 小掃描
    ///
    /// 僅讀取紀錄。
    ///
    /// - db 資料庫
    /// - folder_path 目標任務資料夾(絕對路徑)
    /// - target 匹配目標(可選)
    /// - threshold_secs 閾值秒數
    /// - really_delete 是否徹底刪除
    /// - test_mode 是否為測試模式
    fn perform_small_scan(
        &self,
        db: &mut scanner::ScanDatabase,
        folder_path: &Path,
        target: Option<&Vec<String>>,
        threshold_secs: u64,
        really_delete: bool,
        test_mode: bool
    ) -> io::Result<()> {
        let now: u64 = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_secs(),
            Err(e) => {
                error!("系統時間錯誤: {}", e);
                return Err(io::Error::new(io::ErrorKind::Other, "系統時間錯誤"));
            }
        };

        let mut to_delete: Vec<PathBuf> = Vec::new(); // 絕對路徑

        // 創建匹配器
        let set: Option<globset::GlobSet> = if let Some(some_target) = target {
            let mut builder: GlobSetBuilder = GlobSetBuilder::new();
            for p in some_target {
                builder.add(
                    Glob::new(&p).map_err(|e: globset::Error|
                        io::Error::new(io::ErrorKind::Other, e)
                    )?
                );
            }
            Some(
                builder
                    .build()
                    .map_err(|e: globset::Error| io::Error::new(io::ErrorKind::Other, e))?
            )
        } else {
            None
        };

        // 取得所有目標
        let mut target_path: Vec<(usize, PathBuf)> = db
            .entries_older_than(folder_path, now - threshold_secs)
            .map(|(p, _)| {
                let path: PathBuf = scanner::bytes_to_path(p); // 產生新的PathBuf
                (path.components().count(), path)
            })
            .collect();

        target_path.sort_by(|(depth_a, _), (depth_b, _)| depth_b.cmp(depth_a)); // 由深到淺排序

        /*
            抓到資料夾過期的情況
            代表其內部最後修改的檔案是過期的
            也就是說理論上其內部所有檔案都過期了
            這時由深遍歷，只要發現其中一個沒過期(紀錄沒更新到)
            它便會向上傳遞最後修改日期
            那原本過期的資料夾就會脫離過期狀態

            如果真的發現一個資料夾紀錄是過期的
            會再驗證其metadata(最後修改日期)
            這時如果發現metadata更新了
            代表資料夾發生內容增刪
            這時再更新資料並向上傳遞
         */

        // 遍歷目標
        // rela_path 是相對路徑
        for (_, rela_path) in target_path {
            // 關鍵修正：檢查是否匹配 target
            if let Some(match_set) = &set {
                // 檢查路徑是否匹配
                let is_match = match_set.is_match(&rela_path);

                // 檢查是否在已匹配的父資料夾內
                let in_target_folder = rela_path
                    .ancestors()
                    .skip(1) // 跳過自己
                    .any(|ancestor| match_set.is_match(ancestor));

                if !is_match && !in_target_folder {
                    continue; // 不匹配，跳過
                }
            }

            // 讀取閥值，讀不到的話，跳過(不會刪除)
            if let Some(recorded_time) = db.get(folder_path, &rela_path) {
                // 使用 checked_sub 防止溢出
                if let Some(age) = now.checked_sub(recorded_time) {
                    if age < threshold_secs {
                        continue; // 尚未達到閥值，跳過
                    }
                } else {
                    warn!("  時間計算溢出，跳過: {}", rela_path.display());
                    continue;
                }

                /*
                    到達閥值後的處理
                 */

                // 實際檔案驗證
                let full_path: PathBuf = folder_path.join(rela_path.clone());
                if full_path.exists() {
                    let metadata: fs::Metadata = full_path.metadata()?;
                    if let Ok(modified) = metadata.modified() {
                        let file_modified: u64 = match modified.duration_since(UNIX_EPOCH) {
                            Ok(d) => d.as_secs(),
                            Err(_) => {
                                warn!("  檔案時間異常，跳過: {}", rela_path.display());
                                continue;
                            }
                        }; // 取得實際最後修改時間
                        if file_modified > recorded_time {
                            /*
                                等於最正常，
                                小於 => 必定過期，可能是下載等遺留舊時間，
                                大於才代表更新了，需要重新檢查
                             */

                            // 更新資料庫
                            db.upsert(folder_path, &rela_path, file_modified); // 更新自己
                            self.update_parent_folders_in_db(
                                &rela_path,
                                folder_path,
                                file_modified,
                                db
                            ); // 更新父資料夾

                            // 再度判斷是否達到閥值，未達到 => 跳過
                            if let Some(age) = now.checked_sub(file_modified) {
                                if age < threshold_secs {
                                    continue;
                                }
                            } else {
                                warn!("  時間計算溢出，跳過: {}", rela_path.display());
                                continue;
                            }
                        }
                    }
                } else {
                    // 檔案已不存在，從資料庫中移除
                    db.remove(folder_path, &rela_path);
                    continue;
                }

                // 添加到待刪除名單
                to_delete.push(full_path);
            }
        }

        // 執行刪除（優化後）
        if !to_delete.is_empty() {
            let optimized: Vec<PathBuf> = self.optimize_delete_paths(&to_delete, folder_path, db);
            self.execute_deletions(&optimized, folder_path, really_delete, test_mode, db);
        }

        Ok(())
    }

    /// ### 更新父資料夾的修改時間
    ///
    /// 根據指定時間，去更新父資料夾的最後修改時間。
    /// 存在紀錄才修改。
    ///
    /// ! 根目錄不做判定
    /// ! 從目標路徑(path)的父目錄開始
    ///
    /// - path 目標路徑(相對)
    /// - root 根目錄(絕對)
    /// - modified_time 修改時間(秒)
    /// - db 資料庫
    fn update_parent_folders_in_db(
        &self,
        path: &Path,
        root: &Path,
        modified_time: u64,
        db: &mut scanner::ScanDatabase
    ) {
        let mut current: &Path = path;
        while let Some(parent) = current.parent() {
            if parent == "" {
                // 如果是根目錄，結束
                break;
            }

            // 檢查父資料夾記錄
            if let Some(parent_recorded) = db.get(root, parent) {
                if modified_time > parent_recorded {
                    // 更新資料
                    db.upsert(root, parent, modified_time);
                } else {
                    // 如果父資料夾不用更新，不需要繼續向上
                    break;
                }
            } else {
                // 父資料夾沒有記錄，創建記錄以保持一致性
                db.upsert(root, parent, modified_time);
            }

            current = parent;
        }
    }

    /// ### 優化刪除路徑
    ///
    /// 合併刪除目標，避免刪除碎片化。
    /// 確保不會刪除任務根目錄。
    ///
    /// - paths 路徑列表(絕對路徑)
    /// - task_folder 任務資料夾(絕對路徑)
    /// - db 資料庫
    fn optimize_delete_paths(
        &self,
        paths: &[PathBuf],
        task_folder: &Path,
        db: &mut scanner::ScanDatabase
    ) -> Vec<PathBuf> {
        if paths.is_empty() {
            return Vec::new();
        }

        let mut sorted_paths: Vec<&PathBuf> = paths.iter().collect();

        // 用字典序排序
        sorted_paths.sort_unstable();

        let mut optimized: Vec<PathBuf> = Vec::with_capacity(sorted_paths.len() / 2);
        let mut removed_count = 0;

        for path in sorted_paths {
            // 關鍵修正：不刪除任務根目錄
            if *path == task_folder {
                error!("  嚴重警告：嘗試刪除任務根目錄，已阻止: {}", task_folder.display());
                continue;
            }

            // 確保路徑在任務資料夾內
            if !path.starts_with(task_folder) {
                error!("  警告：路徑不在任務資料夾內，跳過: {}", path.display());
                continue;
            }

            let mut is_child = false;

            // 檢查是否是已有路徑的子路徑
            for parent in &optimized {
                if path.starts_with(parent) {
                    is_child = true;
                    // 從資料庫移除
                    if let Ok(rela_path) = path.strip_prefix(task_folder) {
                        db.remove(task_folder, rela_path);
                        removed_count += 1;
                    }
                    break;
                }
            }

            if !is_child {
                optimized.push((*path).clone());
            }
        }

        if removed_count > 0 {
            info!("  優化刪除: 合併了 {} 個子路徑", removed_count);
        }

        optimized
    }

    /// ### 執行刪除操作
    ///
    /// 將目標檔案刪除，並且從資料庫移除。
    /// 多重安全檢查確保不會誤刪。
    ///
    /// - paths 目標檔案列表(絕對路徑)
    /// - task_folder 任務根目錄(絕對路徑)
    /// - really_delete 是否徹底刪除
    /// - test_mode 是否為測試模式
    /// - db 資料庫
    fn execute_deletions(
        &self,
        paths: &[PathBuf],
        task_folder: &Path,
        really_delete: bool,
        test_mode: bool,
        db: &mut scanner::ScanDatabase
    ) {
        let mut success_count = 0;
        let mut fail_count = 0;

        for path in paths {
            // 多重安全檢查
            // 1. 不刪除任務根目錄
            if path == task_folder {
                error!("  ✗ 嚴重警告：嘗試刪除任務根目錄，已阻止: {}", task_folder.display());
                fail_count += 1;
                continue;
            }

            // 2. 確保路徑在任務資料夾內
            if !path.starts_with(task_folder) {
                error!("  ✗ 警告：路徑不在任務資料夾內，跳過: {}", path.display());
                fail_count += 1;
                continue;
            }

            // 3. 檢查路徑是否存在
            if !path.exists() {
                warn!("  ⚠ 路徑已不存在，跳過: {}", path.display());
                // 從資料庫移除
                if let Ok(rela_path) = path.strip_prefix(task_folder) {
                    db.remove(task_folder, rela_path);
                }
                continue;
            }

            if test_mode {
                info!("  [測試模式] 將刪除: {}", path.display());
                success_count += 1;
                continue;
            }

            let result: Result<(), io::Error> = if really_delete {
                // 徹底刪除
                if path.is_dir() {
                    fs::remove_dir_all(path)
                } else {
                    fs::remove_file(path)
                }
            } else {
                // 移入垃圾桶
                trash
                    ::delete(path)
                    .map_err(|e: trash::Error| io::Error::new(io::ErrorKind::Other, e))
            };

            match result {
                Ok(_) => {
                    let method: &str = if really_delete { "徹底刪除" } else { "移入垃圾桶" };
                    info!("  ✓ {}: {}", method, path.display());
                    if let Ok(rela_path) = path.strip_prefix(task_folder) {
                        // 從資料庫移除
                        db.remove(task_folder, rela_path);
                    }
                    success_count += 1;
                }
                Err(e) => {
                    error!("  ✗ 刪除失敗: {} - {}", path.display(), e);
                    fail_count += 1;
                }
            }
        }

        if success_count > 0 || fail_count > 0 {
            info!("  刪除統計: 成功 {} 個，失敗 {} 個", success_count, fail_count);
        }
    }
}

// ########## 應用基本定義 ##########
impl ApplicationHandler<FileEvent> for App {
    // 啟動: 初始化圖標
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {
        if self.tray_icon.is_none() {
            // 創建選單
            let tray_menu: Menu = Menu::new();
            if
                let Err(e) = tray_menu.append_items(
                    &[
                        &self.open_config,
                        &self.open_log,
                        &self.refresh_config,
                        &PredefinedMenuItem::separator(),
                        &self.creat_startup_link,
                        &self.remove_startup_link,
                        &PredefinedMenuItem::separator(),
                        &self.quit_item,
                    ]
                )
            {
                error!("創建選單失敗: {}", e);
                return;
            }

            // 創建小圖示
            match
                TrayIconBuilder::new()
                    .with_menu(Box::new(tray_menu))
                    .with_tooltip("ONEE SWEEPER")
                    .build()
            {
                Ok(tray) => {
                    self.tray_icon = Some(tray);
                    self.change_icon();
                }
                Err(e) => {
                    error!("創建小圖示失敗: {}", e);
                }
            }
        }
    }

    // 視窗處理事件: 用不到
    fn window_event(
        &mut self,
        _event_loop: &winit::event_loop::ActiveEventLoop,
        _id: winit::window::WindowId,
        _event: WindowEvent
    ) {}

    // 處理自訂事件
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: FileEvent) {
        match event {
            FileEvent::FileChanged(path) => {
                self.pending_paths.insert(path);
            }
            FileEvent::ConfigChanged => {
                self.reload_config();
            }
        }
    }

    // 處理等待事件
    fn about_to_wait(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        let now: Instant = Instant::now();

        // 任務調度：計算下一次需要喚醒的時間點，並設置事件循環在該時間點喚醒
        let mut next_wakeup: Instant = now + Duration::from_secs(3600); // 預設睡一小時（如果沒任務）

        // 事件驅動
        // 風門(throttle)，非防彈跳
        if !self.pending_paths.is_empty() {
            if now - self.last_process_watcher_path >= Duration::from_secs(3) {
                for path in self.pending_paths.drain() {
                    println!("{}", path.display());
                }
                self.last_process_watcher_path = now;
            }else {
                next_wakeup = self.last_process_watcher_path + Duration::from_secs(3);
            }
        }

        // 掃描
        if let Some(cfg) = &self.config {
            // 判斷掃描狀態
            let s_interval: Duration = Duration::from_secs(
                (cfg.app_setting.small_scan_interval as u64) * 60
            );
            let c_interval: Duration = Duration::from_secs(
                (cfg.app_setting.complete_scan_interval as u64) * 60
            );

            let next_s: Instant = self.last_small_scan + s_interval;
            let next_c: Instant = self.last_complete_scan + c_interval;

            let should_run_small: bool = now >= next_s;
            let should_run_complete: bool = now >= next_c;

            // 執行掃描
            if should_run_complete && should_run_small {
                /*
                    兩者都需，只執行大掃描
                 */
                self.perform_scan(true);
                self.last_complete_scan = now;
                self.last_small_scan = now;
            } else if should_run_complete {
                self.perform_scan(true);
                self.last_complete_scan = now;
            } else if should_run_small {
                self.perform_scan(false);
                self.last_small_scan = now;
            }

            // 計算下一次喚醒時間，使用更精確的調度
            let next_s_time: Instant = self.last_small_scan + s_interval;
            let next_c_time: Instant = self.last_complete_scan + c_interval;
            next_wakeup = next_wakeup.min(next_s_time.min(next_c_time));

            // 如果計算出的喚醒時間已經過去，設為立即喚醒
            if next_wakeup <= now {
                next_wakeup = now + Duration::from_millis(100);
            }
        }

        // 排程下次喚醒
        event_loop.set_control_flow(ControlFlow::WaitUntil(next_wakeup));

        // 處理選單事件
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            debug!("選單點擊事件: {:?}", event);

            if event.id == self.quit_item.id() {
                info!("用戶請求退出程序");
                event_loop.exit();
            } else if event.id == self.open_config.id() {
                info!("打開配置文件");
                if let Err(e) = open_or_create_toml(CONFIG_TOML_PATH) {
                    error!("無法開啟配置文件: {}", e);
                }
            } else if event.id == self.open_log.id() {
                info!("打開日誌文件");
                if let Err(e) = open_log_file() {
                    error!("無法打開日誌文件: {}", e);
                }
            } else if event.id == self.refresh_config.id() {
                info!("刷新配置");
                self.reload_config();
            } else if event.id == self.creat_startup_link.id() {
                match create_startup_link() {
                    Ok(()) => {
                        info!("創建啟動鏈接成功");
                        Notification::new()
                            .appname("ONEE SWEEPER")
                            .summary("創建成功")
                            .body("創建啟動鏈接成功。")
                            .timeout(5000)
                            .show()
                            .unwrap();
                    }
                    Err(e) => {
                        error!("創建啟動鏈接失敗{}", e);
                        Notification::new()
                            .appname("ONEE SWEEPER")
                            .summary("創建失敗")
                            .body("創建啟動鏈接失敗，詳情請查閱日誌文件。")
                            .timeout(5000)
                            .show()
                            .unwrap();
                    }
                }
            } else if event.id == self.remove_startup_link.id() {
                match remove_startup_link() {
                    Ok(true) => {
                        info!("已移除啟動連結");
                        Notification::new()
                            .appname("ONEE SWEEPER")
                            .summary("移除成功")
                            .body("已移除啟動連結。")
                            .timeout(5000)
                            .show()
                            .unwrap();
                    }
                    Ok(false) => {
                        info!("未找到啟動連結");
                        Notification::new()
                            .appname("ONEE SWEEPER")
                            .summary("移除失敗")
                            .body("未找到啟動連結。")
                            .timeout(5000)
                            .show()
                            .unwrap();
                    }
                    Err(e) => {
                        error!("移除啟動鏈接失敗{}", e);
                        Notification::new()
                            .appname("ONEE SWEEPER")
                            .summary("移除失敗")
                            .body("移除啟動鏈接失敗，詳情請查閱日誌文件。")
                            .timeout(5000)
                            .show()
                            .unwrap();
                    }
                }
            }
        }
    }
}

fn main() -> io::Result<()> {
    // 初始化日誌系統
    if let Err(e) = init_logging() {
        eprintln!("日誌系統初始化失敗: {}", e);
        return Ok(());
    }

    info!("程序啟動");

    let event_loop: EventLoop<FileEvent> = match EventLoop::with_user_event().build() {
        Ok(el) => el,
        Err(e) => {
            error!("創建事件迴圈失敗: {}", e);
            return Err(io::Error::new(io::ErrorKind::Other, e.to_string()));
        }
    }; // 創建事件迴圈

    let proxy: EventLoopProxy<FileEvent> = event_loop.create_proxy();
    let watcher_cmd: crossbeam_channel::Sender<WatchCommand> = start_watcher(proxy.clone());

    // 讀取配置並清理舊日誌
    let config: Option<Config> = read_config(); // 讀取配置文件
    if config.is_none() {
        Notification::new()
            .appname("ONEE SWEEPER")
            .summary("配置錯誤")
            .body("配置錯誤，程序未運行，詳情請查閱日誌文件。")
            .timeout(5000)
            .show()
            .unwrap();
    }

    let log_max_size: u64 = config // 紀錄檔最大檔案大小 ( mb )
        .as_ref()
        .and_then(|c: &Config| c.app_setting.log_max_size_mb)
        .unwrap_or(10);

    if let Err(e) = cleanup_old_logs(log_max_size) {
        error!("清理舊日誌失敗: {}", e);
    }

    // 初始化應用狀態
    let now_instant: Instant = Instant::now();
    let mut app: App = App {
        proxy: proxy.clone(),
        watcher_cmd: watcher_cmd,
        pending_paths: HashSet::new(),
        last_process_watcher_path: Instant::now(),
        tray_icon: None,
        open_config: MenuItem::new("開啟配置", true, None),
        open_log: MenuItem::new("查看日誌", true, None),
        refresh_config: MenuItem::new("刷新配置", true, None),
        creat_startup_link: MenuItem::new("創建開機啟動", true, None),
        remove_startup_link: MenuItem::new("移除開機啟動", true, None),
        quit_item: MenuItem::new("退出", true, None),
        last_complete_scan: now_instant,
        last_small_scan: now_instant,
        config,
    };

    // 使用新的 run_app
    if let Err(e) = event_loop.run_app(&mut app) {
        error!("事件迴圈異常退出: {}", e);
    }

    info!("程序退出");

    Ok(())
}
