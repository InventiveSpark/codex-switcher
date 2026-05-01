//! Codex Switcher - Usage fetch module
//!
//! Fetch Codex usage information from OpenAI API

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Usage data displayed in frontend
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageDisplay {
    /// Plan type
    pub plan_type: String,
    /// 5-hour window usage percentage
    pub five_hour_used: i32,
    /// 5-hour window remaining percentage
    pub five_hour_left: i32,
    /// 5-hour window label (e.g. "5H Limit")
    pub five_hour_label: String,
    /// 5-hour reset time description
    pub five_hour_reset: String,
    /// 5-hour reset timestamp
    pub five_hour_reset_at: Option<i64>,
    /// Weekly window usage percentage
    pub weekly_used: i32,
    /// Weekly window remaining percentage
    pub weekly_left: i32,
    /// Weekly window label (e.g. "Weekly Limit")
    pub weekly_label: String,
    /// Weekly reset time description
    pub weekly_reset: String,
    /// Weekly reset timestamp
    pub weekly_reset_at: Option<i64>,
    /// Credits balance
    pub credits_balance: Option<f64>,
    /// Has credits
    pub has_credits: bool,
    /// Token valid for CLI (api.openai.com)
    pub is_valid_for_cli: bool,
}

/// Usage fetcher
pub struct UsageFetcher;

impl UsageFetcher {
    /// Fetch usage from API (using provided token directly, does not read auth.json)
    pub async fn fetch_usage_direct(
        access_token: String,
        account_id: Option<String>,
        refresh_token: Option<String>,
        allow_local_refresh: bool,
    ) -> Result<(UsageDisplay, Option<crate::oauth::TokenResponse>), String> {
        let mut current_token = access_token;
        let mut new_tokens: Option<crate::oauth::TokenResponse> = None;

        let client = reqwest::Client::new();
        let user_agent = format!(
            "codex_cli_rs/{} (Mac OS; x86_64) codex-cli",
            env!("CARGO_PKG_VERSION")
        );
        let build_request = |at: &str, aid: &Option<String>| {
            let mut req = client
                .get("https://chatgpt.com/backend-api/wham/usage")
                .header("Authorization", format!("Bearer {}", at))
                .header("User-Agent", &user_agent)
                .header("originator", "codex_cli_rs")
                .header("Accept", "application/json")
                .timeout(std::time::Duration::from_secs(30));
            if let Some(id) = aid {
                req = req.header("ChatGPT-Account-Id", id);
            }
            req
        };

        let mut response = build_request(&current_token, &account_id)
            .send()
            .await
            .map_err(|e| format!("Network request failed: {}", e))?;

        let mut status = response.status();

        // If local refresh allowed, and 401/403 with refresh_token, attempt refresh
        if allow_local_refresh && (status == 401 || status == 403) && refresh_token.is_some() {
            if let Some(ref rt) = refresh_token {
                match crate::oauth::refresh_access_token(rt).await {
                    Ok(token_res) => {
                        current_token = token_res.access_token.clone();
                        new_tokens = Some(token_res);

                        // Retry request
                        response = build_request(&current_token, &account_id)
                            .send()
                            .await
                            .map_err(|e| format!("Retry after refresh failed: {}", e))?;
                        status = response.status();
                    }
                    Err(e) => {
                        let lower = e.to_lowercase();
                        if lower.contains("logged out")
                            || lower.contains("signed in to another account")
                            || lower.contains("invalid_grant")
                        {
                            return Err("ACCOUNT_LOGGED_OUT:You have logged out or signed in to another account, please sign in again"
                                .to_string());
                        }
                    }
                }
            }
        }

        if status == 401 || status == 403 {
            // Read response body to detect account ban
            let body = response.text().await.unwrap_or_default().to_lowercase();
            let is_banned = body.contains("deactivated")
                || body.contains("banned")
                || body.contains("suspended")
                || body.contains("account_deactivated");

            if is_banned {
                return Err("ACCOUNT_BANNED:This account has been banned".to_string());
            }

            if !allow_local_refresh {
                return Err(
                    "Current active account quota API returned 401/403; local refresh_token refresh is disabled, please retry later or trigger a request in Codex".to_string(),
                );
            }
            // If still 401/403 after refresh, mark as invalid
            return Err("TOKEN_INVALID:Authorization expired, please delete this account and sign in again".to_string());
        }

        let text = response
            .text()
            .await
            .map_err(|e| format!("Failed to read response body: {}", e))?;

        let json: Value =
            serde_json::from_str(&text)
            .map_err(|e| format!("Failed to parse JSON response: {}", e))?;

        // Detect soft ban/deactivation response under 200 status, e.g. {"detail":{"code":"deactivated_workspace"}}
        if let Some(detail_code) = json
            .get("detail")
            .and_then(|d| d.get("code"))
            .and_then(|c| c.as_str())
        {
            let code_lower = detail_code.to_lowercase();
            if code_lower.contains("deactivated")
                || code_lower.contains("banned")
                || code_lower.contains("suspended")
            {
                println!("[Usage] Detected account deactivation: detail.code={}", detail_code);
                return Err("ACCOUNT_BANNED:This account has been banned (workspace deactivated)".to_string());
            }
        }

        let display = Self::parse_usage_response(&json)?;

        Ok((display, new_tokens))
    }

    /// Parse usage data from Value
    fn parse_usage_response(json: &Value) -> Result<UsageDisplay, String> {
        let plan_type = json
            .get("plan_type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let rate_limit = json.get("rate_limit");

        // Parse 5-hour window (Primary)
        let primary_val = rate_limit.and_then(|r| r.get("primary_window"));
        let (p_used, p_reset, p_label, p_reset_at) = Self::parse_window(primary_val, "5H Limit");

        // Parse weekly window (Secondary)
        let secondary_val = rate_limit.and_then(|r| r.get("secondary_window"));
        let (s_used, s_reset, s_label, s_reset_at) = Self::parse_window(secondary_val, "Weekly Limit");

        // Parse credits
        let credits = json.get("credits");
        let has_credits = credits
            .and_then(|c| c.get("has_credits"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let unlimited = credits
            .and_then(|c| c.get("unlimited"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let credits_balance = credits
            .and_then(|c| c.get("balance"))
            .and_then(Self::parse_number);

        Ok(UsageDisplay {
            plan_type,
            five_hour_used: p_used,
            five_hour_left: 100 - p_used,
            five_hour_label: p_label,
            five_hour_reset: p_reset,
            five_hour_reset_at: p_reset_at,
            weekly_used: s_used,
            weekly_left: 100 - s_used,
            weekly_label: s_label,
            weekly_reset: s_reset,
            weekly_reset_at: s_reset_at,
            credits_balance,
            has_credits: has_credits || unlimited,
            is_valid_for_cli: true,
        })
    }

    /// Parse window data
    fn parse_window(
        window: Option<&Value>,
        default_label: &str,
    ) -> (i32, String, String, Option<i64>) {
        let window = match window {
            Some(w) => w,
            None => return (0, "Unknown".to_string(), default_label.to_string(), None),
        };

        // Critical fix: use f64 to parse percentage, then round
        let used_percent = window
            .get("used_percent")
            .and_then(Self::parse_number)
            .map(|f| f.round() as i32)
            .unwrap_or(0);

        let reset_at = window
            .get("reset_at")
            .and_then(Self::parse_number)
            .map(|f| f as i64);

        let limit_window_seconds = window
            .get("limit_window_seconds")
            .and_then(Self::parse_number)
            .map(|f| f as i64)
            .unwrap_or(0);

        // Dynamically calculate label
        let label = if limit_window_seconds > 0 {
            Self::get_limits_label(limit_window_seconds)
        } else {
            default_label.to_string()
        };

        let reset_str = if let Some(ts) = reset_at {
            if ts > 0 {
                Self::format_reset(ts)
            } else {
                "Unknown".to_string()
            }
        } else {
            // Try using reset_after_seconds
            let reset_after = window
                .get("reset_after_seconds")
                .or_else(|| window.get("reset_after_sec"))
                .and_then(Self::parse_number)
                .map(|f| f as i64)
                .unwrap_or(0);
            if reset_after > 0 {
                Self::format_duration(reset_after)
            } else {
                "Unknown".to_string()
            }
        };

        (used_percent, reset_str, label, reset_at)
    }

    /// Get human-readable label from window seconds
    fn get_limits_label(seconds: i64) -> String {
        const SECS_PER_HOUR: i64 = 3600;
        const SECS_PER_DAY: i64 = 24 * SECS_PER_HOUR;
        const SECS_PER_WEEK: i64 = 7 * SECS_PER_DAY;

        if seconds <= SECS_PER_HOUR * 5 + 600 {
            "5H Limit".to_string()
        } else if seconds <= SECS_PER_DAY + 600 {
            "24H Limit".to_string()
        } else if seconds <= SECS_PER_WEEK + 3600 {
            "Weekly Limit".to_string()
        } else {
            format!("{}H Limit", (seconds + 3599) / 3600)
        }
    }

    /// Parse number (supports string and number)
    fn parse_number(v: &Value) -> Option<f64> {
        match v {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.parse().ok(),
            _ => None,
        }
    }

    /// Parse integer (supports string and number)
    fn parse_int(v: &Value) -> Option<i32> {
        match v {
            Value::Number(n) => n.as_i64().map(|i| i as i32),
            Value::String(s) => s.parse().ok(),
            _ => None,
        }
    }

    /// Format reset time (timestamp)
    fn format_reset(reset_at: i64) -> String {
        use chrono::{TimeZone, Utc};

        if reset_at == 0 {
            return "Unknown".to_string();
        }

        let reset_time = Utc
            .timestamp_opt(reset_at, 0)
            .single()
            .unwrap_or_else(Utc::now);
        let now = Utc::now();

        let duration = reset_time.signed_duration_since(now);
        Self::format_chrono_duration(duration)
    }

    /// Format duration (seconds)
    fn format_duration(seconds: i64) -> String {
        let hours = seconds / 3600;
        let minutes = (seconds % 3600) / 60;

        if hours > 24 {
            let days = hours / 24;
            format!("Resets in {} days", days)
        } else if hours > 0 {
            format!("Resets in {}h {}m", hours, minutes)
        } else if minutes > 0 {
            format!("Resets in {}m", minutes)
        } else {
            "Reset soon".to_string()
        }
    }

    /// Format chrono Duration
    fn format_chrono_duration(duration: chrono::Duration) -> String {
        let hours = duration.num_hours();
        let minutes = duration.num_minutes() % 60;

        if hours > 24 {
            let days = hours / 24;
            format!("Resets in {} days", days)
        } else if hours > 0 {
            format!("Resets in {}h {}m", hours, minutes.abs())
        } else if minutes > 0 {
            format!("Resets in {}m", minutes)
        } else {
            "Reset soon".to_string()
        }
    }
}
