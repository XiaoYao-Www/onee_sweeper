#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// 使用 mimalloc 作為全域分配器，降低記憶體碎片化（尤其是在 Windows 上）
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod scanner;
mod audit_log;

use log::{ info, warn, error, debug };
use simplelog::*;
use winit::{
    application::ApplicationHandler,
    event::{ WindowEvent },
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

use onee_sweeper_core::type_define::Config;

const CONFIG_TOML_PATH: &str = "config.toml";
const LOG_FILE_NAME: &str = "run.log";
const TEMP_BIN_PATH: &str = "temp.bin";
/// UI 與 daemon 之間的簡易檔案級 IPC：UI 寫入此檔案來觸發立即掃描
const IPC_SIGNAL_PATH: &str = "command.signal";
/// daemon 啟動時寫入 PID，讓 UI 判斷 daemon 是否在執行
const DAEMON_PID_PATH: &str = "daemon.pid";

// 檔案系統監控事件
enum FileEvent {
    ConfigChanged, // 配置文件變更
    FileChanged(PathBuf), // 檔案變更 ( 非配置文件 )
}

// 監控系統命令（部分枚舉變體保留供未來擴展）
#[allow(dead_code)]
enum WatchCommand {
    Watch(PathBuf), // 監控路徑
    Unwatch(PathBuf), // 取消監控路徑
    UnwatchAll, // 取消所有監控路徑
    ReplaceAll(Vec<PathBuf>), // 替換所有路徑
    Stop, // 停止監控
}

fn start_watcher(proxy: EventLoopProxy<FileEvent>, config_path: Option<PathBuf>) -> crossbeam_channel::Sender<WatchCommand> {
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

        // 如果提供了設定檔路徑，監控其所在目錄（notify 需要監控父目錄才能捕獲檔案變更）
        let config_path_for_watch: Option<PathBuf> = config_path.clone();
        if let Some(ref cfg_path) = config_path {
            if let Some(parent) = cfg_path.parent() {
                if parent.exists() {
                    let _ = watcher.watch(parent, RecursiveMode::NonRecursive);
                    info!("已註冊設定檔監控: {:?}", cfg_path);
                }
            }
        }

        loop {
            select! {
                // 🟢 notify event
                recv(event_rx) -> res => {
                    if let Ok(Ok(event)) = res {
                        match event.kind {
                            EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_) => {
                                for path in event.paths {
                                    // 檢查是否為設定檔變更
                                    if let Some(ref cfg_path) = config_path_for_watch {
                                        if path == *cfg_path || path.ends_with("config.toml") {
                                            let _ = proxy.send_event(FileEvent::ConfigChanged);
                                            info!("偵測到設定檔變更");
                                            continue;
                                        }
                                    }
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
/// ### 初始化日誌系統
/// 同時輸出到終端機（Debug 等級）和 run.log 檔案（Info 等級）。
/// daemon 啟動時呼叫一次。
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
/// ### 清理過大的舊日誌檔案
/// 當 run.log 超過指定大小（MB）時，備份為 run.log.old 並建立新檔案。
/// 防止日誌檔案無限增長佔用磁碟空間。
///
/// - max_size_mb: 觸發清理的檔案大小閾值（MB）
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
/// ### 獲取基於當前執行檔的完整檔案路徑
/// 所有資料檔案（config.toml, temp.bin, run.log 等）都存放在 .exe 所在目錄，
/// 確保 daemon 和 UI 共用同一份資料。
///
/// - file_path: 相對於執行檔目錄的檔案名稱
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
    onee_sweeper_core::config::read_config(&path)
}

/// ### 創建開機啟動
///
/// 創建開機啟動連結。
/// ### 創建 Windows 開機自動啟動捷徑
/// 在開始功能表的啟動資料夾中建立 .lnk 捷徑，
/// 讓 daemon 隨 Windows 開機自動啟動。
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
/// ### 移除 Windows 開機自動啟動捷徑
/// 回傳是否成功找到並刪除捷徑。
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

/// ### 取得當前程序的記憶體使用量（MB）
///
/// 在 Windows 上使用 GetProcessMemoryInfo，其他平台回傳 0（不限制）。
/// 用於在掃描迴圈中檢查是否超過 max_memory_mb 設定。
#[cfg(windows)]
fn get_current_memory_mb() -> u64 {
    #[allow(non_snake_case, dead_code)]
    // 直接連結 psapi.dll，不需要 windows-sys 的類型系統
    #[link(name = "psapi")]
    extern "system" {
        fn GetProcessMemoryInfo(
            hProcess: isize,
            ppmem_counters: *mut std::ffi::c_void,
            cb: u32,
        ) -> i32;
        fn GetCurrentProcess() -> isize;
    }

    #[allow(non_snake_case)]
    // PROCESS_MEMORY_COUNTERS 結構（Windows SDK 定義）
    #[repr(C)]
    struct PROCESS_MEMORY_COUNTERS {
        cb: u32,
        PageFaultCount: u32,
        PeakWorkingSetSize: usize,
        WorkingSetSize: usize,         // bytes，這就是我們要的值
        QuotaPeakPagedPoolUsage: usize,
        QuotaPagedPoolUsage: usize,
        QuotaPeakNonPagedPoolUsage: usize,
        QuotaNonPagedPoolUsage: usize,
        PagefileUsage: usize,
        PeakPagefileUsage: usize,
    }

    let mut pmc = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        PageFaultCount: 0,
        PeakWorkingSetSize: 0,
        WorkingSetSize: 0,
        QuotaPeakPagedPoolUsage: 0,
        QuotaPagedPoolUsage: 0,
        QuotaPeakNonPagedPoolUsage: 0,
        QuotaNonPagedPoolUsage: 0,
        PagefileUsage: 0,
        PeakPagefileUsage: 0,
    };

    let ret = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut pmc as *mut _ as *mut std::ffi::c_void,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        )
    };

    if ret != 0 {
        pmc.WorkingSetSize as u64 / (1024 * 1024)
    } else {
        0
    }
}

#[cfg(not(windows))]
fn get_current_memory_mb() -> u64 { 0 }

/// ### 應用程序結構
/// ### 應用程式主結構
///
/// 持有事件迴圈代理、檔案監控、系統托盤圖標、配置等所有狀態。
/// 實作 winit ApplicationHandler trait 作為事件驅動核心。
struct App {
    #[allow(dead_code)]
    proxy: EventLoopProxy<FileEvent>, // 文件事件代理
    watcher_cmd: crossbeam_channel::Sender<WatchCommand>, // 監控命令發射器
    pending_paths: HashSet<PathBuf>, // 等待處理路徑
    last_process_watcher_path: Instant, // 最後一次處理監控路徑的時間戳

    tray_icon: Option<TrayIcon>, // 圖標
    open_config: MenuItem, // 打開配置
    /// 開啟 UI 設定面板（啟動 onee_sweeper_ui.exe）
    open_ui: MenuItem,
    open_log: MenuItem, // 打開日誌
    refresh_config: MenuItem, // 刷新配置
    creat_startup_link: MenuItem, // 創建開機啟動連結
    remove_startup_link: MenuItem, // 移除開機啟動連結
    quit_item: MenuItem, // 退出選項

    config: Option<Config>, // 配置文件
    last_small_scan: Instant, // 上次小掃描時間
    last_complete_scan: Instant, // 上次完整掃描時間
    last_user_activity: Instant, // 上次使用者活動時間（用於閒置偵測）
    /// 追蹤上一輪的閒置狀態（None = 第一輪，避免刷屏日誌）
    was_user_idle: Option<bool>,
}

// ########## 應用功能 ##########
impl App {
    /// ### 切換小圖示圖標
    ///
    /// 根據狀態切換小圖示圖標，有合法配置時視為執行
    fn change_icon(&mut self) {
        if let Some(tray) = self.tray_icon.as_mut() {
            let icon_result = if self.config.is_some() {
                load_icon(include_bytes!("../../assets/icon_run.ico"))
            } else {
                load_icon(include_bytes!("../../assets/icon_stop.ico"))
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
                                .map(|t: &onee_sweeper_core::type_define::FolderTask| t.folder_path.clone())
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
/// ### 執行一次排程掃描（完整或快速）
///
/// 載入資料庫，遍歷所有已啟用的任務，根據類型執行：
/// - 完整掃描（is_complete=true）：遞迴遍歷目錄，發現新檔案並判斷過期
/// - 快速掃描（is_complete=false）：僅查詢現有資料庫記錄，不實際讀取檔案系統
///
    fn perform_scan(&self, is_complete: bool) {
// - is_complete: true = 完整掃描，false = 快速掃描
        let memory_limit_mb: u64 = self.config.as_ref()
            .map(|c| c.app_setting.max_memory_mb_effective())
            .unwrap_or(0);
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
                // 檢查任務是否已停用（enabled = false）
                if !task.is_enabled() {
                    debug!("  任務已停用，跳過: {}", task.folder_path.to_string_lossy());
                    continue;
                }

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
                        cfg.app_setting.test_mode.unwrap_or(false),
                        task.follow_symlinks_effective()
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

            // 🔍 記憶體用量檢查：如果超過 max_memory_mb 設定，主動釋放
            if memory_limit_mb > 0 {
                let used_mb = get_current_memory_mb();
                if used_mb > memory_limit_mb {
                    info!("記憶體用量 {} MB 超過上限 {} MB，強制釋放", used_mb, memory_limit_mb);
                    db.cleanup_nonexistent_entries();
                    db.folders.shrink_to_fit();
                    if let Err(e) = db.save_to_file(&temp_bin_path) {
                        error!("記憶體釋放時儲存失敗: {}", e);
                    }
                    if let Ok(new_db) = scanner::ScanDatabase::load_from_file(&temp_bin_path, false) {
                        db = new_db;
                    }
                }
            }

            // 定期清理不存在的條目（每次完整掃描後）
            if is_complete {
                db.cleanup_nonexistent_entries();
            }

            // 掃描後統計
            let (folder_count, entry_count) = db.get_stats();
            info!("資料庫狀態: {} 個資料夾，{} 個路徑記錄", folder_count, entry_count);

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
        test_mode: bool,
        follow_symlinks: bool,
    ) -> io::Result<()> {
        // 操作權限預檢
        if !folder_path.is_dir() {
            warn!("  ⚠ 任務資料夾不是有效目錄，跳過: {}", folder_path.display());
            return Ok(());
        }
        // 檢查讀取權限：嘗試列出目錄內容
        match fs::read_dir(folder_path) {
            Ok(_) => {}
            Err(e) => {
                warn!("  ⚠ 無法讀取目錄（權限不足），跳過任務: {} - {}", folder_path.display(), e);
                return Ok(());
            }
        }
        // 如果啟用徹底刪除，額外檢查刪除權限
        if really_delete {
            let probe_file = folder_path.join(".onee_sweeper_perms_check");
            match fs::File::create(&probe_file) {
                Ok(_) => {
                    // 成功創建後立即刪除
                    let _ = fs::remove_file(&probe_file);
                }
                Err(e) => {
                    warn!("  ⚠ 無法在目錄中創建/刪除文件（權限不足），跳過徹底刪除任務: {} - {}", folder_path.display(), e);
                    return Ok(());
                }
            }
        }

        let now: u64 = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_secs(),
            Err(e) => {
                error!("系統時間錯誤: {}", e);
                return Err(io::Error::new(io::ErrorKind::Other, "系統時間錯誤"));
            }
        };

        let mut to_delete: Vec<PathBuf> = Vec::with_capacity(50); // 分批處理，初始容量 50

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

        // 分批掃描與刪除：每累積 BATCH_SIZE 個待刪除路徑就處理一次
        const BATCH_SIZE: usize = 50;

        // 遞迴掃描資料夾（分批處理）
            self.scan_directory_recursive_batched(
                folder_path,
                folder_path,
                &set,
                false,
                threshold_secs,
                now,
                db,
                &mut to_delete,
                BATCH_SIZE,
                really_delete,
                test_mode,
                follow_symlinks,
            )?;

        // 處理最後一批
        if !to_delete.is_empty() {
            let optimized = self.optimize_delete_paths(&to_delete, folder_path, db);
            self.execute_deletions(&optimized, folder_path, really_delete, test_mode, db);
            to_delete.clear();
        }

        Ok(())
    }

    /// ### 遞迴掃描目錄（分批處理版）
    ///
    /// 與 scan_directory_recursive 功能相同，但在 to_delete 達到 batch_size 時
    /// 自動執行刪除並清空緩衝區，降低尖峰記憶體使用。
/// ### 遞迴掃描目錄（分批處理版）
///
/// 與傳統遞迴掃描功能相同，但在 to_delete 達到 batch_size 時自動執行批次刪除，
/// 降低尖峰記憶體使用量。每批處理 50 個刪除目標。
///
/// - follow_symlinks: 是否跟隨符號連結（來自任務設定）
    fn scan_directory_recursive_batched(
        &self,
        path: &Path,
        root: &Path,
        target: &Option<globset::GlobSet>,
        in_target_folder: bool,
        threshold_secs: u64,
        now: u64,
        db: &mut scanner::ScanDatabase,
        to_delete: &mut Vec<PathBuf>,
        batch_size: usize,
        really_delete: bool,
        test_mode: bool,
        // 是否跟隨符號連結（來自任務設定）
        follow_symlinks: bool,
    ) -> io::Result<()> {
        let entries: fs::ReadDir = fs::read_dir(path)?;
        let mut max_child_modified: u64 = 0u64;

        for entry in entries {
            let entry: fs::DirEntry = entry?;
            let entry_path: PathBuf = entry.path();
            let rela_path: PathBuf = entry_path
                .strip_prefix(root)
                .map_err(|e: std::path::StripPrefixError|
                    io::Error::new(io::ErrorKind::Other, e.to_string())
                )?
                .to_path_buf();
            let metadata: fs::Metadata = entry.metadata()?;

            let is_match: bool = match target {
                None => true,
                Some(match_set) => {
                    let path_matches = match_set.is_match(&rela_path);
                    path_matches || in_target_folder
                }
            };

            if metadata.is_dir() {
                if entry_path == root {
                    warn!("  警告：跳過任務根目錄: {}", root.display());
                    continue;
                }

                // 安全檢查：根據設定決定是否跳過符號連結
                let is_symlink: bool = metadata.file_type().is_symlink();
                if is_symlink && !follow_symlinks {
                    warn!(
                        "  ⚠ 跳過符號連結（follow_symlinks = false）: {} -> {}",
                        rela_path.display(),
                        fs::read_link(&entry_path).unwrap_or_default().display()
                    );
                    continue;
                }

                // 遞迴子目錄（分批）
                self.scan_directory_recursive_batched(
                    &entry_path,
                    root,
                    target,
                    is_match,
                    threshold_secs,
                    now,
                    db,
                    to_delete,
                    batch_size,
                    really_delete,
                    test_mode,
                    follow_symlinks,
                )?;

                if !is_match {
                    continue;
                }

                let recorded_time: u64 = if let Some(recorded) = db.get(root, &rela_path) {
                    recorded
                } else {
                    db.upsert(root, &rela_path, now);
                    now
                };

                if recorded_time > max_child_modified {
                    max_child_modified = recorded_time;
                }

                if let Some(age) = now.checked_sub(recorded_time) {
                    if age >= threshold_secs {
                        to_delete.push(entry_path);
                    }
                } else {
                    warn!("  資料夾時間計算溢出，跳過: {}", rela_path.display());
                }

                continue;
            }

            if !is_match {
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

                let recorded_time: u64 = if let Some(recorded) = db.get(root, &rela_path) {
                    if file_modified > recorded {
                        db.upsert(root, &rela_path, file_modified);
                        file_modified
                    } else {
                        recorded
                    }
                } else {
                    db.upsert(root, &rela_path, now);
                    now
                };

                if recorded_time > max_child_modified {
                    max_child_modified = recorded_time;
                }

                if let Some(age) = now.checked_sub(recorded_time) {
                    if age >= threshold_secs {
                        to_delete.push(entry_path);
                    }
                } else {
                    warn!("  時間計算溢出，跳過: {}", rela_path.display());
                }
            }

            // 達到批次大小時立即處理
            if to_delete.len() >= batch_size {
                let batch: Vec<PathBuf> = to_delete.drain(..).collect();
                let optimized = self.optimize_delete_paths(&batch, root, db);
                self.execute_deletions(&optimized, root, really_delete, test_mode, db);
                info!("  批次刪除完成 ({} 個)", optimized.len());
            }
        }

        // 更新當前資料夾的記錄時間
        if max_child_modified > 0 {
            let rela_path: &Path = path
                .strip_prefix(root)
                .map_err(|e: std::path::StripPrefixError|
                    io::Error::new(io::ErrorKind::Other, e.to_string())
                )?;

            if rela_path.as_os_str() != "" {
                if let Some(current_recorded) = db.get(root, rela_path) {
                    if max_child_modified > current_recorded {
                        db.upsert(root, rela_path, max_child_modified);
                    }
                } else {
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
        // 初始化審計日誌（使用應用程式資料目錄）
        let audit_log: Option<audit_log::AuditLog> = get_file_path("")
            .ok()
            .map(|base_dir| audit_log::AuditLog::new(&base_dir, 10)); // 最大 10 MB

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

            // 4. 計算刪除前 hash（用於審計日誌）
            let pre_delete_hash: String = match audit_log::compute_file_hash(path) {
                Ok(h) => h,
                Err(_) => "HASH_FAILED".to_string(),
            };
            let file_size: u64 = fs::metadata(path).map(|m| m.len()).unwrap_or(0);

            if test_mode {
                info!("  [測試模式] 將刪除: {}", path.display());
                if let Some(ref log) = audit_log {
                    let entry = audit_log::AuditEntry {
                        timestamp: audit_log::current_timestamp(),
                        operation: "TEST_DELETE".to_string(),
                        file_path: path.to_path_buf(),
                        file_size,
                        hash: pre_delete_hash,
                        result: "TEST_MODE".to_string(),
                    };
                    let _ = log.append(&entry);
                }
                success_count += 1;
                continue;
            }

            let result: Result<(), io::Error> = if really_delete {
                if path.is_dir() {
                    fs::remove_dir_all(path)
                } else {
                    fs::remove_file(path)
                }
            } else {
                trash
                    ::delete(path)
                    .map_err(|e: trash::Error| io::Error::new(io::ErrorKind::Other, e))
            };

            match result {
                Ok(_) => {
                    let method: &str = if really_delete { "徹底刪除" } else { "移入垃圾桶" };
                    info!("  ✓ {}: {}", method, path.display());
                    if let Ok(rela_path) = path.strip_prefix(task_folder) {
                        db.remove(task_folder, rela_path);
                    }
                    success_count += 1;

                    // 寫入審計日誌
                    if let Some(ref log) = audit_log {
                        let entry = audit_log::AuditEntry {
                            timestamp: audit_log::current_timestamp(),
                            operation: if really_delete { "PERMANENT_DELETE" } else { "TRASH_DELETE" }.to_string(),
                            file_path: path.to_path_buf(),
                            file_size,
                            hash: pre_delete_hash,
                            result: "SUCCESS".to_string(),
                        };
                        if let Err(e) = log.append(&entry) {
                            warn!("  ⚠ 審計日誌寫入失敗: {}", e);
                        }
                    }
                }
                Err(e) => {
                    error!("  ✗ 刪除失敗: {} - {}", path.display(), e);
                    fail_count += 1;

                    if let Some(ref log) = audit_log {
                        let entry = audit_log::AuditEntry {
                            timestamp: audit_log::current_timestamp(),
                            operation: "DELETE_FAILED".to_string(),
                            file_path: path.to_path_buf(),
                            file_size,
                            hash: pre_delete_hash,
                            result: format!("FAILURE: {}", e),
                        };
                        let _ = log.append(&entry);
                    }
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
                        &self.open_ui,
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
            // 使用 checked_duration_since 防止 Instant 非單調問題（suspend/resume 後跳變）
            let elapsed: Duration = now.checked_duration_since(self.last_process_watcher_path)
                .unwrap_or(Duration::ZERO);
            if elapsed >= Duration::from_secs(3) {
                // 載入資料庫以處理檔案變更事件
                let temp_bin_path: PathBuf = match get_file_path(TEMP_BIN_PATH) {
                    Ok(p) => p,
                    Err(_) => {
                        warn!("無法解析暫存路徑，跳過檔案變更處理");
                        self.pending_paths.clear();
                        self.last_process_watcher_path = now;
                        return;
                    }
                };

                if let Ok(mut db) = scanner::ScanDatabase::load_from_file(&temp_bin_path, true) {
                    let changes: Vec<PathBuf> = self.pending_paths.drain().collect();
                    for path in &changes {
                        // 檢查每個任務資料夾，找到對應的任務
                        let mut handled = false;
                        if let Some(cfg) = &self.config {
                            for task in &cfg.tasks {
                                if path.starts_with(&task.folder_path) {
                                    if let Ok(rela_path) = path.strip_prefix(&task.folder_path) {
                                        if path.exists() {
                                            // 檔案存在：檢查 metadata 並更新資料庫
                                            match path.metadata() {
                                                Ok(meta) => {
                                                    if let Ok(modified) = meta.modified() {
                                                        if let Ok(d) = modified.duration_since(UNIX_EPOCH) {
                                                            let file_modified: u64 = d.as_secs();
                                                            let recorded: u64 = db.get(&task.folder_path, rela_path).unwrap_or(0);
                                                            if file_modified != recorded {
                                                                db.upsert(&task.folder_path, rela_path, file_modified);
                                                                debug!("  檔案變更已更新: {} (mtime: {})", rela_path.display(), file_modified);
                                                            }
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    warn!("  無法讀取檔案 metadata: {} - {}", path.display(), e);
                                                }
                                            }
                                        } else {
                                            // 檔案已不存在：從資料庫移除
                                            db.remove(&task.folder_path, rela_path);
                                            info!("  檔案已刪除，從資料庫移除: {}", rela_path.display());
                                        }
                                        handled = true;
                                    }
                                    break;
                                }
                            }
                        }
                        if !handled {
                            debug!("  檔案變更（非任務路徑）: {}", path.display());
                        }
                    }
                    // 儲存更新後的資料庫
                    let _ = db.save_to_file(&temp_bin_path);
                } else {
                    warn!("無法載入資料庫，跳過檔案變更處理");
                    self.pending_paths.clear();
                }

                self.last_process_watcher_path = now;
            }else {
                next_wakeup = self.last_process_watcher_path + Duration::from_secs(3);
            }
        }

        // 掃描（檢查使用者是否足夠閒置）
        if let Some(cfg) = &self.config {
            // 掃描間隔（在閒置檢查前計算，因為需要這些值來排程喚醒）
            let s_interval: Duration = Duration::from_secs(
                (cfg.app_setting.small_scan_interval as u64) * 60
            );
            let c_interval: Duration = Duration::from_secs(
                (cfg.app_setting.complete_scan_interval as u64) * 60
            );

            // 閒置偵測：如果使用者近期有活動，推遲掃描
            let idle_threshold: Duration = Duration::from_secs(
                cfg.app_setting.idle_threshold_min_effective() * 60
            );
            // 使用 checked_duration_since 防止 Instant 非單調性問題
            let idle_duration: Duration = now.checked_duration_since(self.last_user_activity)
                .unwrap_or(Duration::ZERO);
            let user_is_idle: bool = idle_duration >= idle_threshold;

            if !user_is_idle {
                // 只在狀態變化時輸出一行日誌，避免 about_to_wait 每幀刷屏
                if self.was_user_idle != Some(false) {
                    info!("使用者活躍中，推遲排程掃描（閒置 {} 分鐘後執行）", cfg.app_setting.idle_threshold_min_effective());
                }
                self.was_user_idle = Some(false);
                // 使用者仍在活動，推遲掃描，設為閒置後再檢查
                let wake_at: Instant = self.last_user_activity + idle_threshold;
                if wake_at < next_wakeup {
                    next_wakeup = wake_at;
                }
            } else {
                // 使用者已閒置，恢復掃描排程
                if self.was_user_idle != Some(true) {
                    info!("使用者已閒置，恢復正常掃描排程");
                }
                self.was_user_idle = Some(true);
                // 判斷掃描狀態
                let next_s: Instant = self.last_small_scan + s_interval;
                let next_c: Instant = self.last_complete_scan + c_interval;

                let should_run_small: bool = now >= next_s;
                let should_run_complete: bool = now >= next_c;

                // 執行掃描
                if should_run_complete && should_run_small {
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
            }

            // 計算下一次喚醒時間（無論是否閒置都需要）
            let next_s_time: Instant = self.last_small_scan + s_interval;
            let next_c_time: Instant = self.last_complete_scan + c_interval;
            next_wakeup = next_wakeup.min(next_s_time.min(next_c_time));

            // 如果計算出的喚醒時間已經過去，設為立即喚醒
            if next_wakeup <= now {
                next_wakeup = now + Duration::from_millis(100);
            }
        }

        // 🔔 檢查 UI 發送的信號檔案（簡易檔案級 IPC）
        // 必須在排程喚醒之前處理，才能收到信號後立即觸發掃描
        if let Ok(signal_path) = get_file_path(IPC_SIGNAL_PATH) {
            if signal_path.exists() {
                if let Ok(signal_content) = fs::read_to_string(&signal_path) {
                    let _ = fs::remove_file(&signal_path); // 刪除防止重複處理
                    if signal_content.trim() == "scan_now" {
                        info!("收到 UI 的立即掃描請求，執行完整掃描");
                        self.perform_scan(true);
                        self.last_complete_scan = Instant::now();
                        self.last_small_scan = Instant::now();
                    } else {
                        warn!("未知的信號命令: {}", signal_content.trim());
                    }
                }
            }
        }

        // 排程下次喚醒
        event_loop.set_control_flow(ControlFlow::WaitUntil(next_wakeup));

        // 處理選單事件（同時更新使用者活動時間）
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            debug!("選單點擊事件: {:?}", event);
            self.last_user_activity = Instant::now();

            if event.id == self.quit_item.id() {
                info!("用戶請求退出程序");
                event_loop.exit();
            } else if event.id == self.open_config.id() {
                info!("用文字編輯器開啟 config.toml");
                if let Err(e) = open_or_create_toml(CONFIG_TOML_PATH) {
                    error!("無法開啟配置文件: {}", e);
                }
            } else if event.id == self.open_ui.id() {
                info!("啟動 UI 設定面板");
                let ui_path: PathBuf = get_file_path("onee_sweeper_ui.exe").unwrap_or_else(|_| PathBuf::from("onee_sweeper_ui.exe"));
                match std::process::Command::new(&ui_path).spawn() {
                    Ok(_) => info!("UI 設定面板已啟動"),
                    Err(e) => {
                        error!("無法啟動 UI: {}（路徑: {}）", e, ui_path.display());
                        Notification::new()
                            .appname("ONEE SWEEPER")
                            .summary("啟動失敗")
                            .body(&format!("無法啟動 UI 設定面板: {}", e))
                            .timeout(5000)
                            .show()
                            .unwrap();
                    }
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

    info!("daemon 啟動 (PID: {})", std::process::id());

    // 寫入 PID 檔案，讓 UI 判斷 daemon 是否在執行
    if let Ok(pid_path) = get_file_path(DAEMON_PID_PATH) {
        if let Err(e) = fs::write(&pid_path, std::process::id().to_string()) {
            warn!("無法寫入 PID 檔案: {}", e);
        }
    }

    let event_loop: EventLoop<FileEvent> = match EventLoop::with_user_event().build() {
        Ok(el) => el,
        Err(e) => {
            error!("創建事件迴圈失敗: {}", e);
            return Err(io::Error::new(io::ErrorKind::Other, e.to_string()));
        }
    }; // 創建事件迴圈

    let proxy: EventLoopProxy<FileEvent> = event_loop.create_proxy();
    let config_path: PathBuf = match get_file_path(CONFIG_TOML_PATH) {
        Ok(p) => p,
        Err(_) => {
            warn!("無法解析設定檔路徑");
            PathBuf::from(CONFIG_TOML_PATH)
        }
    };
    let watcher_cmd: crossbeam_channel::Sender<WatchCommand> = start_watcher(proxy.clone(), Some(config_path.clone()));

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

    // 根據 scan_on_startup 決定初始掃描時間
    let scan_on_startup: bool = config
        .as_ref()
        .map(|c| c.app_setting.scan_on_startup_effective())
        .unwrap_or(true);

    let initial_scan_time: Instant = if scan_on_startup {
        // 使用 checked_sub 防止 Instant 內部表示過小導致溢位 panic
        // 若溢位則降級為現在時間（跳過啟動掃描，等下次排程）
        now_instant.checked_sub(Duration::from_secs(86400))
            .unwrap_or_else(|| {
                warn!("Instant 內部值過小，跳過啟動掃描（下次排程觸發）");
                now_instant
            })
    } else {
        now_instant
    };

    let mut app: App = App {
        proxy: proxy.clone(),
        watcher_cmd: watcher_cmd,
        pending_paths: HashSet::new(),
        last_process_watcher_path: Instant::now(),
        tray_icon: None,
        open_config: MenuItem::new("開啟配置 (文字編輯器)", true, None),
        open_ui: MenuItem::new("開啟設定面板 (UI)", true, None),
        open_log: MenuItem::new("查看日誌", true, None),
        refresh_config: MenuItem::new("刷新配置", true, None),
        creat_startup_link: MenuItem::new("創建開機啟動", true, None),
        remove_startup_link: MenuItem::new("移除開機啟動", true, None),
        quit_item: MenuItem::new("退出", true, None),
        last_complete_scan: initial_scan_time,
        last_small_scan: initial_scan_time,
        last_user_activity: Instant::now(),
        was_user_idle: None, // 第一輪尚未判定，避免日誌刷屏
        config,
    };

    // 使用新的 run_app
    if let Err(e) = event_loop.run_app(&mut app) {
        error!("事件迴圈異常退出: {}", e);
    }

    info!("程序退出");

    Ok(())
}
