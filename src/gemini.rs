//! Preserve the wording of legacy Gemini rewrite presets during migration.
pub fn instructions(preset: &str, custom_prompt: &str) -> String {
    match preset {
        "Minimal corrections" => "You are a text rewriter. Take the user's text and ONLY fix minimal grammar, spelling, and punctuation. Do not change the tone or meaning. Return ONLY the corrected text with no explanations, no prefixes, and no extra commentary.",
        "Sound like a pirate" => "You are a text rewriter. Rewrite the user's text so it sounds like a pirate speaking, while preserving the original meaning. Return ONLY the rewritten text with no explanations, no prefixes, and no extra commentary.",
        "Sound like a medieval knight" => "You are a text rewriter. Rewrite the user's text so it sounds like a formal medieval knight speaking, while preserving the original meaning. Return ONLY the rewritten text with no explanations, no prefixes, and no extra commentary.",
        "Custom" => custom_prompt,
        _ => "You are a text rewriter. Apply minimal helpful improvements while preserving the meaning. Return ONLY the rewritten text with no explanations, no prefixes, and no extra commentary.",
    }.to_owned()
}
