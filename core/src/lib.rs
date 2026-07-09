//! ONEE SWEEPER v2.0 — 共享核心模組
//!
//! 提供設定結構定義（`type_define`）、設定載入與校驗（`config`），
//! 供 daemon（背景服務）與 UI（設定面板）共用，避免重複定義。

pub mod type_define;
pub mod config;
