//! ONEE SWEEPER v3.0 — UI 設定面板
//!
//! 獨立執行檔，關閉後不殘留背景程序。
//! 透過共用 config.toml 與 daemon 通訊，使用 command.signal 觸發立即掃描。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use egui::{CentralPanel, CollapsingHeader, Context, FontId, ScrollArea, SidePanel, TopBottomPanel};
use rfd::FileDialog;
use std::fs;
use std::path::{Path, PathBuf};

use onee_sweeper_core::type_define::{AppSettings, Config, FolderTask, Threshold, ScheduleWindow};

// ─── 常數 ────────────────────────────────────────────────────────────────────

const APP_TITLE: &str = "ONEE SWEEPER 設定面板";
const CONFIG_PATH: &str = "config.toml";
const TEMP_BIN_PATH: &str = "temp.bin";
const LOG_PATH: &str = "run.log";
const SIGNAL_PATH: &str = "command.signal";
const PID_PATH: &str = "daemon.pid";
const WINDOW_W: f32 = 900.0;
const WINDOW_H: f32 = 680.0;

// ─── 編輯用的可變結構（鏡像 Config，每個欄位都可直接修改） ──────────────────

/// 編輯中的應用設定（對應 AppSettings 所有欄位）
struct EditAppSettings {
    small_scan_interval: u64,
    complete_scan_interval: u64,
    test_mode: bool,
    log_max_size_mb: u64,
    idle_threshold_min: u64,
    max_memory_mb: u64,
    notification_level: String,
    scan_on_startup: bool,
}

impl From<&AppSettings> for EditAppSettings {
    fn from(s: &AppSettings) -> Self {
        Self {
            small_scan_interval: s.small_scan_interval,
            complete_scan_interval: s.complete_scan_interval,
            test_mode: s.test_mode.unwrap_or(false),
            log_max_size_mb: s.log_max_size_mb.unwrap_or(10),
            idle_threshold_min: s.idle_threshold_min_effective(),
            max_memory_mb: s.max_memory_mb_effective(),
            notification_level: s.notification_level.clone().unwrap_or_else(|| "summary".into()),
            scan_on_startup: s.scan_on_startup_effective(),
        }
    }
}

impl EditAppSettings {
    fn to_app_settings(&self) -> AppSettings {
        AppSettings {
            small_scan_interval: self.small_scan_interval,
            complete_scan_interval: self.complete_scan_interval,
            test_mode: Some(self.test_mode),
            log_max_size_mb: Some(self.log_max_size_mb),
            idle_threshold_min: Some(self.idle_threshold_min),
            max_memory_mb: Some(self.max_memory_mb),
            notification_level: Some(self.notification_level.clone()),
            scan_on_startup: Some(self.scan_on_startup),
        }
    }
}

/// 編輯中的單一任務
#[derive(Clone)]
struct EditFolderTask {
    folder_path: String,
    target_text: String,
    exclude_text: String,
    really_delete: bool,
    enabled: bool,
    follow_symlinks: bool,
    scan_mode: String,
    threshold_day: u32,
    threshold_hour: u32,
    threshold_minute: u32,
    sched_start: String,
    sched_end: String,
}

impl From<&FolderTask> for EditFolderTask {
    fn from(t: &FolderTask) -> Self {
        Self {
            folder_path: t.folder_path.to_string_lossy().to_string(),
            target_text: t.target.as_ref().map(|v| v.join("\n")).unwrap_or_default(),
            exclude_text: t.exclude.as_ref().map(|v| v.join("\n")).unwrap_or_default(),
            really_delete: t.really_delete.unwrap_or(false),
            enabled: t.is_enabled(),
            follow_symlinks: t.follow_symlinks_effective(),
            scan_mode: t.scan_mode.clone().unwrap_or_else(|| "both".into()),
            threshold_day: t.threshold.day as u32,
            threshold_hour: t.threshold.hour as u32,
            threshold_minute: t.threshold.minute as u32,
            sched_start: t.schedule_window.as_ref().and_then(|w| w.start.clone()).unwrap_or_default(),
            sched_end: t.schedule_window.as_ref().and_then(|w| w.end.clone()).unwrap_or_default(),
        }
    }
}

impl EditFolderTask {
    fn to_folder_task(&self) -> FolderTask {
        let target: Option<Vec<String>> = {
            let v: Vec<String> = self.target_text.lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            if v.is_empty() { None } else { Some(v) }
        };
        let exclude: Option<Vec<String>> = {
            let v: Vec<String> = self.exclude_text.lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            if v.is_empty() { None } else { Some(v) }
        };

        let sched = if self.sched_start.is_empty() && self.sched_end.is_empty() {
            None
        } else {
            Some(ScheduleWindow {
                start: if self.sched_start.is_empty() { None } else { Some(self.sched_start.clone()) },
                end: if self.sched_end.is_empty() { None } else { Some(self.sched_end.clone()) },
            })
        };

        FolderTask {
            folder_path: PathBuf::from(&self.folder_path),
            target,
            really_delete: Some(self.really_delete),
            threshold: Threshold {
                day: self.threshold_day as u8,
                hour: self.threshold_hour as u8,
                minute: self.threshold_minute as u8,
            },
            exclude,
            follow_symlinks: Some(self.follow_symlinks),
            enabled: Some(self.enabled),
            scan_mode: Some(self.scan_mode.clone()),
            schedule_window: sched,
        }
    }
}

// ─── 應用狀態 ────────────────────────────────────────────────────────────────

/// UI 應用全域狀態
struct AppState {
    active_tab: Tab,
    /// 由互動式表單控制項編輯的設定（而非 TOML 文字）
    edit_app: EditAppSettings,
    /// 任務列表（可直接修改）
    edit_tasks: Vec<EditFolderTask>,
    /// 錯誤訊息
    config_error: Option<String>,
    /// 狀態列訊息
    status_message: String,
    /// 資料庫狀態快取
    db_stats: String,
    /// 日誌尾部
    log_text: String,
    working_dir: PathBuf,
    /// 是否顯示 RAW（TOML）編輯模式
    show_raw: bool,
    /// RAW 模式的 TOML 文字（僅在 show_raw=true 時使用）
    raw_text: String,
    /// 展開哪個任務的詳細設定（索引）；None = 全部收合
    expanded_task: Option<usize>,
}

#[derive(PartialEq, Clone, Copy)]
enum Tab { Settings, Status, Log, Actions }

impl Tab {
    fn name(&self) -> &'static str {
        match self {
            Tab::Settings => "設定編輯",
            Tab::Status => "任務狀態",
            Tab::Log => "系統日誌",
            Tab::Actions => "操作面板",
        }
    }
}

impl AppState {
    fn path(&self, name: &str) -> PathBuf { self.working_dir.join(name) }

    // ── 載入/儲存 ──────────────────────────────────────────────────────────

    fn load_config(&mut self) {
        let config_path = self.path(CONFIG_PATH);
        match fs::read_to_string(&config_path) {
            Ok(content) => {
                match toml::from_str::<Config>(&content) {
                    Ok(cfg) => {
                        self.edit_app = EditAppSettings::from(&cfg.app_setting);
                        self.edit_tasks = cfg.tasks.iter().map(EditFolderTask::from).collect();
                        self.raw_text = content;
                        self.config_error = None;
                        self.status_message = "設定已載入".into();
                    }
                    Err(e) => {
                        self.config_error = Some(format!("TOML 解析錯誤: {}", e));
                        self.status_message = "載入失敗".into();
                    }
                }
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    self.status_message = "設定檔不存在，請建立".into();
                } else {
                    self.config_error = Some(format!("讀取失敗: {}", e));
                }
            }
        }
    }

    fn save_config(&mut self) {
        let config_path = self.path(CONFIG_PATH);
        let cfg = self.build_config();
        let errors = cfg.validate();
        if !errors.is_empty() {
            self.config_error = Some(format!("設定驗證失敗:\n{}", errors.join("\n")));
            self.status_message = "儲存失敗".into();
            return;
        }
        let toml_str = toml::to_string_pretty(&cfg).unwrap_or_else(|e| {
            self.config_error = Some(format!("序列化失敗: {}", e));
            String::new()
        });
        if toml_str.is_empty() { return; }

        match fs::write(&config_path, &toml_str) {
            Ok(_) => {
                self.config_error = None;
                self.raw_text = toml_str;
                self.status_message = "已儲存，daemon 將自動重新載入".into();
            }
            Err(e) => {
                self.config_error = Some(format!("寫入失敗: {}", e));
            }
        }
    }

    /// 將編輯中的表單資料轉回 Config
    fn build_config(&self) -> Config {
        Config {
            config_version: Some(3),
            app_setting: self.edit_app.to_app_settings(),
            tasks: self.edit_tasks.iter().map(|t| t.to_folder_task()).collect(),
        }
    }

    // ── 任務管理 ────────────────────────────────────────────────────────────

    fn add_task(&mut self) {
        self.edit_tasks.push(EditFolderTask {
            folder_path: String::new(),
            target_text: String::new(),
            exclude_text: String::new(),
            really_delete: false,
            enabled: true,
            follow_symlinks: false,
            scan_mode: "both".into(),
            threshold_day: 7,
            threshold_hour: 0,
            threshold_minute: 0,
            sched_start: String::new(),
            sched_end: String::new(),
        });
        self.expanded_task = Some(self.edit_tasks.len() - 1);
    }

    fn remove_task(&mut self, idx: usize) {
        if idx < self.edit_tasks.len() {
            self.edit_tasks.remove(idx);
            self.expanded_task = None;
        }
    }

    // ── 掃描／狀態 ──────────────────────────────────────────────────────────

    fn trigger_scan_now(&mut self) {
        let signal_path = self.path(SIGNAL_PATH);
        match fs::write(&signal_path, "scan_now") {
            Ok(_) => self.status_message = "掃描指令已發送".into(),
            Err(e) => self.status_message = format!("發送失敗: {}", e),
        }
    }

    fn is_daemon_running(&self) -> bool {
        let pid_path = self.path(PID_PATH);
        pid_path.exists()
            && fs::read_to_string(&pid_path)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
                .map_or(false, |pid| pid > 0)
    }

    fn load_db_stats(&mut self) {
        let temp_path = self.path(TEMP_BIN_PATH);
        match fs::metadata(&temp_path) {
            Ok(meta) => {
                let size_mb = meta.len() as f64 / (1024.0 * 1024.0);
                self.db_stats = format!(
                    "資料庫: {}\n大小: {:.2} MB\n更新: {:?}",
                    temp_path.display(), size_mb,
                    meta.modified().map(|t| format!("{:?}", t)).unwrap_or_default()
                );
            }
            Err(_) => self.db_stats = "資料庫尚未建立".into(),
        }
    }

    fn load_log_tail(&mut self) {
        let log_path = self.path(LOG_PATH);
        match fs::read_to_string(&log_path) {
            Ok(content) => {
                let lines: Vec<&str> = content.lines().collect();
                let start = if lines.len() > 100 { lines.len() - 100 } else { 0 };
                self.log_text = lines[start..].join("\n");
            }
            Err(_) => self.log_text = "日誌尚未產生".into(),
        }
    }

    fn refresh_all(&mut self) {
        self.load_db_stats();
        self.load_log_tail();
        self.status_message = "已重新整理".into();
    }
}

// ─── egui 應用入口 ────────────────────────────────────────────────────────────

impl eframe::App for AppState {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        TopBottomPanel::top("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let running = self.is_daemon_running();
                ui.colored_label(
                    if running { egui::Color32::GREEN } else { egui::Color32::RED },
                    if running { "daemon 執行中" } else { "daemon 未執行" }
                );
                ui.separator();
                ui.label(&self.status_message);
            });
        });

        SidePanel::left("nav_panel").min_width(140.0).resizable(false).show(ctx, |ui| {
            ui.add_space(12.0);
            ui.heading("導航");
            ui.add_space(12.0);
            for tab in &[Tab::Settings, Tab::Status, Tab::Log, Tab::Actions] {
                let is_active = self.active_tab == *tab;
                if ui.add_sized(
                    egui::vec2(ui.available_width(), 40.0),
                    egui::Button::new(egui::RichText::new(tab.name()).size(14.0))
                        .fill(if is_active { egui::Color32::from_rgb(74, 144, 217) } else { egui::Color32::TRANSPARENT }),
                ).clicked() { self.active_tab = *tab; }
            }
        });

        CentralPanel::default().show(ctx, |ui| match self.active_tab {
            Tab::Settings => self.show_settings_tab(ui),
            Tab::Status => self.show_status_tab(ui),
            Tab::Log => self.show_log_tab(ui),
            Tab::Actions => self.show_actions_tab(ui),
        });
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  設定編輯頁 — 互動式表單
// ═══════════════════════════════════════════════════════════════════════════════

impl AppState {
    fn show_settings_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("設定編輯");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("儲存").clicked() { self.save_config(); }
                if ui.button("重新載入").clicked() { self.load_config(); }
                if ui.toggle_value(&mut self.show_raw, "RAW 模式").clicked() {
                    if self.show_raw {
                        let cfg = self.build_config();
                        self.raw_text = toml::to_string_pretty(&cfg).unwrap_or_default();
                    }
                }
            });
        });

        ui.separator();
        ui.add_space(4.0);

        if let Some(ref e) = self.config_error {
            ui.colored_label(egui::Color32::RED, e);
            ui.add_space(4.0);
        }

        if self.show_raw {
            // RAW TOML 編輯器（後備方案）
            let font_id = FontId::monospace(13.0);
            ScrollArea::vertical().max_height(ui.available_height()).show(ui, |ui| {
                ui.add(egui::TextEdit::multiline(&mut self.raw_text)
                    .font(font_id).code_editor().desired_width(f32::INFINITY).desired_rows(30));
            });
            return;
        }

        ScrollArea::vertical().max_height(ui.available_height()).show(ui, |ui| {
            // ── 一般設定 ──────────────────────────────────────────────────────
            ui.group(|ui| {
                ui.label(egui::RichText::new("一般設定").size(15.0).strong());
                ui.add_space(4.0);

                egui::Grid::new("app_grid").striped(true).num_columns(4).spacing([8.0, 4.0]).show(ui, |ui| {
                    // 快速掃描間隔
                    ui.label("快速掃描間隔（分鐘）");
                    ui.add(egui::Slider::new(&mut self.edit_app.small_scan_interval, 5..=240).suffix(" 分鐘").clamping(egui::SliderClamping::Never));
                    ui.label("完整掃描間隔（分鐘）");
                    ui.add(egui::Slider::new(&mut self.edit_app.complete_scan_interval, 15..=480).suffix(" 分鐘").clamping(egui::SliderClamping::Never));
                    ui.end_row();

                    ui.label("測試模式");
                    ui.checkbox(&mut self.edit_app.test_mode, "只記錄不刪除");
                    ui.label("啟動時掃描");
                    ui.checkbox(&mut self.edit_app.scan_on_startup, "啟動後立即執行一次");
                    ui.end_row();

                    ui.label("閒置閾值（分鐘）");
                    ui.add(egui::Slider::new(&mut self.edit_app.idle_threshold_min, 1..=60).suffix(" 分鐘"));
                    ui.label("記憶體上限（MB）");
                    ui.add(egui::Slider::new(&mut self.edit_app.max_memory_mb, 10..=500).suffix(" MB"));
                    ui.end_row();

                    ui.label("通知等級");
                    egui::ComboBox::from_id_salt("notif_level")
                        .selected_text(&self.edit_app.notification_level)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.edit_app.notification_level, "none".into(), "無");
                            ui.selectable_value(&mut self.edit_app.notification_level, "summary".into(), "摘要");
                            ui.selectable_value(&mut self.edit_app.notification_level, "verbose".into(), "詳細");
                        });
                    ui.label("日誌上限（MB）");
                    ui.add(egui::Slider::new(&mut self.edit_app.log_max_size_mb, 1..=100).suffix(" MB"));
                    ui.end_row();
                });
            });

            ui.add_space(12.0);

            // ── 任務列表 ──────────────────────────────────────────────────────
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("任務列表").size(15.0).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("新增任務").clicked() { self.add_task(); }
                    });
                });
            });
            ui.add_space(4.0);

            let mut to_remove: Option<usize> = None;
            for (i, task) in self.edit_tasks.iter_mut().enumerate() {
                let expanded = self.expanded_task == Some(i);
                let header_text = format!(
                    "{} {}",
                    if task.folder_path.is_empty() { "(未設定路徑)" } else { &task.folder_path },
                    if task.enabled { "" } else { " [停用]" }
                );
                CollapsingHeader::new(header_text)
                    .default_open(expanded)
                    .id_salt(format!("task_{}", i))
                    .show(ui, |ui| {
                        self.expanded_task = Some(i);

                        // 路徑 + 啟用
                        ui.horizontal(|ui| {
                            ui.label("資料夾路徑");
                            if ui.button("選取").clicked() {
                                if let Some(dir) = FileDialog::new().set_title("選擇任務資料夾").pick_folder() {
                                    task.folder_path = dir.to_string_lossy().to_string();
                                }
                            }
                        });
                        ui.add(egui::TextEdit::singleline(&mut task.folder_path)
                            .hint_text("C:/Users/.../Downloads")
                            .desired_width(f32::INFINITY));

                        ui.horizontal(|ui| {
                            ui.checkbox(&mut task.enabled, "啟用此任務");
                            ui.separator();
                            ui.checkbox(&mut task.really_delete, "徹底刪除（不進垃圾桶）");
                            ui.separator();
                            ui.checkbox(&mut task.follow_symlinks, "跟隨符號連結");
                        });

                        ui.add_space(6.0);
                        ui.separator();
                        ui.add_space(6.0);

                        // 閾值
                        egui::Grid::new(format!("thresh_{}", i)).num_columns(6).spacing([4.0, 2.0]).show(ui, |ui| {
                            ui.add(egui::Slider::new(&mut task.threshold_day, 0..=365).clamping(egui::SliderClamping::Never));
                            ui.label("天");
                            ui.add(egui::Slider::new(&mut task.threshold_hour, 0..=23).clamping(egui::SliderClamping::Never));
                            ui.label("小時");
                            ui.add(egui::Slider::new(&mut task.threshold_minute, 0..=59).clamping(egui::SliderClamping::Never));
                            ui.label("分鐘");
                            ui.end_row();
                        });

                        ui.add_space(6.0);
                        ui.separator();
                        ui.add_space(6.0);

                        // 掃描模式 + 年齡
                        egui::Grid::new(format!("taskopt_{}", i)).num_columns(4).spacing([8.0, 4.0]).show(ui, |ui| {
                            ui.label("掃描模式");
                            egui::ComboBox::from_id_salt(format!("scanmode_{}", i))
                                .selected_text(&task.scan_mode)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut task.scan_mode, "both".into(), "兩者");
                                    ui.selectable_value(&mut task.scan_mode, "small_only".into(), "僅快速");
                                    ui.selectable_value(&mut task.scan_mode, "complete_only".into(), "僅完整");
                                });
                        });

                        ui.add_space(6.0);
                        ui.separator();
                        ui.add_space(6.0);

                        // 時間窗
                        ui.label("排程時間窗（可選，留空=不限制）");
                        ui.horizontal(|ui| {
                            ui.label("開始");
                            ui.add(egui::TextEdit::singleline(&mut task.sched_start).hint_text("HH:MM (如 02:00)"));
                            ui.label("結束");
                            ui.add(egui::TextEdit::singleline(&mut task.sched_end).hint_text("HH:MM (如 06:00)"));
                        });

                        ui.add_space(6.0);
                        ui.separator();
                        ui.add_space(6.0);

                        // glob 模式編輯區
                        ui.label("目標模式（一行一個 glob，留空=所有檔案）");
                        ui.add(egui::TextEdit::multiline(&mut task.target_text)
                            .desired_rows(3).desired_width(f32::INFINITY)
                            .hint_text("# 例：\n*.tmp\n**/temp/**\nTemp_*"));

                        ui.label("排除模式（一行一個 glob，永遠不刪）");
                        ui.add(egui::TextEdit::multiline(&mut task.exclude_text)
                            .desired_rows(3).desired_width(f32::INFINITY)
                            .hint_text("*important*\n*.keep"));

                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button(egui::RichText::new("刪除此任務").color(egui::Color32::RED)).clicked() {
                                    to_remove = Some(i);
                                }
                            });
                        });
                    });
                ui.add_space(4.0);
            }

            if let Some(idx) = to_remove { self.remove_task(idx); }
        });
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  狀態檢視頁
// ═══════════════════════════════════════════════════════════════════════════════

impl AppState {
    fn show_status_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("任務狀態");
        ui.add_space(12.0);

        let config_path = self.path(CONFIG_PATH);
        if let Ok(content) = fs::read_to_string(&config_path) {
            if let Ok(cfg) = toml::from_str::<Config>(&content) {
                egui::Grid::new("cfg_grid").striped(true).show(ui, |ui| {
                    ui.label("掃描模式");
                    ui.label(if cfg.app_setting.test_mode.unwrap_or(false) { "測試模式" } else { "正式模式" }); ui.end_row();
                    ui.label("快速掃描間隔"); ui.label(format!("{} 分鐘", cfg.app_setting.small_scan_interval)); ui.end_row();
                    ui.label("完整掃描間隔"); ui.label(format!("{} 分鐘", cfg.app_setting.complete_scan_interval)); ui.end_row();
                    ui.label("任務數量"); ui.label(format!("{} 個", cfg.tasks.len())); ui.end_row();
                    for (i, t) in cfg.tasks.iter().enumerate() {
                        let enable = if t.is_enabled() { "✔" } else { "✘" };
                        ui.label(format!("  任務{}", i+1));
                        ui.label(format!("{} {} | 閾值 {}d {}h {}m | {}",
                            enable, t.folder_path.display(),
                            t.threshold.day, t.threshold.hour, t.threshold.minute,
                            if t.really_delete.unwrap_or(false) { "徹底刪除" } else { "垃圾桶" }));
                        ui.end_row();
                    }
                });
            }
        }

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);
        ui.heading("資料庫");
        ui.label(&self.db_stats);
        ui.add_space(8.0);
        if ui.button("重新整理").clicked() { self.refresh_all(); }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  日誌頁
// ═══════════════════════════════════════════════════════════════════════════════

impl AppState {
    fn show_log_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("系統日誌");
        ui.label("run.log 最後 100 行");
        ui.add_space(8.0);
        if ui.button("重新讀取").clicked() { self.load_log_tail(); }
        ui.add_space(8.0);

        let font_id = FontId::monospace(11.0);
        ScrollArea::vertical().max_height(ui.available_height() - 20.0).show(ui, |ui| {
            ui.add(egui::TextEdit::multiline(&mut self.log_text)
                .font(font_id).interactive(false).desired_width(f32::INFINITY).desired_rows(28));
        });
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  操作面板頁
// ═══════════════════════════════════════════════════════════════════════════════

impl AppState {
    fn show_actions_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("操作面板");
        ui.add_space(16.0);

        ui.group(|ui| {
            ui.label(egui::RichText::new("掃描操作").size(15.0).strong());
            ui.add_space(8.0);
            if ui.add_sized(egui::vec2(200.0, 36.0),
                egui::Button::new(egui::RichText::new("立即掃描").size(16.0))).clicked() {
                self.trigger_scan_now();
            }
        });

        ui.add_space(16.0);

        ui.group(|ui| {
            ui.label(egui::RichText::new("設定操作").size(15.0).strong());
            ui.add_space(8.0);
            if ui.button("用系統編輯器開啟 config.toml").clicked() {
                if let Err(e) = open::that(&self.path(CONFIG_PATH)) {
                    self.status_message = format!("開啟失敗: {}", e);
                }
            }
            ui.add_space(8.0);
            if ui.button("清除 temp.bin 資料庫").clicked() {
                let p = self.path(TEMP_BIN_PATH);
                if p.exists() {
                    if let Err(e) = fs::remove_file(&p) { self.status_message = format!("清除失敗: {}", e); }
                    else { self.status_message = "資料庫已清除".into(); self.load_db_stats(); }
                } else { self.status_message = "資料庫不存在".into(); }
            }
        });

        ui.add_space(16.0);

        ui.group(|ui| {
            ui.label(egui::RichText::new("Daemon 控制").size(15.0).strong());
            ui.add_space(8.0);
            let running = self.is_daemon_running();
            ui.label(if running { "執行中" } else { "未執行" });
            if ui.button("啟動 daemon").clicked() {
                let daemon_path = self.working_dir.join("onee_sweeper_daemon.exe");
                match std::process::Command::new(&daemon_path).spawn() {
                    Ok(_) => self.status_message = "Daemon 已啟動".into(),
                    Err(e) => self.status_message = format!("啟動失敗: {}", e),
                }
            }
        });

        ui.add_space(16.0);

        ui.group(|ui| {
            ui.label(egui::RichText::new("關於").size(15.0).strong());
            ui.add_space(4.0);
            ui.label(APP_TITLE);
        });
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  main
// ═══════════════════════════════════════════════════════════════════════════════

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([WINDOW_W, WINDOW_H])
            .with_resizable(true)
            .with_min_inner_size([600.0, 400.0]),
        ..Default::default()
    };

    eframe::run_native(APP_TITLE, options, Box::new(|cc| {
        let mut fonts = egui::FontDefinitions::default();
        if let Ok(font_dir) = exe_font_dir() { load_fonts_from_dir(&font_dir, &mut fonts); }
        cc.egui_ctx.set_fonts(fonts);

        let working_dir = std::env::current_exe()
            .ok().and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));

        let mut app = AppState {
            active_tab: Tab::Settings,
            edit_app: EditAppSettings {
                small_scan_interval: 30, complete_scan_interval: 120, test_mode: true,
                log_max_size_mb: 10, idle_threshold_min: 5, max_memory_mb: 50,
                notification_level: "summary".into(), scan_on_startup: true,
            },
            edit_tasks: Vec::new(),
            config_error: None,
            status_message: "就緒".into(),
            db_stats: String::new(),
            log_text: String::new(),
            working_dir,
            show_raw: false,
            raw_text: String::new(),
            expanded_task: None,
        };
        app.load_config();
        app.load_db_stats();
        app.load_log_tail();
        Ok(Box::new(app))
    }))
}

/// ### 取得 exe 同目錄下的 fonts/ 資料夾路徑
fn exe_font_dir() -> Result<PathBuf, std::io::Error> {
    let exe = std::env::current_exe()?;
    let exe_dir = exe.parent().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "無法取得執行檔目錄"))?;
    Ok(exe_dir.join("fonts"))
}

/// ### 從目錄載入所有 .ttf / .otf 字體到 FontDefinitions
fn load_fonts_from_dir(dir: &Path, fonts: &mut egui::FontDefinitions) {
    let entries = match std::fs::read_dir(dir) { Ok(e) => e, Err(_) => return };
    for entry in entries.flatten() {
        let path = entry.path();
        let ext = match path.extension().and_then(|e| e.to_str()) { Some(e) => e.to_lowercase(), None => continue };
        if ext != "ttf" && ext != "otf" { continue; }
        let data = match std::fs::read(&path) { Ok(d) => d, Err(e) => { eprintln!("⚠ 字體讀取失敗 {}: {}", path.display(), e); continue; }};
        let name = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
        fonts.font_data.insert(name.clone(), egui::FontData::from_owned(data).into());
        fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(0, name.clone());
        fonts.families.entry(egui::FontFamily::Monospace).or_default().insert(0, name);
    }
    eprintln!("已載入自訂字體");
}
