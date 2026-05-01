//! Codex Switcher - Tauri main entry
//!
//! Expose all Tauri commands for frontend

mod account;
mod ide_control;
mod oauth;
mod oauth_server;
mod proxy;
mod refresh_lock;
mod scheduler;
mod skills;
mod switch_log;
mod token_tracker;
mod tray;
mod usage;

use account::{Account, AccountStore};
use chrono::Utc;
use refresh_lock::RefreshLockManager;
use tauri::{Emitter, Manager, State};
use usage::{UsageDisplay, UsageFetcher};
use std::net::{IpAddr, Ipv4Addr, UdpSocket};

const QUARANTINE_FIX_TICKET_TTL_SECS: i64 = 120;

#[derive(Clone, Debug)]
struct QuarantineFixTicket {
    value: String,
    expires_at: chrono::DateTime<Utc>,
}

fn allow_local_refresh_for_quota(is_current: bool) -> bool {
    let _ = is_current;
    // Disable local refresh on quota query path. Prevents non-current accounts from consuming old refresh_token.
    false
}

fn detect_sync_conflict_for_current(
    account: &Account,
    disk_auth: &serde_json::Value,
) -> Option<String> {
    // Don't show "Token conflict" when identities don't match to avoid false positives
    if !AccountStore::auth_identity_matches(&account.auth_json, disk_auth) {
        return None;
    }

    let official_rt = AccountStore::extract_refresh_token(disk_auth);
    let local_rt = AccountStore::extract_refresh_token(&account.auth_json);

    // If official Token exists and differs from local (usually updated), treat as conflict
    if official_rt.is_some() && official_rt != local_rt {
        let disk_email =
            AccountStore::extract_email(disk_auth).unwrap_or_else(|| "Unknown account".to_string());
        if disk_email == account.name {
            return Some(account.name.clone());
        } else {
            return Some(format!("{} ({})", account.name, disk_email));
        }
    }

    None
}

/// App state
pub struct AppState {
    pub store: std::sync::Arc<std::sync::Mutex<AccountStore>>,
    pub scheduler: std::sync::Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    pub proxy_handle: std::sync::Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    pub proxy_stats: std::sync::Arc<proxy::ProxyStats>,
    pub token_tracker: std::sync::Arc<token_tracker::TokenTracker>,
    /// Notify all WebSocket connections to reconnect on switch
    pub ws_disconnect: std::sync::Arc<tokio::sync::Notify>,
    pub switch_logger: std::sync::Arc<switch_log::SwitchLogger>,
    pub quota_refresh_handle: std::sync::Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    pub refresh_locks: RefreshLockManager,
    quarantine_fix_ticket: std::sync::Mutex<Option<QuarantineFixTicket>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            store: std::sync::Arc::new(std::sync::Mutex::new(AccountStore::load())),
            scheduler: std::sync::Mutex::new(None),
            proxy_handle: std::sync::Mutex::new(None),
            proxy_stats: std::sync::Arc::new(proxy::ProxyStats::default()),
            token_tracker: token_tracker::TokenTracker::new(),
            ws_disconnect: std::sync::Arc::new(tokio::sync::Notify::new()),
            switch_logger: switch_log::SwitchLogger::new(),
            quota_refresh_handle: std::sync::Mutex::new(None),
            refresh_locks: RefreshLockManager::default(),
            quarantine_fix_ticket: std::sync::Mutex::new(None),
        }
    }

    fn issue_quarantine_fix_ticket(&self) -> Result<String, String> {
        let ticket = uuid::Uuid::new_v4().to_string();
        let expires_at = Utc::now() + chrono::Duration::seconds(QUARANTINE_FIX_TICKET_TTL_SECS);
        let mut slot = self
            .quarantine_fix_ticket
            .lock()
            .map_err(|e| e.to_string())?;
        *slot = Some(QuarantineFixTicket {
            value: ticket.clone(),
            expires_at,
        });
        Ok(ticket)
    }

    fn consume_quarantine_fix_ticket(&self, provided_ticket: &str) -> Result<(), String> {
        let mut slot = self
            .quarantine_fix_ticket
            .lock()
            .map_err(|e| e.to_string())?;
        let now = Utc::now();
        match slot.take() {
            Some(stored) if stored.expires_at < now => {
                Err("Security confirmation expired, please click fix again".to_string())
            }
            Some(stored) if stored.value != provided_ticket => {
                Err("Security confirmation invalid, please click fix again".to_string())
            }
            Some(_) => Ok(()),
            None => Err("Missing security confirmation, please click fix again".to_string()),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// Get all accounts
#[tauri::command]
fn get_accounts(state: State<AppState>) -> Result<Vec<Account>, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    Ok(store.list_accounts().into_iter().cloned().collect())
}

/// Get current active account ID
#[tauri::command]
fn get_current_account_id(state: State<AppState>) -> Result<Option<String>, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    Ok(store.current.clone())
}

/// Get global settings
#[tauri::command]
fn get_settings(state: State<AppState>) -> Result<account::AppSettings, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    Ok(store.settings.clone())
}

/// Proxy status info
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProxyStatus {
    pub enabled: bool,
    pub port: u16,
    pub is_running: bool,
    pub base_url: String,
    pub allow_lan: bool,
    pub lan_base_url: Option<String>,
    pub total_requests: u64,
    pub auto_switches: u64,
}

fn detect_lan_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(1, 1, 1, 1), 80)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_loopback() => Some(ip),
        _ => None,
    }
}

/// Get proxy status
#[tauri::command]
fn get_proxy_status(state: State<AppState>) -> Result<ProxyStatus, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    let is_running = state
        .proxy_handle
        .lock()
        .map(|h| h.is_some())
        .unwrap_or(false);
    Ok(ProxyStatus {
        enabled: store.settings.proxy_enabled,
        port: store.settings.proxy_port,
        is_running,
        base_url: format!("http://localhost:{}/v1", store.settings.proxy_port),
        allow_lan: store.settings.proxy_allow_lan,
        lan_base_url: if store.settings.proxy_allow_lan {
            detect_lan_ipv4().map(|ip| format!("http://{}:{}/v1", ip, store.settings.proxy_port))
        } else {
            None
        },
        total_requests: state
            .proxy_stats
            .total_requests
            .load(std::sync::atomic::Ordering::Relaxed),
        auto_switches: state
            .proxy_stats
            .auto_switches
            .load(std::sync::atomic::Ordering::Relaxed),
    })
}

/// Update global settings
#[tauri::command]
fn update_settings(
    state: State<AppState>,
    app: tauri::AppHandle,
    settings: account::AppSettings,
) -> Result<(), String> {
    let (
        prev_bg_refresh,
        prev_proxy_enabled,
        prev_proxy_port,
        prev_proxy_allow_lan,
        prev_quota_refresh,
    ) = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        let prev = (
            store.settings.background_refresh,
            store.settings.proxy_enabled,
            store.settings.proxy_port,
            store.settings.proxy_allow_lan,
            store.settings.quota_refresh_enabled,
        );
        store.settings = settings.clone();
        store.save()?;
        prev
    };

    // Refresh tray menu text (sync update "next account" preview)
    crate::tray::update_tray_menu(&app);

    // Background refresh lifecycle
    let mut scheduler_handle = state.scheduler.lock().map_err(|e| e.to_string())?;
    match (prev_bg_refresh, settings.background_refresh) {
        (false, true) => {
            if scheduler_handle.is_none() {
                let handle = scheduler::start(state.store.clone(), app.clone());
                *scheduler_handle = Some(handle);
            }
        }
        (true, false) => {
            if let Some(handle) = scheduler_handle.take() {
                handle.abort();
            }
        }
        _ => {}
    }

    // Proxy lifecycle
    let mut proxy_handle = state.proxy_handle.lock().map_err(|e| e.to_string())?;
    let proxy_config_changed =
        prev_proxy_port != settings.proxy_port || prev_proxy_allow_lan != settings.proxy_allow_lan;
    match (prev_proxy_enabled, settings.proxy_enabled) {
        (false, true) => {
            if proxy_handle.is_none() {
                let handle = proxy::start(
                    state.store.clone(),
                    settings.proxy_port,
                    settings.proxy_allow_lan,
                    app.clone(),
                    state.proxy_stats.clone(),
                    state.token_tracker.clone(),
                    state.ws_disconnect.clone(),
                    state.switch_logger.clone(),
                );
                *proxy_handle = Some(handle);
                println!("[Proxy] Proxy started (port {})", settings.proxy_port);
            }
        }
        (true, false) => {
            if let Some(handle) = proxy_handle.take() {
                handle.abort();
                println!("[Proxy] Proxy stopped");
            }
        }
        (true, true) if proxy_config_changed => {
            if let Some(handle) = proxy_handle.take() {
                handle.abort();
            }
            let handle = proxy::start(
                state.store.clone(),
                settings.proxy_port,
                settings.proxy_allow_lan,
                app.clone(),
                state.proxy_stats.clone(),
                state.token_tracker.clone(),
                state.ws_disconnect.clone(),
                state.switch_logger.clone(),
            );
            *proxy_handle = Some(handle);
            println!(
                "[Proxy] Proxy restarted (port {}, LAN access: {})",
                settings.proxy_port, settings.proxy_allow_lan
            );
        }
        _ => {}
    }

    // Scheduled quota refresh lifecycle
    let mut qr_handle = state
        .quota_refresh_handle
        .lock()
        .map_err(|e| e.to_string())?;
    match (prev_quota_refresh, settings.quota_refresh_enabled) {
        (false, true) => {
            if qr_handle.is_none() {
                let handle = start_quota_refresh(state.store.clone(), app.clone());
                *qr_handle = Some(handle);
                println!("[QuotaRefresh] Scheduled quota refresh started");
            }
        }
        (true, false) => {
            if let Some(handle) = qr_handle.take() {
                handle.abort();
                println!("[QuotaRefresh] Scheduled quota refresh stopped");
            }
        }
        _ => {}
    }

    app.emit("settings-updated", ()).ok();
    Ok(())
}

/// Import account from current Codex login state
#[tauri::command]
fn import_current_account(
    state: State<AppState>,
    app: tauri::AppHandle,
    name: String,
    notes: Option<String>,
) -> Result<Account, String> {
    let auth_json = AccountStore::read_codex_auth()?;
    if AccountStore::extract_refresh_token(&auth_json).is_none() {
        return Err("Current auth.json missing refresh_token, cannot auto-renew. Please sign in again".to_string());
    }

    let account = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        let account = store.add_account(name, auth_json, notes);
        store.save()?;
        account
    };
    crate::tray::update_tray_menu(&app);
    Ok(account)
}

// is_token_expired removed: align with Codex last_refresh-based refresh

/// Check if account in current IDE has unsynced Token updates
#[tauri::command]
fn check_sync_conflict(state: State<AppState>) -> Result<Option<String>, String> {
    let auth_json = match AccountStore::read_codex_auth() {
        Ok(a) => a,
        Err(_) => return Ok(None), // If read fails (file not found etc.), treat as no conflict
    };

    let store = state.store.lock().map_err(|e| e.to_string())?;

    // Check if auth.json belongs to current active account and if content changed
    if let Some(current_id) = &store.current {
        if let Some(account) = store.accounts.get(current_id) {
            if let Some(name) = detect_sync_conflict_for_current(account, &auth_json) {
                return Ok(Some(name));
            }
        }
    }

    Ok(None)
}

/// Delete account
#[tauri::command]
fn delete_account(state: State<AppState>, app: tauri::AppHandle, id: String) -> Result<(), String> {
    {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        if store.current.as_deref() == Some(&id) {
            store.current = None;
        }
        store.delete_account(&id)?;
        store.save()?;
    } // Lock released here
      // Refresh tray menu (needs re-acquire lock, no deadlock)
    crate::tray::update_tray_menu(&app);
    Ok(())
}

/// Update account info
#[tauri::command]
fn update_account(
    state: State<AppState>,
    app: tauri::AppHandle,
    id: String,
    name: Option<String>,
    notes: Option<String>,
) -> Result<(), String> {
    {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store.update_account(&id, name, notes)?;
        store.save()?;
    }
    crate::tray::update_tray_menu(&app);
    Ok(())
}

/// Set account-level "inactive keep-alive refresh" toggle
#[tauri::command]
fn set_account_inactive_refresh_enabled(
    state: State<AppState>,
    id: String,
    enabled: bool,
) -> Result<(), String> {
    let mut store = state.store.lock().map_err(|e| e.to_string())?;
    store.set_inactive_refresh_enabled(&id, enabled)?;
    store.save()?;
    Ok(())
}

/// Export all account configurations
#[tauri::command]
fn export_accounts(state: State<AppState>) -> Result<String, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    store.export()
}

/// Import account configuration
#[tauri::command]
fn import_accounts(
    state: State<AppState>,
    app: tauri::AppHandle,
    json: String,
) -> Result<(), String> {
    let new_store = AccountStore::import(&json)?;
    let missing = new_store.accounts_missing_refresh_token();
    if !missing.is_empty() {
        return Err(format!(
            "The following accounts are missing refresh_token and cannot auto-renew. Please sign in again before importing: {}",
            missing.join(", ")
        ));
    }
    {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        *store = new_store;
        store.save()?;
    }
    crate::tray::update_tray_menu(&app);
    Ok(())
}

/// Complete OAuth login and save account
#[tauri::command]
async fn finalize_oauth_login(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
    code: String,
) -> Result<Account, String> {
    let token_res = oauth_server::complete_oauth_login(code).await?;
    if token_res.refresh_token.is_none() {
        return Err("OAuth did not return refresh_token, cannot auto-renew".to_string());
    }

    let user_info = token_res
        .id_token
        .as_ref()
        .and_then(|id_t| oauth::parse_user_info(id_t))
        .ok_or("Failed to parse user info from authorization response (Missing ID Token)")?;

    let account = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;

        let expires_at = token_res
            .expires_in
            .map(|secs| (chrono::Utc::now() + chrono::Duration::seconds(secs as i64)).to_rfc3339());

        let auth_json = serde_json::json!({
            "tokens": {
                "access_token": token_res.access_token,
                "refresh_token": token_res.refresh_token,
                "id_token": token_res.id_token,
                "account_id": user_info.account_id,
                "expires_at": expires_at
            },
            "last_refresh": chrono::Utc::now().to_rfc3339()
        });

        let mut account = store.add_account(
            user_info.email,
            auth_json,
            Some("OpenAI OAuth Login".to_string()),
        );

        account.refresh_token = token_res.refresh_token.clone();
        if let Some(acc) = store.accounts.get_mut(&account.id) {
            acc.refresh_token = token_res.refresh_token;
        }

        store.save()?;
        account
    };
    crate::tray::update_tray_menu(&app);
    Ok(account)
}

// Adding helper methods to AppState for accessing AppHandle in finalize_oauth_login won't work
// because finalize_oauth_login is async and the Command macro handles it.
// We directly add AppHandle parameter to finalize_oauth_login.

/// Switch to specified account (async version, no local Token refresh)
#[tauri::command]
async fn switch_account(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
    id: String,
) -> Result<(), String> {
    // 0. Before switching, sync "current active account" with official auth.json only
    if let Ok(current_auth) = AccountStore::read_codex_auth() {
        if let Ok(mut store) = state.store.lock() {
            if let Some(current_id) = store.current.clone() {
                if store.sync_account_from_auth_json(&current_id, current_auth) {
                    if let Err(e) = store.save() {
                        eprintln!("[Sync] Failed to save current account: {}", e);
                    }
                }
            }
        }
    }

    // 1. Get target account verification credentials
    let (target_id, access_token, refresh_token, account_id) = {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        let account = store
            .accounts
            .get(&id)
            .ok_or_else(|| format!("Account {} not found", id))?;

        let access_token = account
            .auth_json
            .get("tokens")
            .and_then(|t| t.get("access_token"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or("Account missing access_token")?;

        let refresh_token = account.refresh_token.clone();

        let account_id = account
            .auth_json
            .get("account_id")
            .or_else(|| {
                account
                    .auth_json
                    .get("tokens")
                    .and_then(|t| t.get("account_id"))
            })
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        (account.id.clone(), access_token, refresh_token, account_id)
    };

    // 1.5. Check if JWT expired, refresh if so
    let (access_token, refresh_token) = {
        let mut needs_refresh = false;
        if let Ok(claims) = AccountStore::extract_jwt_claims_from_token(&access_token) {
            if let Some(exp) = claims.get("exp").and_then(|v| v.as_i64()) {
                let now = Utc::now().timestamp();
                // If remaining time < 5 minutes, trigger refresh
                if exp - now < 300 {
                    println!("[Switch] JWT expired or expiring soon ({}), triggering auto-refresh", exp);
                    needs_refresh = true;
                }
            }
        } else {
            println!("[Switch] Cannot parse JWT Claims, attempting blind refresh");
            needs_refresh = true;
        }

        if needs_refresh && refresh_token.is_some() {
            if let Some(ref rt) = refresh_token {
                match oauth::refresh_access_token(rt).await {
                    Ok(token_res) => {
                        println!("[Switch] Auto-refresh Token success");
                        let mut store = state.store.lock().map_err(|e| e.to_string())?;
                        if let Some(account) = store.accounts.get_mut(&target_id) {
                            AccountStore::apply_refreshed_tokens(
                                account,
                                token_res.access_token.clone(),
                                token_res.refresh_token.clone(),
                                token_res.id_token,
                                token_res.expires_in,
                            );
                            if let Err(e) = store.save() {
                                eprintln!("[Store] Save failed: {}", e);
                            }
                            (token_res.access_token, token_res.refresh_token)
                        } else {
                            (access_token, refresh_token)
                        }
                    }
                    Err(e) => {
                        println!("[Switch] Auto-refresh Token failed: {}", e);
                        (access_token, refresh_token)
                    }
                }
            } else {
                (access_token, refresh_token)
            }
        } else {
            (access_token, refresh_token)
        }
    };

    // 2. Pre-check (non-blocking): Only try reading quota cache, no local refresh_token refresh.
    // Failures don't block switching; let Codex handle token lifecycle on-demand.
    println!(
        "[Switch] Pre-check target account quota (no local refresh trigger): {}",
        target_id
    );
    match usage::UsageFetcher::fetch_usage_direct(access_token, account_id, refresh_token, false)
        .await
    {
        Ok((usage, _)) => {
            let mut store = state.store.lock().map_err(|e| e.to_string())?;
            if let Some(account) = store.accounts.get_mut(&target_id) {
                account.cached_quota = Some(account::CachedQuota {
                    five_hour_left: usage.five_hour_left as f64,
                    five_hour_reset: usage.five_hour_reset.clone(),
                    five_hour_reset_at: usage.five_hour_reset_at,
                    five_hour_label: usage.five_hour_label.clone(),
                    weekly_left: usage.weekly_left as f64,
                    weekly_reset: usage.weekly_reset.clone(),
                    weekly_reset_at: usage.weekly_reset_at,
                    weekly_label: usage.weekly_label.clone(),
                    plan_type: usage.plan_type.clone(),
                    is_valid_for_cli: usage.is_valid_for_cli,
                    updated_at: chrono::Utc::now(),
                });
                if let Err(e) = store.save() {
                    eprintln!("[Store] Save failed: {}", e);
                }
            }
        }
        Err(e) => {
            println!("[Switch] Pre-check quota failed (ignored, doesn't block switch): {}", e);
        }
    }

    // 3. Execute switch (write auth.json)
    println!("[Switch] Executing switch...");
    if !state
        .refresh_locks
        .acquire(&target_id, tokio::time::Duration::from_secs(5))
        .await
    {
        return Err("This account is being refreshed by another process, please try again later".to_string());
    }
    let switch_result: Result<(), String> = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        match store.switch_to(&target_id) {
            Ok(()) => store.save(),
            Err(e) => Err(e),
        }
    };
    state.refresh_locks.release(&target_id).await;
    switch_result?;
    println!("[Switch] Switch complete!");

    // Log account switch
    {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        let from_name = store
            .accounts
            .values()
            .find(|a| Some(&a.id) != store.current.as_ref() && a.last_used.is_some())
            .map(|a| a.name.clone());
        let to_name = store
            .accounts
            .get(&target_id)
            .map(|a| a.name.clone())
            .unwrap_or_default();
        let to_quota = store
            .accounts
            .get(&target_id)
            .and_then(|a| a.cached_quota.as_ref())
            .map(|q| q.five_hour_left);
        state.switch_logger.log_switch(
            from_name,
            to_name,
            switch_log::SwitchReason::Manual,
            None,
            to_quota,
        );
    }

    // Disconnect all proxy WebSocket connections, force Codex App to reconnect with new token
    state.ws_disconnect.notify_waiters();
    println!("[Switch] Notified proxy to disconnect WebSocket connections");

    // Refresh tray menu
    crate::tray::update_tray_menu(&app);
    Ok(())
}

/// Predict next account to switch (cache-based only, no network requests)
/// Shared scoring algorithm: based on CachedQuota reset_at timestamp and remaining quota
/// Returns (account_id, account_name, score) sorted by score descending
/// Start scheduled quota refresh scheduler
pub fn start_quota_refresh(
    store: std::sync::Arc<std::sync::Mutex<AccountStore>>,
    app_handle: tauri::AppHandle,
) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        println!("[QuotaRefresh] Scheduled quota refresh started");

        loop {
            let (enabled, interval_minutes, batch_size) = {
                let s = store.lock().unwrap();
                (
                    s.settings.quota_refresh_enabled,
                    s.settings.quota_refresh_interval.max(1),
                    s.settings.quota_refresh_batch.max(1),
                )
            };

            if !enabled {
                tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
                continue;
            }

            // Sort by cached_quota.updated_at, oldest first
            let targets: Vec<(String, String)> = {
                let s = store.lock().unwrap();
                let mut accounts: Vec<_> = s
                    .accounts
                    .values()
                    .filter(|a| !a.is_banned && !a.is_token_invalid && !a.is_logged_out)
                    .map(|a| {
                        let updated = a
                            .cached_quota
                            .as_ref()
                            .map(|q| q.updated_at)
                            .unwrap_or(chrono::DateTime::<chrono::Utc>::MIN_UTC);
                        (a.id.clone(), a.name.clone(), updated)
                    })
                    .collect();
                // Oldest first
                accounts.sort_by_key(|(_, _, t)| *t);
                accounts
                    .into_iter()
                    .take(batch_size as usize)
                    .map(|(id, name, _)| (id, name))
                    .collect()
            };

            for (id, name) in &targets {
                println!("[QuotaRefresh] Refreshing {} ...", name);

                let (at, aid, rt) = {
                    let s = store.lock().unwrap();
                    let acc = match s.accounts.get(id) {
                        Some(a) => a,
                        None => continue,
                    };
                    (
                        AccountStore::extract_access_token(&acc.auth_json),
                        AccountStore::extract_account_id(&acc.auth_json),
                        acc.refresh_token.clone(),
                    )
                };

                // No access_token, use refresh_token to exchange
                let access_token = match at {
                    Some(t) => t,
                    None => {
                        if let Some(ref rt_val) = rt {
                            match crate::oauth::refresh_access_token(rt_val).await {
                                Ok(res) => {
                                    if let Ok(mut s) = store.lock() {
                                        if let Some(acc) = s.accounts.get_mut(id) {
                                            AccountStore::apply_refreshed_tokens(
                                                acc,
                                                res.access_token.clone(),
                                                res.refresh_token.clone(),
                                                res.id_token,
                                                res.expires_in,
                                            );
                                            let _ = s.save();
                                        }
                                    }
                                    res.access_token
                                }
                                Err(e) => {
                                    println!("[QuotaRefresh] {} token refresh failed: {}", name, e);
                                    continue;
                                }
                            }
                        } else {
                            continue;
                        }
                    }
                };

                match usage::UsageFetcher::fetch_usage_direct(access_token, aid, rt, false).await {
                    Ok((usage, _)) => {
                        if let Ok(mut s) = store.lock() {
                            if let Some(acc) = s.accounts.get_mut(id) {
                                acc.cached_quota = Some(account::CachedQuota {
                                    five_hour_left: usage.five_hour_left as f64,
                                    five_hour_reset: usage.five_hour_reset.clone(),
                                    five_hour_reset_at: usage.five_hour_reset_at,
                                    five_hour_label: usage.five_hour_label.clone(),
                                    weekly_left: usage.weekly_left as f64,
                                    weekly_reset: usage.weekly_reset.clone(),
                                    weekly_reset_at: usage.weekly_reset_at,
                                    weekly_label: usage.weekly_label.clone(),
                                    plan_type: usage.plan_type.clone(),
                                    is_valid_for_cli: usage.is_valid_for_cli,
                                    updated_at: chrono::Utc::now(),
                                });
                                let _ = s.save();
                            }
                        }
                        println!(
                            "[QuotaRefresh] {} → 5h:{}% weekly:{}%",
                            name, usage.five_hour_left, usage.weekly_left
                        );

                        // Log auto-refresh quota
                        use tauri::Manager;
                        if let Some(logger) = app_handle
                            .try_state::<std::sync::Arc<crate::switch_log::SwitchLogger>>()
                        {
                            logger.inner().log_switch(
                                None,
                                name.clone(),
                                crate::switch_log::SwitchReason::AutoQuotaRefresh,
                                None,
                                Some(usage.five_hour_left as f64),
                            );
                        }

                        let _ = app_handle.emit("accounts-updated", ());
                    }
                    Err(e) => {
                        println!("[QuotaRefresh] {} quota query failed: {}", name, e);
                        // Ban/invalid mark
                        if e.contains("ACCOUNT_BANNED") {
                            if let Ok(mut s) = store.lock() {
                                if let Some(acc) = s.accounts.get_mut(id) {
                                    acc.is_banned = true;
                                    let _ = s.save();
                                }
                            }
                        } else if e.contains("TOKEN_INVALID") {
                            if let Ok(mut s) = store.lock() {
                                if let Some(acc) = s.accounts.get_mut(id) {
                                    acc.is_token_invalid = true;
                                    acc.is_logged_out = false;
                                    let _ = s.save();
                                }
                            }
                        }
                    }
                }

                // Interval of interval_minutes minutes between each account
                tokio::time::sleep(tokio::time::Duration::from_secs(
                    u64::from(interval_minutes) * 60,
                ))
                .await;
            }

            // If no targets, wait a round
            if targets.is_empty() {
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
            }
        }
    })
}

pub fn score_candidate_accounts(store: &AccountStore) -> Vec<(String, String, f64)> {
    let current_id = store.current.as_deref().unwrap_or("");
    let allow_free = store.settings.allow_auto_switch_to_free;
    let now = chrono::Utc::now().timestamp();

    let mut scored: Vec<(String, String, f64)> = Vec::new();

    for account in store.accounts.values() {
        if account.id == current_id
            || account.is_banned
            || account.is_token_invalid
            || account.is_logged_out
        {
            continue;
        }

        let score = match &account.cached_quota {
            None => 50.0,
            Some(q) => {
                let plan = q.plan_type.to_lowercase();
                let is_free = plan == "free" || plan == "unknown";

                if is_free && !allow_free {
                    continue;
                }

                // Plan priority bonus: pro > plus/team > free
                let plan_bonus = match plan.as_str() {
                    "pro" => 30.0,
                    "plus" | "team" | "enterprise" => 20.0,
                    "edu" | "business" => 15.0,
                    "free" | "unknown" => 0.0,
                    _ => 10.0,
                };

                // 5h availability
                let five_h = if q.five_hour_left <= 0.0 {
                    match q.five_hour_reset_at {
                        Some(reset_at) if now >= reset_at => 50.0,
                        _ => 0.0,
                    }
                } else {
                    q.five_hour_left
                };

                // Weekly availability
                let weekly = if q.weekly_left <= 0.0 {
                    match q.weekly_reset_at {
                        Some(reset_at) if now >= reset_at => 50.0,
                        _ => 0.0,
                    }
                } else {
                    q.weekly_left
                };

                let effective = if is_free { five_h } else { five_h.min(weekly) };
                if effective <= 0.0 {
                    continue;
                }
                // Final score = quota score + Plan bonus
                effective + plan_bonus
            }
        };

        scored.push((account.id.clone(), account.name.clone(), score));
    }

    // Sort by score descending
    scored.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    scored
}

/// Predict next optimal account (tray menu preview)
pub fn predict_next_account_internal(state: tauri::State<'_, AppState>) -> Option<(String, i32)> {
    let store = state.store.lock().ok()?;
    let candidates = score_candidate_accounts(&store);
    candidates
        .first()
        .map(|(_, name, score)| (name.clone(), *score as i32))
}

/// Smart switch: select optimal account and switch
pub async fn switch_to_next_account_internal(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    // 1. Use scoring algorithm to select optimal candidate
    let candidates = {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        score_candidate_accounts(&store)
    };

    if candidates.is_empty() {
        return Err("No available accounts".to_string());
    }

    // 2. Try from highest to lowest score, check API quota before switching
    for (target_id, target_name, score) in &candidates {
        println!("[SmartSwitch] Candidate: {} (score {:.0})", target_name, score);

        // Check API for latest quota
        let quota = match get_quota_internal(&state, target_id.clone()).await {
            Ok(u) => u,
            Err(e) => {
                // Ban/invalid/logout detection
                if e.contains("ACCOUNT_BANNED")
                    || e.contains("TOKEN_INVALID")
                    || e.contains("ACCOUNT_LOGGED_OUT")
                {
                    println!("[SmartSwitch] Account {} banned/invalid/logged out, skipping", target_name);
                    continue;
                }
                println!(
                    "[SmartSwitch] Account {} quota query failed: {}, skipping",
                    target_name, e
                );
                continue;
            }
        };

        let plan = quota.plan_type.to_lowercase();
        let is_free = plan == "free" || plan == "unknown";

        let has_quota = if is_free {
            quota.five_hour_left > 0
        } else {
            quota.five_hour_left > 0 && quota.weekly_left > 0
        };

        if has_quota {
            println!(
                "[SmartSwitch] Selected optimal account: {} ({}, 5h={}%, weekly={}%)",
                target_name, quota.plan_type, quota.five_hour_left, quota.weekly_left
            );
            return switch_account(state, app.clone(), target_id.clone()).await;
        } else {
            println!("[SmartSwitch] Account {} quota exhausted, continue searching", target_name);
        }
    }

    Err("Checked all accounts, no account with available quota found".to_string())
}

/// Internal helper: get quota data
async fn get_quota_internal(state: &AppState, id: String) -> Result<UsageDisplay, String> {
    let (access_token, account_id, refresh_token) = {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        let account = store.accounts.get(&id).ok_or("Account does not exist")?;
        let at = AccountStore::extract_access_token(&account.auth_json);
        let aid = AccountStore::extract_account_id(&account.auth_json);
        let rt = account.refresh_token.clone();
        (at, aid, rt)
    };

    // If no access_token, use refresh_token to exchange for one
    let access_token = if let Some(at) = access_token {
        at
    } else if let Some(ref rt) = refresh_token {
        match crate::oauth::refresh_access_token(rt).await {
            Ok(token_res) => {
                // Save new token
                let mut store = state.store.lock().map_err(|e| e.to_string())?;
                if let Some(account) = store.accounts.get_mut(&id) {
                    AccountStore::apply_refreshed_tokens(
                        account,
                        token_res.access_token.clone(),
                        token_res.refresh_token.clone(),
                        token_res.id_token,
                        token_res.expires_in,
                    );
                    if let Err(e) = store.save() {
                        eprintln!("[Store] Save failed: {}", e);
                    }
                }
                token_res.access_token
            }
            Err(e) => return Err(format!("TOKEN_INVALID:Token refresh failed: {}", e)),
        }
    } else {
        return Err("TOKEN_INVALID:No access_token and no refresh_token".to_string());
    };

    let result =
        UsageFetcher::fetch_usage_direct(access_token, account_id, refresh_token, true).await;

    // Detect ban/invalid: mark separately
    if let Err(ref e) = result {
        if e.contains("ACCOUNT_BANNED") {
            let mut store = state.store.lock().map_err(|e| e.to_string())?;
            if let Some(account) = store.accounts.get_mut(&id) {
                account.is_banned = true;
                account.is_token_invalid = false;
                account.is_logged_out = false;
                if let Err(e) = store.save() {
                    eprintln!("[Store] Save failed: {}", e);
                }
            }
            return Err(e.clone());
        }
        if e.contains("TOKEN_INVALID") {
            let mut store = state.store.lock().map_err(|e| e.to_string())?;
            if let Some(account) = store.accounts.get_mut(&id) {
                account.is_token_invalid = true;
                account.is_banned = false;
                account.is_logged_out = false;
                if let Err(e) = store.save() {
                    eprintln!("[Store] Save failed: {}", e);
                }
            }
            return Err(e.clone());
        }
        if e.contains("ACCOUNT_LOGGED_OUT") {
            let mut store = state.store.lock().map_err(|e| e.to_string())?;
            if let Some(account) = store.accounts.get_mut(&id) {
                account.is_logged_out = true;
                account.is_banned = false;
                account.is_token_invalid = false;
                if let Err(e) = store.save() {
                    eprintln!("[Store] Save failed: {}", e);
                }
            }
            return Err(e.clone());
        }
    }

    let (display, new_tokens) = result?;

    // If new Token generated, save
    if let Some(res) = new_tokens {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        if let Some(account) = store.accounts.get_mut(&id) {
            AccountStore::apply_refreshed_tokens(
                account,
                res.access_token,
                res.refresh_token,
                res.id_token,
                res.expires_in,
            );
            if let Err(e) = store.save() {
                eprintln!("[Store] Save failed: {}", e);
            }
        }
    }

    // Update cache
    {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        if let Some(account) = store.accounts.get_mut(&id) {
            account.cached_quota = Some(usage_to_cached(&display));
            if let Err(e) = store.save() {
                eprintln!("[Store] Save failed: {}", e);
            }
        }
    }

    Ok(display)
}

fn usage_to_cached(u: &UsageDisplay) -> crate::account::CachedQuota {
    crate::account::CachedQuota {
        five_hour_left: u.five_hour_left as f64,
        five_hour_reset: u.five_hour_reset.clone(),
        five_hour_reset_at: u.five_hour_reset_at,
        five_hour_label: u.five_hour_label.clone(),
        weekly_left: u.weekly_left as f64,
        weekly_reset: u.weekly_reset.clone(),
        weekly_reset_at: u.weekly_reset_at,
        weekly_label: u.weekly_label.clone(),
        plan_type: u.plan_type.clone(),
        is_valid_for_cli: u.is_valid_for_cli,
        updated_at: Utc::now(),
    }
}

/// Force sync current Codex auth.json to specified account
#[tauri::command]
fn sync_current_auth_to_account(state: State<AppState>, id: String) -> Result<(), String> {
    let auth_json = AccountStore::read_codex_auth()?;
    let mut store = state.store.lock().map_err(|e| e.to_string())?;
    if store.sync_account_from_auth_json(&id, auth_json) {
        store.save()?;
        return Ok(());
    }
    Err("Sync failed: Account not found or User ID mismatch".to_string())
}

/// Check if Codex is signed in
#[tauri::command]
fn check_codex_login() -> Result<bool, String> {
    Ok(AccountStore::codex_auth_path().exists())
}

/// Get specified account's usage info (without switching)
#[tauri::command]
async fn get_quota_by_id(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
    id: String,
) -> Result<UsageDisplay, String> {
    // Current active account: verify identity via ~/.codex/auth.json first, then query quota via API
    let is_current = {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        store.current.as_deref() == Some(id.as_str())
    };

    if is_current {
        let official_auth = AccountStore::read_codex_auth()?;
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        let local_auth = store
            .accounts
            .get(&id)
            .ok_or_else(|| format!("Account {} not found", id))?
            .auth_json
            .clone();

        if !AccountStore::auth_identity_matches(&local_auth, &official_auth) {
            return Err(
                "Current active account identity mismatch with ~/.codex/auth.json, overwrite rejected. Please switch to same account in Codex first".to_string(),
            );
        }

        if local_auth != official_auth {
            println!(
                "[Quota] Current active account {}: Detected official auth.json change, syncing from authoritative source.",
                id
            );
            if store.sync_account_from_auth_json(&id, official_auth) {
                store.save()?;
            }
        } else {
            println!("[Quota] Current active account {}: In sync with official auth.json.", id);
        }
    }

    // 1. Get account Token from Store
    let (access_token_opt, account_id, refresh_token) = {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        let account = store
            .accounts
            .get(&id)
            .ok_or_else(|| format!("Account {} not found", id))?;

        let at = AccountStore::extract_access_token(&account.auth_json);
        let aid = AccountStore::extract_account_id(&account.auth_json);
        let rt = account
            .refresh_token
            .clone()
            .or_else(|| AccountStore::extract_refresh_token(&account.auth_json));

        (at, aid, rt)
    };

    // If no access_token, use refresh_token to exchange for one
    let access_token = if let Some(at) = access_token_opt {
        at
    } else if let Some(ref rt) = refresh_token {
        match crate::oauth::refresh_access_token(rt).await {
            Ok(token_res) => {
                let mut store = state.store.lock().map_err(|e| e.to_string())?;
                if let Some(account) = store.accounts.get_mut(&id) {
                    AccountStore::apply_refreshed_tokens(
                        account,
                        token_res.access_token.clone(),
                        token_res.refresh_token.clone(),
                        token_res.id_token,
                        token_res.expires_in,
                    );
                    if let Err(e) = store.save() {
                        eprintln!("[Store] Save failed: {}", e);
                    }
                }
                token_res.access_token
            }
            Err(e) => return Err(format!("TOKEN_INVALID:Token refresh failed: {}", e)),
        }
    } else {
        return Err("TOKEN_INVALID:No access_token and no refresh_token".to_string());
    };

    // 2. Use Token to fetch usage (allow auto-refresh)
    let result = UsageFetcher::fetch_usage_direct(
        access_token,
        account_id,
        refresh_token,
        true, // Allow refresh to solve token expiration
    )
    .await;

    // Detect ban/invalid: mark separately
    if let Err(ref e) = result {
        if e.contains("ACCOUNT_BANNED") {
            let mut store = state.store.lock().map_err(|e| e.to_string())?;
            if let Some(account) = store.accounts.get_mut(&id) {
                account.is_banned = true;
                account.is_token_invalid = false;
                account.is_logged_out = false;
                if let Err(e) = store.save() {
                    eprintln!("[Store] Save failed: {}", e);
                }
            }
            return Err(e.clone());
        }
        if e.contains("TOKEN_INVALID") {
            let mut store = state.store.lock().map_err(|e| e.to_string())?;
            if let Some(account) = store.accounts.get_mut(&id) {
                account.is_token_invalid = true;
                account.is_banned = false;
                account.is_logged_out = false;
                if let Err(e) = store.save() {
                    eprintln!("[Store] Save failed: {}", e);
                }
            }
            return Err(e.clone());
        }
        if e.contains("ACCOUNT_LOGGED_OUT") {
            let mut store = state.store.lock().map_err(|e| e.to_string())?;
            if let Some(account) = store.accounts.get_mut(&id) {
                account.is_logged_out = true;
                account.is_banned = false;
                account.is_token_invalid = false;
                if let Err(e) = store.save() {
                    eprintln!("[Store] Save failed: {}", e);
                }
            }
            return Err(e.clone());
        }
    }

    let (usage, new_tokens) = result?;

    // 3. If new Token, update account data
    if let Some(tokens) = new_tokens {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        if let Some(account) = store.accounts.get_mut(&id) {
            // Update Token info in auth_json
            if let Some(obj) = account.auth_json.as_object_mut() {
                if let Some(tokens_obj) = obj.get_mut("tokens").and_then(|v| v.as_object_mut()) {
                    tokens_obj.insert(
                        "access_token".to_string(),
                        serde_json::json!(tokens.access_token),
                    );

                    if let Some(rt) = &tokens.refresh_token {
                        tokens_obj.insert("refresh_token".to_string(), serde_json::json!(rt));
                    } else if let Some(rt) = account.refresh_token.as_deref() {
                        if tokens_obj.get("refresh_token").is_none() {
                            tokens_obj.insert("refresh_token".to_string(), serde_json::json!(rt));
                        }
                    }

                    if let Some(it) = &tokens.id_token {
                        tokens_obj.insert("id_token".to_string(), serde_json::json!(it));
                    }

                    if let Some(expires_in) = tokens.expires_in {
                        let expires_at = (chrono::Utc::now()
                            + chrono::Duration::seconds(expires_in as i64))
                        .to_rfc3339();
                        tokens_obj.insert("expires_at".to_string(), serde_json::json!(expires_at));
                    }
                }
            }

            // Update refresh_token field
            if let Some(rt) = tokens.refresh_token {
                account.refresh_token = Some(rt);
            }
            if let Some(obj) = account.auth_json.as_object_mut() {
                obj.insert(
                    "last_refresh".to_string(),
                    serde_json::json!(Utc::now().to_rfc3339()),
                );
            }

            // Update quota cache
            account.cached_quota = Some(account::CachedQuota {
                five_hour_left: usage.five_hour_left as f64,
                five_hour_reset: usage.five_hour_reset.clone(),
                five_hour_reset_at: usage.five_hour_reset_at,
                five_hour_label: usage.five_hour_label.clone(),
                weekly_left: usage.weekly_left as f64,
                weekly_reset: usage.weekly_reset.clone(),
                weekly_reset_at: usage.weekly_reset_at,
                weekly_label: usage.weekly_label.clone(),
                plan_type: usage.plan_type.clone(),
                is_valid_for_cli: usage.is_valid_for_cli,
                updated_at: Utc::now(),
            });
        }
        store.save()?;
    } else {
        // Even without new Token, update quota cache
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        if let Some(account) = store.accounts.get_mut(&id) {
            account.cached_quota = Some(account::CachedQuota {
                five_hour_left: usage.five_hour_left as f64,
                five_hour_reset: usage.five_hour_reset.clone(),
                five_hour_reset_at: usage.five_hour_reset_at,
                five_hour_label: usage.five_hour_label.clone(),
                weekly_left: usage.weekly_left as f64,
                weekly_reset: usage.weekly_reset.clone(),
                weekly_reset_at: usage.weekly_reset_at,
                weekly_label: usage.weekly_label.clone(),
                plan_type: usage.plan_type.clone(),
                is_valid_for_cli: usage.is_valid_for_cli,
                updated_at: Utc::now(),
            });
        }
        store.save()?;
    }

    Ok(usage)
}

/// Fix Codex App quarantine attribute (requires sudo)
#[tauri::command]
fn request_quarantine_fix_ticket(state: State<AppState>) -> Result<String, String> {
    state.issue_quarantine_fix_ticket()
}

/// Fix Codex App quarantine attribute (requires sudo)
#[tauri::command]
async fn fix_codex_quarantine(
    state: tauri::State<'_, AppState>,
    ticket: String,
) -> Result<(), String> {
    state.consume_quarantine_fix_ticket(&ticket)?;
    ide_control::remove_quarantine()
}

/// Reload IDE windows
#[tauri::command]
async fn reload_ide_windows(use_window_reload: bool) -> Result<Vec<String>, String> {
    let ides = ide_control::detect_running_ides();
    let mut reloaded = Vec::new();

    for ide in &ides {
        if let Err(e) = ide_control::reload_ide(ide, use_window_reload) {
            println!("Failed to reload {}: {}", ide, e);
        } else {
            reloaded.push(ide.clone());
        }
    }

    Ok(reloaded)
}

/// Get Token usage stats
#[tauri::command]
fn get_token_stats(state: State<AppState>) -> Result<token_tracker::UsageStats, String> {
    Ok(state.token_tracker.get_stats())
}

/// Reset Token usage stats
#[tauri::command]
fn reset_token_stats(state: State<AppState>) -> Result<(), String> {
    state.token_tracker.reset();
    Ok(())
}

/// Get Token usage history (trend chart data)
#[tauri::command]
fn get_token_history(days: u32) -> Result<Vec<token_tracker::TokenHistoryEntry>, String> {
    Ok(token_tracker::TokenTracker::get_history(days))
}

// ── Skills management commands ──

#[tauri::command]
fn get_installed_skills() -> Result<Vec<skills::InstalledSkill>, String> {
    let data = skills::SkillStore::load();
    Ok(data.skills)
}

#[tauri::command]
fn get_skill_repos() -> Result<Vec<skills::SkillRepo>, String> {
    let data = skills::SkillStore::load();
    Ok(data.repos)
}

#[tauri::command]
fn add_skill_repo(owner: String, name: String, branch: String) -> Result<(), String> {
    let mut data = skills::SkillStore::load();
    if data.repos.iter().any(|r| r.owner == owner && r.name == name) {
        return Err("Repo already exists".into());
    }
    data.repos.push(skills::SkillRepo { owner, name, branch, enabled: true });
    skills::SkillStore::save(&data)
}

#[tauri::command]
fn remove_skill_repo(owner: String, name: String) -> Result<(), String> {
    let mut data = skills::SkillStore::load();
    data.repos.retain(|r| !(r.owner == owner && r.name == name));
    skills::SkillStore::save(&data)
}

#[tauri::command]
async fn discover_skills() -> Result<Vec<skills::DiscoverableSkill>, String> {
    let data = skills::SkillStore::load();
    let mut discovered = skills::SkillStore::discover_skills(&data.repos).await;
    // Mark as installed
    let installed_dirs: std::collections::HashSet<String> = data.skills.iter().map(|s| s.directory.clone()).collect();
    for s in &mut discovered {
        s.installed = installed_dirs.contains(&s.directory);
    }
    Ok(discovered)
}

#[tauri::command]
async fn install_skill(skill_json: String) -> Result<(), String> {
    let skill: skills::DiscoverableSkill = serde_json::from_str(&skill_json).map_err(|e| e.to_string())?;
    let mut data = skills::SkillStore::load();
    skills::SkillStore::install_skill(&mut data, &skill).await?;
    skills::SkillStore::save(&data)
}

#[tauri::command]
fn uninstall_skill(skill_id: String) -> Result<(), String> {
    let mut data = skills::SkillStore::load();
    skills::SkillStore::uninstall_skill(&mut data, &skill_id)?;
    skills::SkillStore::save(&data)
}

#[tauri::command]
fn toggle_skill_app_link(app: String, enabled: bool) -> Result<(), String> {
    skills::SkillStore::toggle_app_link(&app, enabled)
}

#[tauri::command]
fn get_skill_app_status() -> Result<std::collections::HashMap<String, bool>, String> {
    Ok(skills::SkillStore::get_app_link_status())
}

#[tauri::command]
fn get_skill_content(directory: String) -> Result<String, String> {
    let ssot = dirs::home_dir().unwrap().join(".codex-switcher").join("skills").join(&directory);
    let md_path = ssot.join("SKILL.md");
    std::fs::read_to_string(&md_path).map_err(|e| format!("Read failed: {}", e))
}

#[tauri::command]
fn scan_and_import_skills() -> Result<usize, String> {
    let mut data = skills::SkillStore::load();
    let count = skills::SkillStore::scan_existing(&mut data);
    if count > 0 {
        skills::SkillStore::save(&data)?;
    }
    Ok(count)
}

#[tauri::command]
fn sync_all_skills() -> Result<(), String> {
    skills::SkillStore::sync_all();
    Ok(())
}

#[tauri::command]
fn get_switch_history(
    state: State<AppState>,
    days: u32,
) -> Result<Vec<switch_log::SwitchEvent>, String> {
    Ok(state.switch_logger.get_history(days))
}

/// Get switch stats
#[tauri::command]
fn get_switch_stats(state: State<AppState>) -> Result<switch_log::SwitchStats, String> {
    Ok(state.switch_logger.get_stats())
}

/// Show main window (called by tray popup)
#[tauri::command]
fn show_main_window_cmd(app: tauri::AppHandle) {
    crate::tray::show_main_window_from_cmd(&app);
}

/// Kill all codex processes (excluding Codex Switcher itself)
#[tauri::command]
fn kill_codex_processes() -> Result<String, String> {
    let script = r#"
        killed=0
        for pid in $(pgrep -f codex 2>/dev/null); do
            cmd=$(ps -p "$pid" -o command= 2>/dev/null || true)
            case "$cmd" in
                *codex-switcher*|*Codex\ Switcher*|*codex_switcher*) continue ;;
            esac
            kill -9 "$pid" 2>/dev/null && killed=$((killed+1))
        done
        echo "$killed"
    "#;

    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(script)
        .output()
        .map_err(|e| format!("Execution failed: {}", e))?;

    let count = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let n: i32 = count.parse().unwrap_or(0);

    if n > 0 {
        Ok(format!("Terminated {} codex processes", n))
    } else {
        Ok("No running codex processes found".to_string())
    }
}

/// Set OPENAI_BASE_URL env var (terminal + GUI apps)
#[tauri::command]
fn set_proxy_env(port: u16, enable: bool) -> Result<String, String> {
    let home = dirs::home_dir().ok_or("Unable to resolve user directory")?;
    let env_value = format!("http://localhost:{}/v1", port);
    let env_line = format!("export OPENAI_BASE_URL={}", env_value);
    let marker = "# codex-switcher-proxy";
    let mut results = Vec::new();

    // ── 1. Terminal: write to .zshrc / .bashrc ──
    for rc_name in &[".zshrc", ".bashrc"] {
        let rc_path = home.join(rc_name);
        if !rc_path.exists() {
            continue;
        }
        let content = std::fs::read_to_string(&rc_path)
            .map_err(|e| format!("Failed to read {}: {}", rc_name, e))?;

        let cleaned: Vec<&str> = content
            .lines()
            .filter(|line| !line.contains(marker))
            .collect();
        let mut new_content = cleaned.join("\n");

        if enable {
            if !new_content.ends_with('\n') {
                new_content.push('\n');
            }
            new_content.push_str(&format!("{} {}\n", env_line, marker));
        }

        std::fs::write(&rc_path, &new_content)
            .map_err(|e| format!("Failed to write {}: {}", rc_name, e))?;
        results.push(rc_name.to_string());
    }

    // ── 2. GUI apps: launchctl setenv (effective after Codex App restart) ──
    #[cfg(target_os = "macos")]
    {
        if enable {
            let _ = std::process::Command::new("launchctl")
                .args(["setenv", "OPENAI_BASE_URL", &env_value])
                .output();
            results.push("launchctl".to_string());
        } else {
            let _ = std::process::Command::new("launchctl")
                .args(["unsetenv", "OPENAI_BASE_URL"])
                .output();
            results.push("launchctl".to_string());
        }
    }

    // ── 3. Codex App config.toml: write openai_base_url ──
    match set_codex_config_base_url(if enable { Some(&env_value) } else { None }) {
        Ok(_) => results.push("config.toml".to_string()),
        Err(e) => results.push(format!("config.toml(failed: {})", e)),
    }

    let status = if enable { "Set" } else { "Removed" };
    Ok(format!(
        "{} OPENAI_BASE_URL ({}).\nTerminal: effective in new window\nCodex App: effective after restart",
        status,
        results.join(", ")
    ))
}

/// Read/write openai_base_url field in ~/.codex/config.toml
fn set_codex_config_base_url(url: Option<&str>) -> Result<(), String> {
    let config_path = dirs::home_dir()
        .ok_or("Failed to get user directory")?
        .join(".codex")
        .join("config.toml");

    if !config_path.exists() {
        if url.is_some() {
            // File doesn't exist, create and write
            let content = format!("openai_base_url = \"{}\"\n", url.unwrap());
            std::fs::write(&config_path, content)
                .map_err(|e| format!("Failed to create config.toml: {}", e))?;
        }
        return Ok(());
    }

    let content = std::fs::read_to_string(&config_path)
        .map_err(|e| format!("Failed to read config.toml: {}", e))?;

    let mut new_lines: Vec<String> = Vec::new();
    let mut found = false;
    let mut in_section = false;

    for line in content.lines() {
        let trimmed = line.trim();

        // Detect [section] header to determine if at top level
        if trimmed.starts_with('[') {
            in_section = true;
        }

        // Match top-level openai_base_url = "xxx"
        if !in_section && trimmed.starts_with("openai_base_url") && trimmed.contains('=') {
            found = true;
            if let Some(u) = url {
                new_lines.push(format!("openai_base_url = \"{}\"", u));
            }
            // Skip this line when url is None (remove)
            continue;
        }
        new_lines.push(line.to_string());
    }

    // If setting but no existing line found, insert before first [section]
    if url.is_some() && !found {
        let u = url.unwrap();
        let insert_line = format!("openai_base_url = \"{}\"", u);
        // Find position of first [section]
        let pos = new_lines.iter().position(|l| l.trim().starts_with('['));
        match pos {
            Some(idx) => new_lines.insert(idx, insert_line),
            None => new_lines.push(insert_line),
        }
    }

    std::fs::write(&config_path, new_lines.join("\n") + "\n")
        .map_err(|e| format!("Failed to write config.toml: {}", e))?;

    Ok(())
}

/// Toggle Codex fast mode (modify profile field in config.toml)
#[tauri::command]
fn set_codex_fast_mode(enable: bool) -> Result<String, String> {
    let config_path = dirs::home_dir()
        .ok_or("Failed to get user directory")?
        .join(".codex")
        .join("config.toml");

    if !config_path.exists() {
        return Err("~/.codex/config.toml does not exist".to_string());
    }

    let content = std::fs::read_to_string(&config_path)
        .map_err(|e| format!("Failed to read config.toml: {}", e))?;

    let mut new_lines: Vec<String> = Vec::new();
    let mut found_profile = false;

    for line in content.lines() {
        let trimmed = line.trim();
        // Match profile = "xxx" line (top-level, not indented under [section])
        if trimmed.starts_with("profile") && trimmed.contains('=') && !trimmed.starts_with('[') {
            found_profile = true;
            if enable {
                new_lines.push("profile = \"fast\"".to_string());
            }
            // Skip this line when not enable (remove profile)
            continue;
        }
        new_lines.push(line.to_string());
    }

    // If enable but no profile line found, insert at file beginning
    if enable && !found_profile {
        new_lines.insert(0, "profile = \"fast\"".to_string());
    }

    std::fs::write(&config_path, new_lines.join("\n") + "\n")
        .map_err(|e| format!("Failed to write config.toml: {}", e))?;

    if enable {
        Ok("Fast mode enabled (2x quota consumption, faster inference). Restart Codex to apply.".to_string())
    } else {
        Ok("Fast mode disabled. Restart Codex to apply.".to_string())
    }
}

/// Get current fast mode status
#[tauri::command]
fn get_codex_fast_mode() -> Result<bool, String> {
    let config_path = dirs::home_dir()
        .ok_or("Failed to get user directory")?
        .join(".codex")
        .join("config.toml");

    if !config_path.exists() {
        return Ok(false);
    }

    let content = std::fs::read_to_string(&config_path).map_err(|e| format!("Read failed: {}", e))?;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("profile") && trimmed.contains('=') {
            return Ok(trimmed.contains("\"fast\""));
        }
    }

    Ok(false)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SyncStatus {
    pub is_synced: bool,
    pub disk_email: Option<String>,
    pub matching_id: Option<String>,
    pub current_id: Option<String>,
}

/// Check sync status between IDE disk state and memory state
#[tauri::command]
fn get_sync_status(state: State<AppState>) -> Result<SyncStatus, String> {
    let disk_auth = match AccountStore::read_codex_auth() {
        Ok(a) => a,
        Err(_) => {
            let store = state.store.lock().map_err(|e| e.to_string())?;
            return Ok(SyncStatus {
                is_synced: true,
                disk_email: None,
                matching_id: None,
                current_id: store.current.clone(),
            });
        }
    };

    let store = state.store.lock().map_err(|e| e.to_string())?;
    let disk_email = AccountStore::extract_email(&disk_auth);

    // Fast path: check if disk auth matches current active account identity
    // This resolves false positives from JWT expiration/corruption causing email extraction failure
    let current_matches_disk = store
        .current
        .as_ref()
        .and_then(|curr_id| {
            store.accounts.get(curr_id).map(|a| {
                AccountStore::auth_identity_matches(&a.auth_json, &disk_auth)
                    || disk_email
                        .as_deref()
                        .map(|e| a.name.to_lowercase() == e.to_lowercase())
                        .unwrap_or(false)
            })
        })
        .unwrap_or(false);

    if current_matches_disk {
        return Ok(SyncStatus {
            is_synced: true,
            disk_email,
            matching_id: store.current.clone(),
            current_id: store.current.clone(),
        });
    }

    // Slow path: iterate all accounts to match
    let matching_id = disk_email
        .as_deref()
        .and_then(|email| {
            let email_lower = email.to_lowercase();
            store
                .accounts
                .values()
                .find(|a| {
                    AccountStore::extract_email(&a.auth_json)
                        .map(|e| e.to_lowercase() == email_lower)
                        .unwrap_or(false)
                        || a.name.to_lowercase() == email_lower
                })
                .map(|a| a.id.clone())
        })
        .or_else(|| {
            store
                .accounts
                .values()
                .find(|a| AccountStore::auth_identity_matches(&a.auth_json, &disk_auth))
                .map(|a| a.id.clone())
        });

    let is_synced = match (&store.current, &matching_id) {
        (Some(curr), Some(match_id)) => curr == match_id,
        (None, None) => true,
        _ => false,
    };

    Ok(SyncStatus {
        is_synced,
        disk_email,
        matching_id,
        current_id: store.current.clone(),
    })
}

/// Force align Switcher's active pointer to disk account
/// Safety policy: only modify active pointer, never overwrite existing account Token data
#[tauri::command]
fn sync_active_with_disk(state: State<AppState>, app: tauri::AppHandle) -> Result<(), String> {
    let disk_auth = AccountStore::read_codex_auth()?;
    let disk_email = AccountStore::extract_email(&disk_auth);
    let mut store = state.store.lock().map_err(|e| e.to_string())?;

    // Prefer JWT Email matching (most reliable), fallback to account_id
    let matching_id = disk_email
        .as_deref()
        .and_then(|email| {
            let email_lower = email.to_lowercase();
            store
                .accounts
                .values()
                .find(|a| {
                    AccountStore::extract_email(&a.auth_json)
                        .map(|e| e.to_lowercase() == email_lower)
                        .unwrap_or(false)
                        || a.name.to_lowercase() == email_lower
                })
                .map(|a| a.id.clone())
        })
        .or_else(|| {
            // fallback: account_id match
            store
                .accounts
                .values()
                .find(|a| AccountStore::auth_identity_matches(&a.auth_json, &disk_auth))
                .map(|a| a.id.clone())
        })
        .ok_or_else(|| "Disk account not in management list, please import first".to_string())?;

    // Safety: only change pointer, don't overwrite Token. Avoid banned Token contaminating good accounts.
    store.current = Some(matching_id);
    store.save()?;

    crate::tray::update_tray_menu(&app);
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .manage(AppState::new())
        .setup(|app| {
            // Initialize system tray
            if let Err(e) = tray::init(app.handle()) {
                eprintln!("Tray initialization failed: {:?}", e);
            }

            // Start background scheduler (only when enabled in settings)
            let state = app.state::<AppState>();
            let should_start = state
                .store
                .lock()
                .map(|store| store.settings.background_refresh)
                .unwrap_or(false);
            if should_start {
                let handle = scheduler::start(state.store.clone(), app.handle().clone());
                let mut scheduler_handle = state.scheduler.lock().unwrap();
                *scheduler_handle = Some(handle);
            } else {
                println!("[Scheduler] Background refresh not enabled, skipping start");
            }

            // Start local proxy (only when enabled in settings)
            let (proxy_enabled, proxy_port, proxy_allow_lan) = state
                .store
                .lock()
                .map(|s| {
                    (
                        s.settings.proxy_enabled,
                        s.settings.proxy_port,
                        s.settings.proxy_allow_lan,
                    )
                })
                .unwrap_or((false, 18080, false));
            if proxy_enabled {
                let handle = proxy::start(
                    state.store.clone(),
                    proxy_port,
                    proxy_allow_lan,
                    app.handle().clone(),
                    state.proxy_stats.clone(),
                    state.token_tracker.clone(),
                    state.ws_disconnect.clone(),
                    state.switch_logger.clone(),
                );
                let mut proxy_handle = state.proxy_handle.lock().unwrap();
                *proxy_handle = Some(handle);
                println!("[Proxy] Proxy started with app (port {})", proxy_port);
            } else {
                println!("[Proxy] Local proxy not enabled, skipping start");
            }

            // Start scheduled quota refresh
            let quota_refresh_enabled = state
                .store
                .lock()
                .map(|s| s.settings.quota_refresh_enabled)
                .unwrap_or(false);
            if quota_refresh_enabled {
                let handle = start_quota_refresh(state.store.clone(), app.handle().clone());
                let mut qr = state.quota_refresh_handle.lock().unwrap();
                *qr = Some(handle);
            }

            // Initialize Skills SSOT + auto import
            if let Err(e) = skills::init_ssot() {
                eprintln!("[Skills] SSOT initialization failed: {}", e);
            }
            {
                let mut data = skills::SkillStore::load();
                let count = skills::SkillStore::scan_existing(&mut data);
                if count > 0 {
                    let _ = skills::SkillStore::save(&data);
                    println!("[Skills] Auto imported {} existing skills", count);
                }
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            // Intercept close event, hide window and remove from Dock instead
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                // macOS: hide Dock icon, become pure background tray app
                #[cfg(target_os = "macos")]
                {
                    let app = window.app_handle();
                    app.set_activation_policy(tauri::ActivationPolicy::Accessory)
                        .unwrap_or(());
                }
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_accounts,
            get_current_account_id,
            import_current_account,
            switch_account,
            sync_current_auth_to_account,
            delete_account,
            update_account,
            set_account_inactive_refresh_enabled,
            export_accounts,
            import_accounts,
            check_codex_login,
            get_quota_by_id,
            oauth_server::start_oauth_login,
            finalize_oauth_login,
            reload_ide_windows,
            get_settings,
            update_settings,
            get_proxy_status,
            kill_codex_processes,
            set_proxy_env,
            get_token_stats,
            reset_token_stats,
            show_main_window_cmd,
            set_codex_fast_mode,
            get_codex_fast_mode,
            get_token_history,
            get_switch_history,
            get_switch_stats,
            get_installed_skills,
            get_skill_repos,
            add_skill_repo,
            remove_skill_repo,
            discover_skills,
            install_skill,
            uninstall_skill,
            toggle_skill_app_link,
            get_skill_app_status,
            get_skill_content,
            scan_and_import_skills,
            sync_all_skills,
            check_sync_conflict,
            request_quarantine_fix_ticket,
            fix_codex_quarantine,
            get_sync_status,
            sync_active_with_disk,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn test_auth(account_id: &str, refresh_token: &str) -> serde_json::Value {
        serde_json::json!({
            "tokens": {
                "account_id": account_id,
                "refresh_token": refresh_token
            }
        })
    }

    fn test_account(name: &str, account_id: &str, refresh_token: &str) -> Account {
        let auth_json = test_auth(account_id, refresh_token);
        Account {
            id: "acc-1".to_string(),
            name: name.to_string(),
            auth_json: auth_json.clone(),
            refresh_token: AccountStore::extract_refresh_token(&auth_json),
            created_at: Utc::now(),
            last_used: None,
            notes: None,
            cached_quota: None,
            keepalive: account::KeepaliveState::default(),
            is_banned: false,
            is_token_invalid: false,
            is_logged_out: false,
        }
    }

    #[test]
    fn quota_refresh_never_allows_local_token_refresh() {
        assert!(!allow_local_refresh_for_quota(true));
        assert!(!allow_local_refresh_for_quota(false));
    }

    #[test]
    fn sync_conflict_is_ignored_when_identity_mismatch() {
        let current = test_account("current", "acct-local", "rt-local");
        let disk_auth = test_auth("acct-disk", "rt-new");

        assert_eq!(detect_sync_conflict_for_current(&current, &disk_auth), None);
    }

    #[test]
    fn sync_conflict_is_reported_when_identity_matches_and_refresh_token_changed() {
        let current = test_account("current", "acct-1", "rt-local");
        let disk_auth = test_auth("acct-1", "rt-new");

        assert_eq!(
            detect_sync_conflict_for_current(&current, &disk_auth),
            Some("current".to_string())
        );
    }

    #[test]
    fn quarantine_fix_ticket_can_only_be_used_once() {
        let state = AppState::new();
        let ticket = state.issue_quarantine_fix_ticket().unwrap();

        assert!(state.consume_quarantine_fix_ticket(&ticket).is_ok());
        assert!(state.consume_quarantine_fix_ticket(&ticket).is_err());
    }

    #[test]
    fn quarantine_fix_ticket_rejects_mismatch() {
        let state = AppState::new();
        let _ticket = state.issue_quarantine_fix_ticket().unwrap();

        assert!(state.consume_quarantine_fix_ticket("wrong-ticket").is_err());
    }

    #[test]
    fn quarantine_fix_ticket_rejects_expired_ticket() {
        let state = AppState::new();
        {
            let mut slot = state.quarantine_fix_ticket.lock().unwrap();
            *slot = Some(QuarantineFixTicket {
                value: "expired".to_string(),
                expires_at: Utc::now() - chrono::Duration::seconds(1),
            });
        }

        let err = state
            .consume_quarantine_fix_ticket("expired")
            .expect_err("expired ticket should be rejected");
        assert!(err.contains("expired"));
    }
}
