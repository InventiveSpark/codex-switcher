//! Background scheduler - Account state sync
//!
//! Strategy:
//! - Current account: only sync from official auth.json backflow, no proactive refresh
//! - Inactive accounts: Switcher exclusively handles keepalive refresh, atomically writing back to account store

use crate::account::AccountStore;
use crate::oauth;
use serde::Serialize;
use std::sync::{Arc, Mutex};
use tauri::Emitter;
use tokio::time::Duration;

#[derive(Debug, Clone)]
struct RefreshTarget {
    id: String,
    name: String,
    refresh_token: String,
}

#[derive(Serialize, Clone)]
struct RefreshFailedPayload {
    account_name: String,
    reason: String,
}

fn is_reused_or_revoked_error(reason: &str) -> bool {
    let lower = reason.to_lowercase();
    lower.contains("refresh_token_reused")
        || lower.contains("refresh_token_invalidated")
        || lower.contains("refresh_token_expired")
        || lower.contains("deactivated")
        || lower.contains("unauthorized")
        || lower.contains("invalid_grant")
}

fn is_logged_out_error(reason: &str) -> bool {
    let lower = reason.to_lowercase();
    lower.contains("logged out") || lower.contains("signed in to another account")
}

/// Start background state sync scheduler
pub fn start(
    store: Arc<Mutex<AccountStore>>,
    app_handle: tauri::AppHandle,
) -> tauri::async_runtime::JoinHandle<()> {
    // Use Tauri's async runtime instead of direct tokio::spawn
    // Because Tokio runtime may not be fully initialized when called from setup()
    tauri::async_runtime::spawn(async move {
        println!("✅ Background scheduler started");

        loop {
            let (enabled, interval_minutes, inactive_refresh_days) = {
                let store = store.lock().unwrap();
                (
                    store.settings.background_refresh,
                    store.settings.refresh_interval_minutes,
                    store.settings.inactive_refresh_days,
                )
            };

            if !enabled {
                tokio::time::sleep(Duration::from_secs(60)).await;
                continue;
            }

            println!("[Scheduler] Starting background sync check...");

            let interval_minutes = if interval_minutes == 0 {
                30
            } else {
                interval_minutes
            };

            let mut store_changed = false;
            let mut has_failure_event = false;

            // 1) Sync current account (authoritative source: ~/.codex/auth.json)
            if let Ok(official_auth) = AccountStore::read_codex_auth() {
                let mut store = store.lock().unwrap();
                if let Some(current_id) = store.current.clone() {
                    let local_auth = store.accounts.get(&current_id).map(|a| a.auth_json.clone());

                    if let Some(local_auth) = local_auth {
                        if AccountStore::auth_identity_matches(&local_auth, &official_auth) {
                            if local_auth != official_auth {
                                println!(
                                    "[Scheduler] Current account {} detected official auth.json change, syncing from authoritative source.",
                                    current_id
                                );
                                if store.sync_account_from_auth_json(&current_id, official_auth) {
                                    let _ = store.save();
                                    store_changed = true;
                                    println!("[Scheduler] ✅ Current account reverse sync successful");
                                }
                            } else {
                                println!(
                                    "[Scheduler] Current account {} matches official auth.json.",
                                    current_id
                                );
                            }
                        } else {
                            println!(
                                "[Scheduler] Current account {} identity mismatch with official auth.json, skipping sync.",
                                current_id
                            );
                        }
                    }
                }
            }

            // 2) Collect inactive accounts that should be exclusively kept alive by Switcher
            let targets: Vec<RefreshTarget> = {
                let store = store.lock().unwrap();
                let current = store.current.as_deref();
                store
                    .accounts
                    .values()
                    .filter(|account| current != Some(account.id.as_str()))
                    .filter(|account| {
                        AccountStore::should_refresh_inactive_account(
                            account,
                            inactive_refresh_days,
                        )
                    })
                    .filter_map(|account| {
                        let rt = account
                            .refresh_token
                            .clone()
                            .or_else(|| AccountStore::extract_refresh_token(&account.auth_json))?;
                        Some(RefreshTarget {
                            id: account.id.clone(),
                            name: account.name.clone(),
                            refresh_token: rt,
                        })
                    })
                    .collect()
            };

            // 3) Execute exclusive keepalive refresh for inactive accounts
            for target in targets {
                println!("[Scheduler] Inactive account {} attempting keepalive refresh", target.name);

                match oauth::refresh_access_token(&target.refresh_token).await {
                    Ok(tokens) => {
                        let mut store = store.lock().unwrap();
                        if store.current.as_deref() == Some(target.id.as_str()) {
                            // Account has become current, hand over to official path
                            continue;
                        }
                        if let Some(account) = store.accounts.get_mut(&target.id) {
                            if !account.keepalive.inactive_refresh_enabled {
                                continue;
                            }
                            AccountStore::apply_refreshed_tokens(
                                account,
                                tokens.access_token,
                                tokens.refresh_token,
                                tokens.id_token,
                                tokens.expires_in,
                            );
                        }
                        store.mark_keepalive_attempt_success(&target.id);
                        let _ = store.save();
                        store_changed = true;
                        println!("[Scheduler] ✅ Inactive account {} keepalive refresh successful", target.name);

                        // Record background keepalive system log
                        use tauri::Manager;
                        if let Some(logger) =
                            app_handle.try_state::<Arc<crate::switch_log::SwitchLogger>>()
                        {
                            logger.inner().log_switch(
                                None,
                                target.name.clone(),
                                crate::switch_log::SwitchReason::BackgroundKeepalive,
                                None,
                                None,
                            );
                        }
                    }
                    Err(err) => {
                        let reason = err;
                        let mut store = store.lock().unwrap();
                        store.mark_keepalive_attempt_failed(&target.id, reason.clone());
                        if is_reused_or_revoked_error(&reason) || is_logged_out_error(&reason) {
                            // Risk protection: after detecting reused/revoked, auto-disable inactive keepalive for this account to avoid repeated consumption.
                            let _ = store.set_inactive_refresh_enabled(&target.id, false);
                            if let Some(account) = store.accounts.get_mut(&target.id) {
                                if is_logged_out_error(&reason) {
                                    account.is_logged_out = true;
                                } else {
                                    account.is_token_invalid = true;
                                }
                            }
                        }
                        let _ = store.save();
                        has_failure_event = true;
                        println!(
                            "[Scheduler] ❌ Inactive account {} keepalive refresh failed: {}",
                            target.name, reason
                        );

                        let _ = app_handle.emit(
                            "token-refresh-failed",
                            RefreshFailedPayload {
                                account_name: target.name,
                                reason,
                            },
                        );
                    }
                }
            }

            if store_changed || has_failure_event {
                let _ = app_handle.emit("accounts-updated", ());
            }

            tokio::time::sleep(Duration::from_secs(u64::from(interval_minutes) * 60)).await;
        }
    })
}
