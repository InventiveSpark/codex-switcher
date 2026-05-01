//! Token usage statistics and cost calculation
//!
//! Extract usage data from proxy-forwarded SSE streams, calculate costs based on model pricing.

use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Model pricing table (USD / million tokens)
struct ModelPricing {
    input_per_million: f64,
    cached_input_per_million: f64,
    output_per_million: f64,
}

fn get_pricing(model: &str) -> ModelPricing {
    let m = model.to_lowercase();
    // OpenAI 2025 pricing (approximate)
    if m.contains("o3") {
        ModelPricing {
            input_per_million: 2.0,
            cached_input_per_million: 1.0,
            output_per_million: 8.0,
        }
    } else if m.contains("o4-mini") {
        ModelPricing {
            input_per_million: 1.10,
            cached_input_per_million: 0.275,
            output_per_million: 4.40,
        }
    } else if m.contains("codex-mini") {
        ModelPricing {
            input_per_million: 1.50,
            cached_input_per_million: 0.375,
            output_per_million: 6.00,
        }
    } else if m.contains("gpt-4.1") {
        ModelPricing {
            input_per_million: 2.00,
            cached_input_per_million: 0.50,
            output_per_million: 8.00,
        }
    } else if m.contains("gpt-4.1-mini") {
        ModelPricing {
            input_per_million: 0.40,
            cached_input_per_million: 0.10,
            output_per_million: 1.60,
        }
    } else {
        // Default to medium pricing
        ModelPricing {
            input_per_million: 2.00,
            cached_input_per_million: 0.50,
            output_per_million: 8.00,
        }
    }
}

/// Timestamp record (for trend chart)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenHistoryEntry {
    pub timestamp: DateTime<Utc>,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost: f64,
}

/// Single request usage data
#[derive(Debug, Clone)]
pub struct RequestUsage {
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub model: String,
}

/// Cumulative statistics data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageStats {
    pub total_input_tokens: i64,
    pub total_cached_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_tokens: i64,
    pub total_cost_usd: f64,
    pub total_requests: u64,
    /// Token count by model
    pub by_model: HashMap<String, ModelUsage>,
    /// Statistics start time
    pub since: DateTime<Utc>,
    /// Last month comparison data
    pub last_month_cost: Option<f64>,
    pub last_month_tokens: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_usd: f64,
}

impl Default for UsageStats {
    fn default() -> Self {
        Self {
            total_input_tokens: 0,
            total_cached_input_tokens: 0,
            total_output_tokens: 0,
            total_tokens: 0,
            total_cost_usd: 0.0,
            total_requests: 0,
            by_model: HashMap::new(),
            since: Utc::now(),
            last_month_cost: None,
            last_month_tokens: None,
        }
    }
}

/// Token statistics tracker
pub struct TokenTracker {
    stats: Mutex<UsageStats>,
}

impl TokenTracker {
    pub fn new() -> Arc<Self> {
        let mut stats = Self::load_from_disk().unwrap_or_default();
        // Monthly reset
        let now = Utc::now();
        if stats.since.month() != now.month() || stats.since.year() != now.year() {
            let old = stats.clone();
            stats = UsageStats::default();
            stats.last_month_cost = Some(old.total_cost_usd);
            stats.last_month_tokens = Some(old.total_tokens);
        }
        Arc::new(Self {
            stats: Mutex::new(stats),
        })
    }

    /// Record usage for a single request
    pub fn record(&self, usage: RequestUsage) {
        let pricing = get_pricing(&usage.model);

        let uncached_input = usage.input_tokens - usage.cached_input_tokens;
        let cost = (uncached_input as f64 * pricing.input_per_million
            + usage.cached_input_tokens as f64 * pricing.cached_input_per_million
            + usage.output_tokens as f64 * pricing.output_per_million)
            / 1_000_000.0;

        if let Ok(mut stats) = self.stats.lock() {
            stats.total_input_tokens += usage.input_tokens;
            stats.total_cached_input_tokens += usage.cached_input_tokens;
            stats.total_output_tokens += usage.output_tokens;
            stats.total_tokens += usage.total_tokens;
            stats.total_cost_usd += cost;
            stats.total_requests += 1;

            let model_entry = stats
                .by_model
                .entry(usage.model.clone())
                .or_insert(ModelUsage {
                    input_tokens: 0,
                    output_tokens: 0,
                    cost_usd: 0.0,
                });
            model_entry.input_tokens += usage.input_tokens;
            model_entry.output_tokens += usage.output_tokens;
            model_entry.cost_usd += cost;

            // Persist cumulative values
            Self::save_to_disk(&stats);
        }

        // Append timestamp record (for trend chart)
        let entry = TokenHistoryEntry {
            timestamp: Utc::now(),
            model: usage.model,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cost,
        };
        Self::append_history(&entry);
    }

    /// Get current statistics snapshot
    pub fn get_stats(&self) -> UsageStats {
        self.stats.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Get timestamp records for last N days
    pub fn get_history(days: u32) -> Vec<TokenHistoryEntry> {
        let path = Self::history_path();
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        let cutoff = Utc::now() - chrono::Duration::days(days as i64);
        content
            .lines()
            .filter_map(|line| serde_json::from_str::<TokenHistoryEntry>(line).ok())
            .filter(|e| e.timestamp > cutoff)
            .collect()
    }

    /// Reset statistics
    pub fn reset(&self) {
        if let Ok(mut stats) = self.stats.lock() {
            let old = stats.clone();
            *stats = UsageStats::default();
            stats.last_month_cost = Some(old.total_cost_usd);
            stats.last_month_tokens = Some(old.total_tokens);
            Self::save_to_disk(&stats);
        }
    }

    fn history_path() -> PathBuf {
        dirs::home_dir()
            .expect("home dir")
            .join(".codex-switcher")
            .join("token-history.jsonl")
    }

    fn append_history(entry: &TokenHistoryEntry) {
        let path = Self::history_path();
        if let Ok(json) = serde_json::to_string(entry) {
            use std::io::Write;
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                let _ = writeln!(file, "{}", json);
            }
        }
    }

    fn stats_path() -> PathBuf {
        dirs::home_dir()
            .expect("home dir")
            .join(".codex-switcher")
            .join("proxy-usage.json")
    }

    fn load_from_disk() -> Option<UsageStats> {
        let path = Self::stats_path();
        let content = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    }

    fn save_to_disk(stats: &UsageStats) {
        let path = Self::stats_path();
        if let Ok(json) = serde_json::to_string_pretty(stats) {
            let _ = std::fs::write(path, json);
        }
    }
}

/// Extract usage info from accumulated SSE stream data
/// Look for `usage` field in `response.completed` events
pub fn extract_usage_from_sse(data: &[u8], request_model: &str) -> Option<RequestUsage> {
    let text = String::from_utf8_lossy(data);

    // SSE format: each event starts with "data: "
    // Look for events containing "response.completed"
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with("data:") {
            continue;
        }
        let json_str = line.trim_start_matches("data:").trim();
        if !json_str.contains("response.completed") {
            continue;
        }

        // Parse JSON
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) {
            let response = val.get("response")?;
            let usage = response.get("usage")?;

            let model = response
                .get("model")
                .and_then(|m| m.as_str())
                .unwrap_or(request_model)
                .to_string();

            let input_tokens = usage
                .get("input_tokens")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);

            let cached_input = usage
                .get("input_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0);

            let output_tokens = usage
                .get("output_tokens")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);

            let total_tokens = usage
                .get("total_tokens")
                .and_then(|v| v.as_i64())
                .unwrap_or(input_tokens + output_tokens);

            if total_tokens > 0 {
                return Some(RequestUsage {
                    input_tokens,
                    cached_input_tokens: cached_input,
                    output_tokens,
                    total_tokens,
                    model,
                });
            }
        }
    }

    None
}
