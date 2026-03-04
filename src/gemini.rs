use crate::settings::AppSettings;
use reqwest::Client;
use serde_json::json;

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

pub async fn rewrite_text(api_key: &str, model: &str, prompt_preset: &str, custom_prompt: &str, original: &str) -> String {
    if api_key.trim().is_empty() {
        eprintln!("⚠️ Gemini rewriting is enabled but no API key is configured; skipping rewrite.");
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
    println!("🤖 [Gemini] Sending rewrite request to {} ({} chars)", model_id, original.len());

    let body = json!({
        "contents": [{
            "parts": [{ "text": prompt }]
        }],
        "generationConfig": {
            "temperature": 0.4,
            "maxOutputTokens": 512
        }
    });

    let client = Client::new();
    let response = match client
        .post(&endpoint)
        .header("x-goog-api-key", api_key)
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("❌ Gemini request failed: {}", e);
            return original.to_string();
        }
    };

    let status = response.status();
    let value: serde_json::Value = match response.json().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("❌ Failed to decode Gemini response (status {}): {}", status, e);
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
            println!("🤖 [Gemini] Got empty response, using original text");
            original.to_string()
        } else {
            println!("🤖 [Gemini] Rewrite complete: \"{}\" -> \"{}\"", original, cleaned);
            cleaned.to_string()
        }
    } else {
        eprintln!(
            "❌ Unexpected Gemini response structure (status {}): {}",
            status,
            value
        );
        original.to_string()
    }
}
