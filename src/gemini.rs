use crate::settings::AppSettings;
use reqwest::Client;
use serde_json::json;
use std::time::Duration;

const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/models";

fn build_prompt(settings: &AppSettings, original: &str) -> String {
    let base_instruction = match settings.gemini_prompt_preset.as_str() {
        "Minimal corrections" => "You are a text rewriter. Take the user's text and ONLY fix minimal grammar, spelling, and punctuation. Do not change the tone or meaning. Return ONLY the corrected text with no explanations, no prefixes, and no extra commentary.",
        "Sound like a pirate" => "You are a text rewriter. Rewrite the user's text so it sounds like a pirate speaking, while preserving the original meaning. Return ONLY the rewritten text with no explanations, no prefixes, and no extra commentary.",
        "Sound like a medieval knight" => "You are a text rewriter. Rewrite the user's text so it sounds like a formal medieval knight speaking, while preserving the original meaning. Return ONLY the rewritten text with no explanations, no prefixes, and no extra commentary.",
        "Custom" => settings.gemini_custom_prompt.as_str(),
        _ => "You are a text rewriter. Apply minimal helpful improvements while preserving the meaning. Return ONLY the rewritten text with no explanations, no prefixes, and no extra commentary.",
    };

    format!(
        "{instruction}\n\nUser text:\n{body}",
        instruction = base_instruction,
        body = original
    )
}

pub async fn rewrite_text(
    api_key: &str,
    model: &str,
    prompt_preset: &str,
    custom_prompt: &str,
    original: &str,
) -> String {
    if api_key.trim().is_empty() {
        crate::echo_warn!(
            "gemini",
            "Rewriting is enabled but no API key is configured; skipping rewrite"
        );
        return original.to_string();
    }

    let model_id = if model.trim().is_empty() {
        "gemini-3.1-flash-lite-preview"
    } else {
        model.trim()
    };
    let endpoint = format!("{}/{}:generateContent", GEMINI_BASE_URL, model_id);

    let settings_stub = AppSettings {
        gemini_prompt_preset: prompt_preset.to_string(),
        gemini_custom_prompt: custom_prompt.to_string(),
        ..Default::default()
    };
    let prompt = build_prompt(&settings_stub, original);
    crate::echo_info!(
        "gemini",
        "Sending rewrite request model={} input_characters={}",
        model_id,
        original.chars().count()
    );

    let body = json!({
        "contents": [{
            "parts": [{ "text": prompt }]
        }],
        "generationConfig": {
            "temperature": 0.4,
            "maxOutputTokens": 512
        }
    });

    let client = match Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            crate::echo_error!("gemini", "Could not initialize HTTP client: {e}");
            return original.to_string();
        }
    };
    let response = match client
        .post(&endpoint)
        .header("x-goog-api-key", api_key)
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            crate::echo_error!("gemini", "Request failed: {}", e);
            return original.to_string();
        }
    };

    let status = response.status();
    let value: serde_json::Value = match response.json().await {
        Ok(v) => v,
        Err(e) => {
            crate::echo_error!(
                "gemini",
                "Failed to decode response status={}: {}",
                status,
                e
            );
            return original.to_string();
        }
    };

    if let Some(text) = value
        .get("candidates")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.get(0))
        .and_then(|p| p.get("text"))
        .and_then(|t| t.as_str())
    {
        let cleaned = text.trim();
        if cleaned.is_empty() {
            crate::echo_warn!("gemini", "Response was empty; using original text");
            original.to_string()
        } else {
            crate::echo_info!(
                "gemini",
                "Rewrite complete output_characters={}",
                cleaned.chars().count()
            );
            cleaned.to_string()
        }
    } else {
        crate::echo_error!("gemini", "Unexpected response structure status={}", status);
        original.to_string()
    }
}
