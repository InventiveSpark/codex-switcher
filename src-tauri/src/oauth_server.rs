use crate::oauth;
use base64::{engine::general_purpose, Engine as _};
use rand::{rng, RngCore};
use std::sync::Mutex;
use std::sync::OnceLock;
use tauri::{AppHandle, Emitter};
use tauri_plugin_opener::OpenerExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::{Duration, Instant};
use url::Url;

/// Use OnceLock instead of lazy_static to store OAuth flow temp data
static PENDING_LOGIN: OnceLock<Mutex<Option<PendingLogin>>> = OnceLock::new();
static CALLBACK_TASK: OnceLock<Mutex<Option<tokio::task::JoinHandle<()>>>> = OnceLock::new();

fn get_pending_login() -> &'static Mutex<Option<PendingLogin>> {
    PENDING_LOGIN.get_or_init(|| Mutex::new(None))
}

fn get_callback_task() -> &'static Mutex<Option<tokio::task::JoinHandle<()>>> {
    CALLBACK_TASK.get_or_init(|| Mutex::new(None))
}

struct PendingLogin {
    pkce: oauth::PkceCodes,
    port: u16,
}

/// Generate state matching official (Base64-encoded 32-byte random)
fn generate_state() -> String {
    let mut bytes = [0u8; 32];
    rng().fill_bytes(&mut bytes);
    general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Official fixed port
const DEFAULT_PORT: u16 = 1455;

/// Prepare OAuth flow and return auth URL
#[tauri::command]
pub async fn start_oauth_login(app_handle: AppHandle) -> Result<String, String> {
    // 1. Abort old callback task to avoid port conflicts
    if let Ok(mut task_slot) = get_callback_task().lock() {
        if let Some(task) = task_slot.take() {
            task.abort();
        }
    }

    // Wait for port release from old task
    tokio::time::sleep(Duration::from_millis(100)).await;

    let listener = TcpListener::bind(format!("127.0.0.1:{}", DEFAULT_PORT))
        .await
        .map_err(|e| {
            format!(
                "Cannot bind to local port {}: {}. Close the process using this port and retry.",
                DEFAULT_PORT, e
            )
        })?;
    let port = DEFAULT_PORT;

    // 2. Generate PKCE and State (match official)
    let pkce = oauth::generate_pkce();
    let state = generate_state();
    let redirect_uri = format!("http://localhost:{}/auth/callback", port);

    // 3. Build auth URL (exactly as official: manual concat, no special char encoding)
    let qs = format!(
        "response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&id_token_add_organizations=true&codex_cli_simplified_flow=true&state={}&originator=codex_vscode",
        oauth::CLIENT_ID,
        redirect_uri,
        "openid profile email offline_access",
        pkce.code_challenge,
        state
    );

    let auth_url = format!("{}?{}", oauth::AUTH_URL, qs);

    // 4. Save state, start listener task
    {
        let mut pending = get_pending_login()
            .lock()
            .map_err(|_| "Login flow state lock error")?;
        *pending = Some(PendingLogin {
            pkce: pkce.clone(),
            port,
        });
    }

    // 5. Start async listener
    let app_handle_clone = app_handle.clone();
    let handle = tokio::spawn(async move {
        handle_callback(listener, app_handle_clone, state).await;
    });
    if let Ok(mut task_slot) = get_callback_task().lock() {
        *task_slot = Some(handle);
    }

    // 6. Open browser
    let _ = app_handle.opener().open_url(&auth_url, None::<String>);

    Ok(auth_url)
}

/// Listen for callback
async fn handle_callback(listener: TcpListener, app_handle: AppHandle, expected_state: String) {
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            eprintln!("[OAuth] Callback listener timed out, no valid auth code received");
            return;
        }

        let accepted = match tokio::time::timeout(remaining, listener.accept()).await {
            Ok(result) => result,
            Err(_) => {
                eprintln!("[OAuth] Callback listener timed out, no valid auth code received");
                return;
            }
        };

        let (mut socket, _) = match accepted {
            Ok(sock) => sock,
            Err(e) => {
                eprintln!("[OAuth] Callback listener connection failed: {}", e);
                continue;
            }
        };

        let mut buffer = [0; 4096];
        let n = match socket.read(&mut buffer).await {
            Ok(n) => n,
            Err(e) => {
                eprintln!("[OAuth] Failed to read callback request: {}", e);
                continue;
            }
        };
        if n == 0 {
            continue;
        }
        let request = String::from_utf8_lossy(&buffer[..n]);

        if let Some(code) = extract_oauth_code_from_request(&request, &expected_state) {
            // Send success HTML and notify frontend
            let response = "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\r\n\
                <html><body><h1>Authorization Successful</h1><p>Connected to OpenAI successfully. You may close this window and return to the app.</p>\
                <script>setTimeout(() => window.close(), 3000)</script></body></html>";
            let _ = socket.write_all(response.as_bytes()).await;

            if let Err(e) = app_handle.emit("oauth-callback-received", code) {
                eprintln!("Failed to emit oauth-callback-received event: {}", e);
            }
            return;
        }

        let response = "HTTP/1.1 400 Bad Request\r\n\r\nAuthorization failed: State verification failed or missing parameters";
        let _ = socket.write_all(response.as_bytes()).await;
    }
}

fn extract_oauth_code_from_request(request: &str, expected_state: &str) -> Option<String> {
    let first_line = request.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() <= 1 {
        return None;
    }

    let callback_url = format!("http://localhost{}", parts[1]);
    let url = Url::parse(&callback_url).ok()?;
    let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();

    let code = params.get("code")?;
    let state = params.get("state")?;
    if state != expected_state {
        return None;
    }

    Some(code.to_string())
}

/// Final step: exchange captured Code for Token (triggered by frontend)
#[tauri::command]
pub async fn complete_oauth_login(code: String) -> Result<oauth::TokenResponse, String> {
    // Extract data and immediately release lock to avoid holding MutexGuard across await
    let (code_verifier, port) = {
        let mut pending_lock = get_pending_login().lock().map_err(|_| "Lock poisoned")?;
        let pending = pending_lock.take().ok_or("Login flow expired or not started")?;
        (pending.pkce.code_verifier, pending.port)
    };

    let redirect_uri = format!("http://localhost:{}/auth/callback", port);

    oauth::exchange_code(&code, &redirect_uri, &code_verifier).await
}

#[cfg(test)]
mod tests {
    use super::extract_oauth_code_from_request;

    #[test]
    fn extract_code_success_when_state_matches() {
        let req = "GET /auth/callback?code=abc123&state=s1 HTTP/1.1\r\nHost: localhost\r\n\r\n";
        assert_eq!(
            extract_oauth_code_from_request(req, "s1"),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn extract_code_returns_none_when_state_mismatch() {
        let req = "GET /auth/callback?code=abc123&state=s2 HTTP/1.1\r\nHost: localhost\r\n\r\n";
        assert_eq!(extract_oauth_code_from_request(req, "s1"), None);
    }

    #[test]
    fn extract_code_returns_none_when_invalid_request_line() {
        let req = "INVALID\r\nHost: localhost\r\n\r\n";
        assert_eq!(extract_oauth_code_from_request(req, "s1"), None);
    }
}
