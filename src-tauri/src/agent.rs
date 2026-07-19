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
    pub action: String, // "insert" | "delete_chars" | "select_all" | "submit" | "rewrite"
    pub content: Option<String>,
    pub char_count: Option<usize>,
}

const MAX_VOICE_COMMAND_LEN: usize = 10_000;
const MAX_CLIPBOARD_CONTEXT_LEN: usize = 50_000;
const MAX_API_KEY_LEN: usize = 1_000;

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
    if req.voice_command.len() > MAX_VOICE_COMMAND_LEN {
        return Err("Voice command exceeds maximum length".into());
    }
    if req.clipboard_context.len() > MAX_CLIPBOARD_CONTEXT_LEN {
        return Err("Clipboard context exceeds maximum length".into());
    }
    if req.api_key.len() > MAX_API_KEY_LEN {
        return Err("API key exceeds maximum length".into());
    }
    crate::http_client::validate_api_url(&req.base_url)?;

    let system_prompt = AGENT_SYSTEM_PROMPT.replace("{CONTEXT}", &req.clipboard_context);

    let url = crate::http_client::build_api_url(&req.base_url, "chat/completions");

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

    let action: AgentAction = serde_json::from_str(&content)
        .map_err(|e| format!("Action parse error: {}: {}", e, content))?;

    Ok(action)
}

#[tauri::command]
pub async fn test_llm_connection(
    base_url: String,
    api_key: String,
    model: String,
) -> Result<String, String> {
    crate::http_client::validate_api_url(&base_url)?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reject_oversized_voice_command() {
        let req = AgentRequest {
            base_url: "https://api.groq.com/openai".into(),
            api_key: "test".into(),
            model: "llama".into(),
            voice_command: "a".repeat(MAX_VOICE_COMMAND_LEN + 1),
            clipboard_context: String::new(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(execute_agent_command(req));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Voice command"));
    }

    #[test]
    fn reject_oversized_clipboard_context() {
        let req = AgentRequest {
            base_url: "https://api.groq.com/openai".into(),
            api_key: "test".into(),
            model: "llama".into(),
            voice_command: "hello".into(),
            clipboard_context: "a".repeat(MAX_CLIPBOARD_CONTEXT_LEN + 1),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(execute_agent_command(req));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Clipboard context"));
    }

    #[test]
    fn reject_oversized_api_key() {
        let req = AgentRequest {
            base_url: "https://api.groq.com/openai".into(),
            api_key: "k".repeat(MAX_API_KEY_LEN + 1),
            model: "llama".into(),
            voice_command: "hello".into(),
            clipboard_context: String::new(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(execute_agent_command(req));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("API key"));
    }

    #[test]
    fn reject_invalid_url_in_agent() {
        let req = AgentRequest {
            base_url: "not-a-url".into(),
            api_key: "test".into(),
            model: "llama".into(),
            voice_command: "hello".into(),
            clipboard_context: String::new(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(execute_agent_command(req));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid URL"));
    }

    #[test]
    fn reject_http_non_localhost_in_agent() {
        let req = AgentRequest {
            base_url: "http://api.groq.com/openai".into(),
            api_key: "test".into(),
            model: "llama".into(),
            voice_command: "hello".into(),
            clipboard_context: String::new(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(execute_agent_command(req));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("HTTPS"));
    }

    #[test]
    fn allow_localhost_http_in_agent() {
        let req = AgentRequest {
            base_url: "http://localhost:1430".into(),
            api_key: "test".into(),
            model: "llama".into(),
            voice_command: "hello".into(),
            clipboard_context: String::new(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(execute_agent_command(req));
        assert!(result.is_err());
        assert!(!result.unwrap_err().contains("HTTPS"));
    }

    #[test]
    fn size_limit_constants_are_reasonable() {
        assert_eq!(MAX_VOICE_COMMAND_LEN, 10_000);
        assert_eq!(MAX_CLIPBOARD_CONTEXT_LEN, 50_000);
        assert_eq!(MAX_API_KEY_LEN, 1_000);
    }
}
