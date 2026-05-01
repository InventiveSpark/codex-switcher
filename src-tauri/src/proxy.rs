//! Codex Switcher - Local HTTP/WebSocket proxy server
//!
//! Transparent proxy: Intercepts Codex CLI/App requests, dynamically injects current account Token and forwards.
//! HTTP: Header forwarding logic consistent with official responses-api-proxy
//! WebSocket: Bidirectional bridge, supports Codex App WebSocket communication
//!
//! Features: SSE streaming | WebSocket passthrough | 429 auto-switch | Ban detection | Scoring selection

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::header::{HeaderName, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use reqwest::Client;
use tauri::Emitter;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite;
use tungstenite::client::IntoClientRequest;

use crate::account::AccountStore;
use crate::switch_log::{SwitchLogger, SwitchReason};
use crate::token_tracker::TokenTracker;

/// Pending switch notification message to inject
static PENDING_INJECT_MSG: std::sync::LazyLock<Mutex<Option<String>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));

/// Upstream for ChatGPT OAuth login (free/Plus/Team accounts)
const CHATGPT_HOST: &str = "chatgpt.com";
const CHATGPT_ORIGIN: &str = "https://chatgpt.com/backend-api/codex";

/// Upstream for API key
const API_HOST: &str = "api.openai.com";
const API_ORIGIN: &str = "https://api.openai.com";
const MAX_429_RETRIES: usize = 5;

/// Unified response Body type: supports Full (errors/small responses) and Stream (SSE streaming)
type ProxyBody = BoxBody<Bytes, String>;

/// Proxy runtime metrics (shared with AppState)
pub struct ProxyStats {
    pub total_requests: AtomicU64,
    pub auto_switches: AtomicU64,
}

impl Default for ProxyStats {
    fn default() -> Self {
        Self {
            total_requests: AtomicU64::new(0),
            auto_switches: AtomicU64::new(0),
        }
    }
}

/// Proxy runtime shared state
struct ProxyState {
    store: Arc<Mutex<AccountStore>>,
    client: Client,
    app_handle: tauri::AppHandle,
    switching: AtomicBool,
    stats: Arc<ProxyStats>,
    tracker: Arc<TokenTracker>,
    /// Notify WebSocket disconnect on switch
    ws_disconnect: Arc<tokio::sync::Notify>,
    switch_logger: Arc<SwitchLogger>,
}

/// Start proxy server
pub fn start(
    store: Arc<Mutex<AccountStore>>,
    port: u16,
    allow_lan: bool,
    app_handle: tauri::AppHandle,
    stats: Arc<ProxyStats>,
    tracker: Arc<TokenTracker>,
    ws_disconnect: Arc<tokio::sync::Notify>,
    switch_logger: Arc<SwitchLogger>,
) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        let addr = if allow_lan {
            SocketAddr::from(([0, 0, 0, 0], port))
        } else {
            SocketAddr::from(([127, 0, 0, 1], port))
        };
        let listener = match TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[Proxy] Failed to bind port {}: {}", port, e);
                return;
            }
        };

        println!("[Proxy] Proxy server started, listening on {}:{}", addr.ip(), port);

        let client = Client::builder()
            .build()
            .expect("[Proxy] Failed to build reqwest Client");

        let state = Arc::new(ProxyState {
            store,
            client,
            app_handle,
            switching: AtomicBool::new(false),
            stats,
            tracker,
            ws_disconnect,
            switch_logger,
        });

        loop {
            let (stream, peer_addr) = match listener.accept().await {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[Proxy] accept failed: {}", e);
                    continue;
                }
            };

            let state = state.clone();
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let service = service_fn(move |req| {
                    let state = state.clone();
                    handle_request(state, req)
                });

                if let Err(e) = http1::Builder::new()
                    .keep_alive(true)
                    .serve_connection(io, service)
                    .with_upgrades()
                    .await
                {
                    if !e.is_incomplete_message() {
                        eprintln!("[Proxy] Connection {} error: {}", peer_addr, e);
                    }
                }
            });
        }
    })
}

// ────────────────────────────────────────────────────────────────
// Token management
// ────────────────────────────────────────────────────────────────

/// Get current account's latest access_token + auth mode
///
/// Safety policy: Don't proactively refresh token (avoid conflict with Codex CLI),
/// but re-read from auth.json on each request to ensure using latest value refreshed by Codex CLI.
///
/// Returns (token, is_chatgpt_auth)
fn get_current_token(state: &ProxyState) -> Result<(String, bool), String> {
    let mut store = state.store.lock().map_err(|e| e.to_string())?;
    let current_id = store.current.as_ref().ok_or("No active account")?.clone();

    // Re-read latest token from auth.json (Codex CLI may have refreshed)
    if let Ok(disk_auth) = AccountStore::read_codex_auth() {
        if store.sync_account_from_auth_json(&current_id, disk_auth) {
            let _ = store.save();
        }
    }

    let account = store.accounts.get(&current_id).ok_or("Current account does not exist")?;

    let token = AccountStore::extract_access_token(&account.auth_json)
        .ok_or_else(|| "Current account missing access_token".to_string())?;

    // Determine auth mode: JWT (eyJ...) = ChatGPT OAuth, sk-... = API key
    let is_chatgpt = token.starts_with("eyJ");

    Ok((token, is_chatgpt))
}

/// Get upstream address based on auth mode
fn get_upstream(is_chatgpt: bool, path_and_query: &str) -> (String, &'static str) {
    if is_chatgpt {
        // Client path: /v1/responses (because OPENAI_BASE_URL includes /v1)
        // ChatGPT upstream: /backend-api/codex/responses (without /v1)
        // Need to strip /v1 prefix
        let path = path_and_query.strip_prefix("/v1").unwrap_or(path_and_query);
        let url = format!("{}{}", CHATGPT_ORIGIN, path);
        (url, CHATGPT_HOST)
    } else {
        // API key: forward to api.openai.com + original path (keep /v1)
        let url = format!("{}{}", API_ORIGIN, path_and_query);
        (url, API_HOST)
    }
}

// ────────────────────────────────────────────────────────────────
// Account selection algorithm (reuse lib.rs shared scoring)
// ────────────────────────────────────────────────────────────────

enum PickResult {
    Found { id: String, token: String },
    Exhausted { earliest_reset: Option<i64> },
}

fn pick_next_account(state: &ProxyState) -> PickResult {
    let store = match state.store.lock() {
        Ok(s) => s,
        Err(_) => {
            return PickResult::Exhausted {
                earliest_reset: None,
            }
        }
    };

    let candidates = crate::score_candidate_accounts(&store);

    if candidates.is_empty() {
        let now = Utc::now().timestamp();
        let mut earliest: Option<i64> = None;
        for account in store.accounts.values() {
            if let Some(q) = &account.cached_quota {
                for r in [q.five_hour_reset_at, q.weekly_reset_at]
                    .into_iter()
                    .flatten()
                {
                    if now < r {
                        earliest = Some(earliest.map_or(r, |e: i64| e.min(r)));
                    }
                }
            }
        }
        return PickResult::Exhausted {
            earliest_reset: earliest,
        };
    }

    let (id, _, _) = &candidates[0];
    if let Some(account) = store.accounts.get(id) {
        if let Some(token) = AccountStore::extract_access_token(&account.auth_json) {
            return PickResult::Found {
                id: id.clone(),
                token,
            };
        }
    }

    PickResult::Exhausted {
        earliest_reset: None,
    }
}

// ────────────────────────────────────────────────────────────────
// Preemptive switch / Ban detection / Switch execution
// ────────────────────────────────────────────────────────────────

fn should_preemptive_switch(state: &ProxyState) -> bool {
    let store = match state.store.lock() {
        Ok(s) => s,
        Err(_) => return false,
    };

    let (t5h, tw, fg) = (
        store.settings.proxy_threshold_5h as f64,
        store.settings.proxy_threshold_weekly as f64,
        store.settings.proxy_free_guard as f64,
    );

    if t5h == 0.0 && tw == 0.0 && fg == 0.0 {
        return false;
    }

    let current_id = match &store.current {
        Some(id) => id,
        None => return false,
    };

    let account = match store.accounts.get(current_id) {
        Some(a) => a,
        None => return false,
    };

    if account.is_banned || account.is_token_invalid || account.is_logged_out {
        println!("[Proxy] Current account banned/invalid/logged out, triggering preemptive switch");
        return true;
    }

    let quota = match account.cached_quota.as_ref() {
        Some(q) => q,
        None => return false,
    };

    let plan = quota.plan_type.to_lowercase();
    let is_free = plan == "free" || plan == "unknown";

    if is_free && fg > 0.0 && quota.five_hour_left < fg {
        println!(
            "[Proxy] Free protection triggered: {:.0}% < {:.0}%",
            quota.five_hour_left, fg
        );
        return true;
    }
    if t5h > 0.0 && quota.five_hour_left < t5h {
        println!(
            "[Proxy] 5h threshold triggered: {:.0}% < {:.0}%",
            quota.five_hour_left, t5h
        );
        return true;
    }
    if tw > 0.0 && quota.weekly_left < tw {
        println!("[Proxy] Weekly threshold triggered: {:.0}% < {:.0}%", quota.weekly_left, tw);
        return true;
    }
    false
}

fn mark_current_banned(state: &ProxyState) {
    if let Ok(mut store) = state.store.lock() {
        if let Some(current_id) = store.current.clone() {
            if let Some(account) = store.accounts.get_mut(&current_id) {
                account.is_banned = true;
                let name = account.name.clone();
                let _ = store.save();
                println!("[Proxy] Account {} marked as banned", name);
                let _ = state.app_handle.emit("proxy-account-banned", &name);
                // macOS system notification (configurable)
                if store.settings.notify_on_switch {
                    let notify_name = name.clone();
                    std::thread::spawn(move || {
                        let _ = std::process::Command::new("osascript")
                            .arg("-e")
                            .arg(format!(
                                "display notification \"{}\" with title \"Codex Switcher\" subtitle \"Ban detected\"",
                                notify_name
                            ))
                            .output();
                    });
                }
            }
        }
    }
}

/// Mark current account's 5h quota as depleted after 429
/// Mark specified account's 5h quota as depleted
fn mark_account_quota_depleted(state: &ProxyState, account_id: &str) {
    if let Ok(mut store) = state.store.lock() {
        if let Some(account) = store.accounts.get_mut(account_id) {
            if let Some(ref mut q) = account.cached_quota {
                q.five_hour_left = 0.0;
            }
            let _ = store.save();
        }
    }
}

fn mark_current_quota_depleted(state: &ProxyState) {
    if let Ok(mut store) = state.store.lock() {
        if let Some(current_id) = store.current.clone() {
            if let Some(account) = store.accounts.get_mut(&current_id) {
                if let Some(ref mut q) = account.cached_quota {
                    q.five_hour_left = 0.0;
                }
                let _ = store.save();
            }
        }
    }
}

fn do_switch(state: &ProxyState, new_id: &str, reason: SwitchReason) -> Result<(), String> {
    let mut store = state.store.lock().map_err(|e| e.to_string())?;

    // Record account info before switch
    let from_name = store
        .current
        .as_ref()
        .and_then(|id| store.accounts.get(id))
        .map(|a| a.name.clone());
    let from_quota = store
        .current
        .as_ref()
        .and_then(|id| store.accounts.get(id))
        .and_then(|a| a.cached_quota.as_ref())
        .map(|q| q.five_hour_left);

    store.switch_to(new_id)?;
    store.save()?;

    let to_name = store
        .accounts
        .get(new_id)
        .map(|a| a.name.clone())
        .unwrap_or_default();
    let to_quota = store
        .accounts
        .get(new_id)
        .and_then(|a| a.cached_quota.as_ref())
        .map(|q| q.five_hour_left);

    println!("[Proxy] Auto switch → {} ({})", to_name, reason);

    // Log account switch
    state.switch_logger.log_switch(
        from_name.clone(),
        to_name.clone(),
        reason,
        from_quota,
        to_quota,
    );

    state.stats.auto_switches.fetch_add(1, Ordering::Relaxed);
    state.ws_disconnect.notify_waiters();
    let _ = state.app_handle.emit("proxy-account-switched", &to_name);
    let _ = state.app_handle.emit("accounts-updated", ());

    // Read notification settings
    let notify_enabled = store.settings.notify_on_switch;
    let inject_enabled = store.settings.inject_switch_message;

    drop(store); // Release lock

    // macOS system notification (configurable)
    if notify_enabled {
        let from = from_name.unwrap_or_else(|| "None".to_string());
        let notify_msg = format!("{} → {}", from, to_name);
        std::thread::spawn(move || {
            let _ = std::process::Command::new("osascript")
                .arg("-e")
                .arg(format!(
                    "display notification \"{}\" with title \"Codex Switcher\" subtitle \"Auto switch\"",
                    notify_msg
                ))
                .output();
        });
    }

    // Inject WebSocket message marker (configurable, experimental)
    if inject_enabled {
        PENDING_INJECT_MSG.lock().ok().map(|mut msg| {
            *msg = Some(format!("⚡ [Codex Switcher] Switched to {}", to_name));
        });
    }

    Ok(())
}

// ────────────────────────────────────────────────────────────────
// Core request handling
// ────────────────────────────────────────────────────────────────

async fn handle_request(
    state: Arc<ProxyState>,
    req: Request<Incoming>,
) -> Result<Response<ProxyBody>, Infallible> {
    state.stats.total_requests.fetch_add(1, Ordering::Relaxed);

    // ── Health check ──
    if req.method() == Method::GET && req.uri().path() == "/health" {
        let total = state.stats.total_requests.load(Ordering::Relaxed);
        let switches = state.stats.auto_switches.load(Ordering::Relaxed);
        let body = serde_json::json!({
            "status": "ok",
            "total_requests": total,
            "auto_switches": switches,
        });
        return Ok(Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body(full_body(Bytes::from(body.to_string())))
            .unwrap());
    }

    // ── WebSocket upgrade detection ──
    if is_websocket_upgrade(&req) {
        println!("[Proxy] WebSocket upgrade request: {}", req.uri());
        return handle_websocket(state, req).await;
    }

    // 1. Get current token + auth mode
    let (token, is_chatgpt) = match get_current_token(&state) {
        Ok(t) => t,
        Err(e) => return Ok(error_response(StatusCode::SERVICE_UNAVAILABLE, &e)),
    };

    // 2. Extract request metadata + route upstream based on auth mode
    let method = req.method().clone();
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());
    let (upstream_url, upstream_host) = get_upstream(is_chatgpt, &path_and_query);

    // 3. Transparent Header forwarding (official responses-api-proxy logic)
    let mut base_headers = reqwest::header::HeaderMap::new();
    for (name, value) in req.headers() {
        let lower = name.as_str().to_ascii_lowercase();
        if lower == "authorization" || lower == "host" {
            continue;
        }
        if let Ok(rn) = reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes()) {
            if let Ok(rv) = reqwest::header::HeaderValue::from_bytes(value.as_bytes()) {
                base_headers.append(rn, rv);
            }
        }
    }
    if let Ok(host_val) = reqwest::header::HeaderValue::from_str(upstream_host) {
        base_headers.insert(reqwest::header::HOST, host_val);
    }

    // 4. Read request body
    let body_bytes = match req.into_body().collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(e) => {
            eprintln!("[Proxy] Failed to read request body: {}", e);
            return Ok(error_response(StatusCode::BAD_REQUEST, "Failed to read request body"));
        }
    };

    // 5. First forward
    let upstream_resp = match forward_with_token(
        &state,
        &method,
        &upstream_url,
        &base_headers,
        &body_bytes,
        &token,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            return Ok(error_response(
                StatusCode::BAD_GATEWAY,
                &format!("Upstream connection failed: {}", e),
            ))
        }
    };

    let status_code = upstream_resp.status();

    // 6. Ban detection (401/403)
    if status_code == reqwest::StatusCode::UNAUTHORIZED
        || status_code == reqwest::StatusCode::FORBIDDEN
    {
        let resp_bytes = upstream_resp.bytes().await.unwrap_or_default();
        let body_lower = String::from_utf8_lossy(&resp_bytes).to_lowercase();
        let banned = body_lower.contains("deactivated")
            || body_lower.contains("banned")
            || body_lower.contains("suspended")
            || body_lower.contains("account_deactivated");

        if banned {
            println!("[Proxy] Ban detection triggered, marking and switching...");
            mark_current_banned(&state);

            if let Some(resp) =
                try_switch_and_retry(&state, &method, &upstream_url, &base_headers, &body_bytes)
                    .await
            {
                return Ok(resp);
            }
        } else {
            // 401 but not banned, possibly normal expiration or logout.
            println!("[Proxy] Intercepted 401, attempting silent Token refresh...");
            let rt_opt = {
                let store = state.store.lock().unwrap();
                store
                    .current
                    .as_ref()
                    .and_then(|id| store.accounts.get(id))
                    .and_then(|a| a.refresh_token.clone())
            };

            if let Some(rt) = rt_opt {
                match crate::oauth::refresh_access_token(&rt).await {
                    Ok(new_tokens) => {
                        println!("[Proxy] Silent Token refresh successful, retrying request");
                        if let Ok(mut store) = state.store.lock() {
                            if let Some(current_id) = store.current.clone() {
                                if let Some(acc) = store.accounts.get_mut(&current_id) {
                                    AccountStore::apply_refreshed_tokens(
                                        acc,
                                        new_tokens.access_token.clone(),
                                        new_tokens.refresh_token.clone(),
                                        new_tokens.id_token,
                                        new_tokens.expires_in,
                                    );
                                    let _ = store.save();
                                }
                            }
                        }
                        if let Ok(retry_resp) = forward_with_token(
                            &state,
                            &method,
                            &upstream_url,
                            &base_headers,
                            &body_bytes,
                            &new_tokens.access_token,
                        )
                        .await
                        {
                            return Ok(build_stream_response(
                                retry_resp,
                                Some(state.tracker.clone()),
                            ));
                        }
                    }
                    Err(e) => {
                        let lower = e.to_lowercase();
                        if lower.contains("logged out")
                            || lower.contains("invalid_grant")
                            || lower.contains("signed in to another account")
                        {
                            println!("[Proxy] Silent refresh failed (possible global logout/login conflict), marking as logged out and switching: {}", e);
                            if let Ok(mut store) = state.store.lock() {
                                if let Some(current_id) = store.current.clone() {
                                    if let Some(acc) = store.accounts.get_mut(&current_id) {
                                        acc.is_logged_out = true;
                                        let _ = store.save();
                                    }
                                }
                            }
                            if let Some(resp) = try_switch_and_retry(
                                &state,
                                &method,
                                &upstream_url,
                                &base_headers,
                                &body_bytes,
                            )
                            .await
                            {
                                return Ok(resp);
                            }
                        } else {
                            println!("[Proxy] Silent refresh failed (other reason): {}", e);
                        }
                    }
                }
            }
        }

        return Ok(Response::builder()
            .status(status_code.as_u16())
            .header("content-type", "application/json")
            .body(full_body(resp_bytes))
            .unwrap_or_else(|_| error_response(StatusCode::BAD_GATEWAY, "Response build failed")));
    }

    // 7. 429 auto switch
    if status_code == reqwest::StatusCode::TOO_MANY_REQUESTS {
        println!("[Proxy] Received 429, marking quota depleted and switching...");
        mark_current_quota_depleted(&state);

        // Concurrency protection
        if state
            .switching
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let result =
                try_switch_and_retry(&state, &method, &upstream_url, &base_headers, &body_bytes)
                    .await;
            state.switching.store(false, Ordering::SeqCst);

            if let Some(resp) = result {
                return Ok(resp);
            }
        } else {
            // Other request is switching, wait briefly then retry with new token
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if let Ok((new_token, _)) = get_current_token(&state) {
                if let Ok(retry_resp) = forward_with_token(
                    &state,
                    &method,
                    &upstream_url,
                    &base_headers,
                    &body_bytes,
                    &new_token,
                )
                .await
                {
                    return Ok(build_stream_response(
                        retry_resp,
                        Some(state.tracker.clone()),
                    ));
                }
            }
        }

        // Switch failed/accounts exhausted → buffer original 429 response
        let resp_bytes = upstream_resp.bytes().await.unwrap_or_default();
        return Ok(Response::builder()
            .status(429)
            .header("content-type", "application/json")
            .body(full_body(resp_bytes))
            .unwrap_or_else(|_| error_response(StatusCode::TOO_MANY_REQUESTS, "429")));
    }

    // 8. Success response → SSE streaming forward
    let resp = build_stream_response(upstream_resp, Some(state.tracker.clone()));

    // Background preemptive switch check
    let state_clone = state.clone();
    tokio::spawn(async move {
        if should_preemptive_switch(&state_clone) {
            if state_clone
                .switching
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                if let PickResult::Found { id, .. } = pick_next_account(&state_clone) {
                    let _ = do_switch(&state_clone, &id, SwitchReason::QuotaThreshold);
                }
                state_clone.switching.store(false, Ordering::SeqCst);
            }
        }
    });

    Ok(resp)
}

/// Switch and retry (max MAX_429_RETRIES times)
async fn try_switch_and_retry(
    state: &ProxyState,
    method: &hyper::Method,
    upstream_url: &str,
    base_headers: &reqwest::header::HeaderMap,
    body: &Bytes,
) -> Option<Response<ProxyBody>> {
    for attempt in 0..MAX_429_RETRIES {
        match pick_next_account(state) {
            PickResult::Found { id, token } => {
                if let Err(e) = do_switch(state, &id, SwitchReason::Http429) {
                    eprintln!("[Proxy] Switch failed: {}", e);
                    continue;
                }

                match forward_with_token(state, method, upstream_url, base_headers, body, &token)
                    .await
                {
                    Ok(resp) if resp.status() != reqwest::StatusCode::TOO_MANY_REQUESTS => {
                        println!(
                            "[Proxy] Switch retry {} successful ({})",
                            attempt + 1,
                            resp.status()
                        );
                        return Some(build_stream_response(resp, Some(state.tracker.clone())));
                    }
                    Ok(_) => {
                        println!("[Proxy] Still 429 after switch retry {}", attempt + 1);
                        mark_current_quota_depleted(state);
                        continue;
                    }
                    Err(e) => {
                        eprintln!("[Proxy] Forward failed after switch: {}", e);
                        continue;
                    }
                }
            }
            PickResult::Exhausted { earliest_reset } => {
                let msg = if let Some(ts) = earliest_reset {
                    let dt = chrono::DateTime::from_timestamp(ts, 0)
                        .map(|d| d.with_timezone(&chrono::Local).format("%H:%M").to_string())
                        .unwrap_or_else(|| "Unknown".to_string());
                    format!("All account quotas exhausted, earliest recovery: {}", dt)
                } else {
                    "All account quotas exhausted".to_string()
                };
                eprintln!("[Proxy] {}", msg);
                let _ = state.app_handle.emit("proxy-all-exhausted", &msg);
                return None;
            }
        }
    }
    None
}

// ────────────────────────────────────────────────────────────────
// HTTP forwarding and response building
// ────────────────────────────────────────────────────────────────

async fn forward_with_token(
    state: &ProxyState,
    method: &hyper::Method,
    upstream_url: &str,
    base_headers: &reqwest::header::HeaderMap,
    body: &Bytes,
    token: &str,
) -> Result<reqwest::Response, String> {
    let mut headers = base_headers.clone();
    if let Ok(v) = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token)) {
        headers.insert(reqwest::header::AUTHORIZATION, v);
    }

    state
        .client
        .request(
            reqwest::Method::from_bytes(method.as_str().as_bytes())
                .unwrap_or(reqwest::Method::POST),
            upstream_url,
        )
        .headers(headers)
        .body(body.to_vec())
        .send()
        .await
        .map_err(|e| format!("Forward request failed: {}", e))
}

/// SSE streaming response building: copy header + stream body + background usage extraction
fn build_stream_response(
    upstream_resp: reqwest::Response,
    tracker: Option<Arc<TokenTracker>>,
) -> Response<ProxyBody> {
    let status = upstream_resp.status();
    let mut builder = Response::builder().status(status.as_u16());

    // Try to get model info from request (may not be in response header)
    let model_hint = String::new();

    for (name, value) in upstream_resp.headers() {
        if matches!(
            name.as_str(),
            "content-length" | "transfer-encoding" | "connection" | "trailer" | "upgrade"
        ) {
            continue;
        }
        if let Ok(hn) = HeaderName::from_bytes(name.as_str().as_bytes()) {
            if let Ok(hv) = HeaderValue::from_bytes(value.as_bytes()) {
                builder = builder.header(hn, hv);
            }
        }
    }

    // Streaming + usage extraction
    // Each chunk forwarded directly, also copied to buffer
    // When stream ends (received None) parse buffer to extract usage
    let usage_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let buf_clone = usage_buf.clone();
    let tracker_clone = tracker.clone();

    let raw_stream = upstream_resp.bytes_stream();

    // Use chain to append an "end signal" after original stream ends
    // Use map + closure to trigger usage parsing after last chunk
    let stream = raw_stream.map(move |result| match result {
        Ok(bytes) => {
            if let Ok(mut buf) = buf_clone.lock() {
                buf.extend_from_slice(&bytes);
            }
            Ok(Frame::data(bytes))
        }
        Err(e) => Err(e.to_string()),
    });

    // Use chain + once to trigger parsing after stream ends
    let buf_for_end = usage_buf;
    let end_signal = futures_util::stream::once(async move {
        // Stream ended, parse buffer
        if let Some(tracker) = tracker_clone {
            if let Ok(buf) = buf_for_end.lock() {
                if !buf.is_empty() {
                    if let Some(usage) = crate::token_tracker::extract_usage_from_sse(&buf, "") {
                        println!(
                            "[Proxy] Token stats: input={} output={} total={} model={}",
                            usage.input_tokens,
                            usage.output_tokens,
                            usage.total_tokens,
                            usage.model
                        );
                        tracker.record(usage);
                    }
                }
            }
        }
        // No data frame produced, just trigger parsing
        Err("".to_string()) // This Err will be ignored by StreamBody
    })
    // Filter out this empty error, don't let it reach client
    .filter(|_| futures_util::future::ready(false));

    let combined = stream.chain(end_signal);

    builder
        .body(BodyExt::boxed(StreamBody::new(combined)))
        .unwrap_or_else(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Stream build failed"))
}

/// Full body wrapper (for error responses and small data)
fn full_body(bytes: Bytes) -> ProxyBody {
    Full::new(bytes).map_err(|_| String::new()).boxed()
}

// ────────────────────────────────────────────────────────────────
// WebSocket proxy
// ────────────────────────────────────────────────────────────────

/// Detect if request is WebSocket upgrade
fn is_websocket_upgrade(req: &Request<Incoming>) -> bool {
    req.headers()
        .get("upgrade")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_lowercase().contains("websocket"))
        .unwrap_or(false)
}

/// Handle WebSocket proxy: connect upstream + bidirectional bridge
async fn handle_websocket(
    state: Arc<ProxyState>,
    mut req: Request<Incoming>,
) -> Result<Response<ProxyBody>, Infallible> {
    // 1. Get token and upstream address
    let (mut token, mut is_chatgpt) = match get_current_token(&state) {
        Ok(t) => t,
        Err(e) => return Ok(error_response(StatusCode::SERVICE_UNAVAILABLE, &e)),
    };

    // Precheck: if current account has no quota, switch first then connect
    {
        let should_switch = {
            let store = match state.store.lock() {
                Ok(s) => s,
                Err(_) => return Ok(error_response(StatusCode::INTERNAL_SERVER_ERROR, "Lock failed")),
            };
            if let Some(current_id) = &store.current {
                store
                    .accounts
                    .get(current_id)
                    .and_then(|a| {
                        if a.is_banned || a.is_token_invalid || a.is_logged_out {
                            return Some(true);
                        }
                        a.cached_quota.as_ref().map(|q| {
                            let is_free = q.plan_type.to_lowercase() == "free";
                            if is_free {
                                q.five_hour_left <= 0.0
                            } else {
                                q.five_hour_left <= 0.0 || q.weekly_left <= 0.0
                            }
                        })
                    })
                    .unwrap_or(false)
            } else {
                false
            }
        };

        if should_switch {
            println!("[Proxy] WebSocket precheck: current account has no quota, attempting switch...");
            // Try up to 3 candidate accounts, check API to confirm quota before switching
            for _attempt in 0..3 {
                if let PickResult::Found {
                    id,
                    token: new_token,
                } = pick_next_account(&state)
                {
                    // Check API to confirm candidate really has quota
                    let has_quota = {
                        let (at, aid, rt) = {
                            let store = state.store.lock().map_err(|e| e.to_string()).ok();
                            if let Some(s) = store {
                                let acc = s.accounts.get(&id);
                                acc.map(|a| {
                                    (
                                        AccountStore::extract_access_token(&a.auth_json),
                                        AccountStore::extract_account_id(&a.auth_json),
                                        a.refresh_token.clone(),
                                    )
                                })
                                .unwrap_or((None, None, None))
                            } else {
                                (None, None, None)
                            }
                        };
                        if let Some(access_token) = at {
                            match crate::usage::UsageFetcher::fetch_usage_direct(
                                access_token,
                                aid,
                                rt,
                                false,
                            )
                            .await
                            {
                                Ok((usage, _)) => {
                                    // Update cache
                                    if let Ok(mut store) = state.store.lock() {
                                        if let Some(acc) = store.accounts.get_mut(&id) {
                                            acc.cached_quota = Some(crate::account::CachedQuota {
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
                                            let _ = store.save();
                                        }
                                    }
                                    usage.five_hour_left > 0 && usage.weekly_left > 0
                                }
                                Err(e) => {
                                    println!("[Proxy] Precheck candidate quota query failed: {}", e);
                                    false
                                }
                            }
                        } else {
                            false
                        }
                    };

                    if has_quota {
                        if do_switch(&state, &id, SwitchReason::WebSocketPrecheck).is_ok() {
                            is_chatgpt = new_token.starts_with("eyJ");
                            token = new_token;
                            println!("[Proxy] WebSocket precheck switch successful (quota confirmed)");
                        }
                        break;
                    } else {
                        println!("[Proxy] Candidate has no quota, skipping to find next...");
                        // Mark as depleted, don't select next time
                        mark_account_quota_depleted(&state, &id);
                    }
                } else {
                    println!("[Proxy] No available candidates");
                    break;
                }
            }
        }
    }

    let path = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());
    let (http_url, _upstream_host) = get_upstream(is_chatgpt, &path);

    // http(s):// → ws(s)://
    let ws_url = http_url
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);

    // 2. Build upstream WebSocket request (transparent header forwarding + token injection)
    let mut upstream_req: tungstenite::http::Request<()> =
        match ws_url.as_str().into_client_request() {
            Ok(r) => r,
            Err(e) => {
                return Ok(error_response(
                    StatusCode::BAD_GATEWAY,
                    &format!("WebSocket request build failed: {}", e),
                ))
            }
        };

    // Forward client header (exclude WebSocket handshake specific headers, generated by into_client_request)
    for (name, value) in req.headers() {
        let lower = name.as_str().to_lowercase();
        if matches!(
            lower.as_str(),
            "authorization"
                | "host"
                | "upgrade"
                | "connection"
                | "sec-websocket-key"
                | "sec-websocket-version"
                | "sec-websocket-extensions"
        ) {
            continue;
        }
        upstream_req
            .headers_mut()
            .insert(name.clone(), value.clone());
    }

    // Inject token
    if let Ok(auth_val) = HeaderValue::from_str(&format!("Bearer {}", token)) {
        upstream_req
            .headers_mut()
            .insert(hyper::header::AUTHORIZATION, auth_val);
    }

    // 3. Connect upstream WebSocket (auto switch and reconnect on auth failure)
    let connect_result = tokio_tungstenite::connect_async(upstream_req).await;

    let (upstream_ws, upstream_handshake_resp) = match connect_result {
        Ok(conn) => conn,
        Err(e) => {
            let err_lower = e.to_string().to_lowercase();
            let is_auth_err = err_lower.contains("401")
                || err_lower.contains("403")
                || err_lower.contains("unauthorized")
                || err_lower.contains("forbidden");

            if !is_auth_err {
                eprintln!("[Proxy] WebSocket upstream connection failed: {}", e);
                return Ok(error_response(
                    StatusCode::BAD_GATEWAY,
                    &format!("WebSocket upstream connection failed: {}", e),
                ));
            }

            println!("[Proxy] WebSocket auth failed ({}), attempting switch and reconnect...", e);
            mark_current_quota_depleted(&state);

            // Switch and reconnect, try up to 3 accounts
            let mut retry_conn = None;
            for _attempt in 0..3 {
                if let PickResult::Found { id, token: new_tok } = pick_next_account(&state) {
                    if do_switch(&state, &id, SwitchReason::WebSocketPrecheck).is_err() {
                        continue;
                    }
                    let new_chatgpt = new_tok.starts_with("eyJ");
                    let (new_url, _) = get_upstream(new_chatgpt, &path);
                    let ws = new_url.replacen("https://", "wss://", 1).replacen("http://", "ws://", 1);

                    if let Ok(mut r) = ws.as_str().into_client_request() {
                        for (n, v) in req.headers() {
                            let l = n.as_str().to_lowercase();
                            if matches!(l.as_str(), "authorization"|"host"|"upgrade"|"connection"|"sec-websocket-key"|"sec-websocket-version"|"sec-websocket-extensions") { continue; }
                            r.headers_mut().insert(n.clone(), v.clone());
                        }
                        if let Ok(av) = HeaderValue::from_str(&format!("Bearer {}", new_tok)) {
                            r.headers_mut().insert(hyper::header::AUTHORIZATION, av);
                        }
                        if let Ok(c) = tokio_tungstenite::connect_async(r).await {
                            println!("[Proxy] WebSocket switch and reconnect successful");
                            retry_conn = Some(c);
                            break;
                        }
                    }
                } else {
                    break;
                }
            }

            match retry_conn {
                Some(conn) => conn,
                None => {
                    return Ok(error_response(
                        StatusCode::BAD_GATEWAY,
                        "All account WebSocket connections failed",
                    ));
                }
            }
        }
    };

    println!("[Proxy] WebSocket upstream connected");

    // 4. Calculate Sec-WebSocket-Accept to reply to client
    let ws_key = req
        .headers()
        .get("sec-websocket-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let accept_key = tungstenite::handshake::derive_accept_key(ws_key.as_bytes());

    // 5. Extract hyper upgrade handle (must be before returning 101)
    let on_upgrade = hyper::upgrade::on(&mut req);

    // 6. Build 101 response, forward upstream response header (x-codex-turn-state etc.)
    let mut response_builder = Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header("Upgrade", "websocket")
        .header("Connection", "Upgrade")
        .header("Sec-WebSocket-Accept", &accept_key);

    // Forward upstream response header (exclude WebSocket handshake headers)
    for (name, value) in upstream_handshake_resp.headers() {
        let lower = name.as_str().to_lowercase();
        if matches!(
            lower.as_str(),
            "upgrade"
                | "connection"
                | "sec-websocket-accept"
                | "sec-websocket-extensions"
                | "content-length"
                | "transfer-encoding"
        ) {
            continue;
        }
        if let Ok(hn) = HeaderName::from_bytes(name.as_str().as_bytes()) {
            response_builder = response_builder.header(hn, value.clone());
        }
    }

    let response = response_builder
        .body(full_body(Bytes::new()))
        .unwrap_or_else(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "101 build failed"));

    // 7. Background task: bidirectional bridge after upgrade completes
    let disconnect = state.ws_disconnect.clone();
    tokio::spawn(async move {
        match on_upgrade.await {
            Ok(upgraded) => {
                let io = TokioIo::new(upgraded);
                let mut client_ws = tokio_tungstenite::WebSocketStream::from_raw_socket(
                    io,
                    tungstenite::protocol::Role::Server,
                    None,
                )
                .await;

                println!("[Proxy] WebSocket client upgraded, starting bridge");

                // Check if there's a pending switch notification message to inject
                let inject_text = PENDING_INJECT_MSG.lock().ok().and_then(|mut m| m.take());
                if let Some(msg_text) = inject_text {
                    let inject_json = serde_json::json!({
                        "type": "response.output_text.delta",
                        "delta": format!("\n{}\n", msg_text)
                    });
                    let _ = futures_util::SinkExt::send(
                        &mut client_ws,
                        tungstenite::Message::Text(inject_json.to_string().into()),
                    )
                    .await;
                    println!("[Proxy] Injected switch notification to WebSocket");
                }

                bridge_websockets(client_ws, upstream_ws, disconnect, state).await;
                println!("[Proxy] WebSocket connection closed");
            }
            Err(e) => eprintln!("[Proxy] WebSocket upgrade failed: {}", e),
        }
    });

    Ok(response)
}

/// Detect if WebSocket message is rate limit error
/// Only match response.failed type error messages, avoid false positives on rate_limit fields in normal messages
/// Rate limit keywords (for fast text matching)
const RATE_LIMIT_KEYWORDS: &[&str] = &[
    "rate_limit",
    "rate limit",
    "usage_limit",
    "usage limit",
    "too many requests",
    "insufficient_quota",
    "billing_hard_limit",
    "tokens per min",
    "requests per min",
];

fn detect_ws_rate_limit(msg: &tungstenite::Message) -> bool {
    if let tungstenite::Message::Text(ref text) = msg {
        let lower = text.to_lowercase();

        // Fast text matching
        let matched = RATE_LIMIT_KEYWORDS.iter().any(|kw| lower.contains(kw));
        if !matched {
            return false;
        }

        println!("[Proxy] WS message contains rate limit keywords");

        if let Ok(val) = serde_json::from_str::<serde_json::Value>(text) {
            let msg_type = val.get("type").and_then(|v| v.as_str()).unwrap_or("");

            // response.failed / error type direct detection
            if msg_type == "response.failed" || msg_type == "error" {
                println!("[Proxy] WS rate limit: type={}", msg_type);
                return true;
            }

            // Has error field also counts
            if val.get("response").and_then(|r| r.get("error")).is_some()
                || val.get("error").is_some()
            {
                println!("[Proxy] WS rate limit: has error field");
                return true;
            }
        }

        // JSON parse failed or no error field, but text clearly contains rate limit message
        if lower.contains("hit your usage limit")
            || lower.contains("rate limit reached")
            || lower.contains("too many requests")
        {
            println!("[Proxy] WS rate limit: text fallback match");
            return true;
        }
    }
    false
}

/// Ban keywords
const BANNED_KEYWORDS: &[&str] = &[
    "deactivated",
    "banned",
    "suspended",
    "account_deactivated",
    "deactivated_workspace",
];

fn detect_ws_banned(msg: &tungstenite::Message) -> bool {
    if let tungstenite::Message::Text(ref text) = msg {
        let lower = text.to_lowercase();

        // Fast text matching
        if !BANNED_KEYWORDS.iter().any(|kw| lower.contains(kw)) {
            return false;
        }

        if let Ok(val) = serde_json::from_str::<serde_json::Value>(text) {
            let msg_type = val.get("type").and_then(|v| v.as_str()).unwrap_or("");

            if msg_type == "response.failed" || msg_type == "error" {
                return true;
            }

            // error field contains ban keywords
            if val.get("response").and_then(|r| r.get("error")).is_some()
                || val.get("error").is_some()
            {
                return true;
            }
        }
    }
    false
}

/// Bidirectional bridge between two WebSocket connections
/// - Switch signal → disconnect
/// - Detect rate limit/ban message → disconnect (proxy will precheck switch on next connection)
async fn bridge_websockets<S1, S2>(
    client: S1,
    upstream: S2,
    disconnect: Arc<tokio::sync::Notify>,
    state: Arc<ProxyState>,
) where
    S1: futures_util::Stream<Item = Result<tungstenite::Message, tungstenite::Error>>
        + futures_util::Sink<tungstenite::Message, Error = tungstenite::Error>
        + Unpin,
    S2: futures_util::Stream<Item = Result<tungstenite::Message, tungstenite::Error>>
        + futures_util::Sink<tungstenite::Message, Error = tungstenite::Error>
        + Unpin,
{
    let (mut client_write, mut client_read) = client.split();
    let (mut upstream_write, mut upstream_read) = upstream.split();

    let client_to_upstream = async {
        while let Some(msg) = client_read.next().await {
            match msg {
                Ok(msg) => {
                    if msg.is_close() {
                        let _ = upstream_write.send(msg).await;
                        break;
                    }
                    if upstream_write.send(msg).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    };

    let state_clone = state.clone();
    let upstream_to_client = async {
        while let Some(msg) = upstream_read.next().await {
            match msg {
                Ok(msg) => {
                    // Detect rate limit error (only parse response.failed type)
                    if detect_ws_rate_limit(&msg) {
                        println!(
                            "[Proxy] WebSocket detected rate limit error (response.failed), triggering switch..."
                        );
                        mark_current_quota_depleted(&state_clone);
                        let _ = client_write.send(msg).await;
                        if let PickResult::Found { id, .. } = pick_next_account(&state_clone) {
                            let _ = do_switch(&state_clone, &id, SwitchReason::WebSocketRateLimit);
                        }
                        break;
                    }
                    // Detect ban
                    if detect_ws_banned(&msg) {
                        println!("[Proxy] WebSocket detected ban (response.failed), triggering switch...");
                        mark_current_banned(&state_clone);
                        let _ = client_write.send(msg).await;
                        if let PickResult::Found { id, .. } = pick_next_account(&state_clone) {
                            let _ = do_switch(&state_clone, &id, SwitchReason::BannedDetected);
                        }
                        break;
                    }

                    if msg.is_close() {
                        let _ = client_write.send(msg).await;
                        break;
                    }
                    if client_write.send(msg).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    };

    tokio::select! {
        _ = client_to_upstream => {},
        _ = upstream_to_client => {},
        _ = disconnect.notified() => {
            println!("[Proxy] Account switched, disconnecting WebSocket (Codex App will auto-reconnect)");
        },
    }
}

fn error_response(status: StatusCode, message: &str) -> Response<ProxyBody> {
    let body = serde_json::json!({
        "error": {
            "message": message,
            "type": "proxy_error",
            "code": status.as_u16(),
        }
    });

    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(full_body(Bytes::from(body.to_string())))
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(full_body(Bytes::from("internal error")))
                .unwrap()
        })
}
