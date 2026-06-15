// Fluence Windows — Agent Mode module
// Sends voice commands + clipboard context to an OpenAI-compatible LLM
// and executes structured actions (insert, delete, select_all, submit).

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AgentRequest {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub voice_command: String,
    pub clipboard_context: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AgentAction {
    pub action: String,   // "insert" | "delete_chars" | "select_all" | "submit" | "rewrite"
    pub content: Option<String>,
    pub char_count: Option<usize>,
}

const AGENT_SYSTEM_PROMPT: &str = r#"You are an AI writing assistant integrated into a voice typing tool.
The user will speak a command. You must interpret it and return a JSON action.

Available actions:
- {"action": "insert", "content": "<text to insert>"}
- {"action": "rewrite", "content": "<rewritten version of the clipboard text>"}
- {"action": "delete_chars", "char_count": <number>}
- {"action": "select_all"}
- {"action": "submit"}
- {"action": "copy", "content": "<text to copy to clipboard>"}

Rules:
- If the user explicitly asks to copy the output, copy to clipboard, or save to clipboard, use "copy"
- If the user says "delete last [N] words/characters/sentences", calculate char_count and use "delete_chars"
- If the user says "make it more professional/formal/concise/casual", use "rewrite" with the improved text
- If the user says "submit", "send", or "press enter", use "submit"
- If the user says "select all", use "select_all"
- Otherwise, if it's new content to insert, use "insert"
- Return ONLY valid JSON, no explanation, no markdown.

Current clipboard/editor context:
{CONTEXT}"#;

#[tauri::command]
pub async fn execute_agent_command(req: AgentRequest) -> Result<AgentAction, String> {
    let system_prompt = AGENT_SYSTEM_PROMPT.replace("{CONTEXT}", &req.clipboard_context);

    // Smart URL parsing: handle both trailing slashes and missing/extra /v1
    let base = req.base_url.trim_end_matches('/');
    let url = if base.to_lowercase().ends_with("/v1") {
        format!("{}/chat/completions", base)
    } else {
        format!("{}/v1/chat/completions", base)
    };

    let body = serde_json::json!({
        "model": req.model,
        "messages": [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": req.voice_command}
        ],
        "temperature": 0.3,
        "max_tokens": 512,
        "response_format": {"type": "json_object"}
    });

    let resp = crate::http_client::CLIENT
        .post(&url)
        .bearer_auth(&req.api_key)
        .timeout(std::time::Duration::from_secs(20))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("LLM API error {}: {}", status, text));
    }

    #[derive(Deserialize)]
    struct Choice {
        message: Message,
    }
    #[derive(Deserialize)]
    struct Message {
        content: String,
    }
    #[derive(Deserialize)]
    struct ChatResp {
        choices: Vec<Choice>,
    }

    let chat_resp: ChatResp = resp
        .json()
        .await
        .map_err(|e| format!("JSON parse error: {}", e))?;

    let content = chat_resp
        .choices
        .into_iter()
        .next()
        .map(|c| c.message.content)
        .ok_or_else(|| "Empty response from LLM".to_string())?;

    let action: AgentAction =
        serde_json::from_str(&content).map_err(|e| format!("Action parse error: {}: {}", e, content))?;

    Ok(action)
}

#[tauri::command]
pub async fn test_llm_connection(base_url: String, api_key: String, model: String) -> Result<String, String> {
    // Smart URL parsing: handle both trailing slashes and missing/extra /v1
    let base = base_url.trim_end_matches('/');
    let url = if base.to_lowercase().ends_with("/v1") {
        format!("{}/chat/completions", base)
    } else {
        format!("{}/v1/chat/completions", base)
    };

    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "Say 'OK' in one word."}],
        "max_tokens": 5
    });

    let resp = crate::http_client::CLIENT
        .post(&url)
        .bearer_auth(&api_key)
        .timeout(std::time::Duration::from_secs(10))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Connection failed: {}", e))?;

    if resp.status().is_success() {
        Ok("LLM connection successful".to_string())
    } else {
        Err(format!("LLM auth failed ({})", resp.status()))
    }
}
