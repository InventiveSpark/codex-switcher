use base64::{engine::general_purpose, Engine as _};
use rand::{rng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// OpenAI official OAuth constants (reference: codex-main)
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const AUTH_URL: &str = "https://auth.openai.com/oauth/authorize";
pub const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

/// PKCE-related code
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkceCodes {
    pub code_verifier: String,
    pub code_challenge: String,
}

/// Generate PKCE code pair (matches official: 64 bytes)
pub fn generate_pkce() -> PkceCodes {
    let mut bytes = [0u8; 64];
    rng().fill_bytes(&mut bytes);

    // Generate verifier (Base64URL encoded)
    let code_verifier = general_purpose::URL_SAFE_NO_PAD.encode(bytes);

    // Generate challenge (SHA256 hash then Base64URL encoded)
    let mut hasher = Sha256::new();
    hasher.update(code_verifier.as_bytes());
    let code_challenge = general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize());

    PkceCodes {
        code_verifier,
        code_challenge,
    }
}

/// Token response structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
    pub expires_in: Option<u64>,
}

/// User info pre-extraction (by parsing id_token)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInfo {
    pub email: String,
    pub account_id: Option<String>,
}

/// Exchange authorization code for access token (matches official: manually construct request body)
pub async fn exchange_code(
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<TokenResponse, String> {
    let client = reqwest::Client::new();

    // Official format: manual string concatenation
    let body = format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
        urlencoding::encode(code),
        urlencoding::encode(redirect_uri),
        urlencoding::encode(CLIENT_ID),
        urlencoding::encode(code_verifier)
    );

    let response = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("Token request failed: {}", e))?;

    if !response.status().is_success() {
        let error_body = response.text().await.unwrap_or_default();
        return Err(format!("OpenAI returned error: {}", error_body));
    }

    response
        .json::<TokenResponse>()
        .await
        .map_err(|e| format!("Failed to parse token response: {}", e))
}

/// Use refresh token to get new access token
pub async fn refresh_access_token(refresh_token: &str) -> Result<TokenResponse, String> {
    let client = reqwest::Client::new();

    let params = [
        ("grant_type", "refresh_token"),
        ("client_id", CLIENT_ID),
        ("refresh_token", refresh_token),
        ("scope", "openid profile email offline_access"),
    ];

    let response = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("Token refresh failed: {}", e))?;

    if !response.status().is_success() {
        let error_body = response.text().await.unwrap_or_default();
        return Err(format!("Token refresh rejected: {}", error_body));
    }

    response
        .json::<TokenResponse>()
        .await
        .map_err(|e| format!("Failed to parse refresh response: {}", e))
}

/// Extract user info from ID Token (JWT parsing)
pub fn parse_user_info(id_token: &str) -> Option<UserInfo> {
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() < 2 {
        return None;
    }

    let payload = general_purpose::URL_SAFE_NO_PAD.decode(parts[1]).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&payload).ok()?;

    let email = json.get("email")?.as_str()?.to_string();

    // Get account_id from OpenAI-specific claims
    let account_id = json
        .get("https://api.openai.com/auth")
        .and_then(|v| v.get("chatgpt_account_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Some(UserInfo { email, account_id })
}
