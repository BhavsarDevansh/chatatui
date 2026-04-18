use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone, Debug)]
pub struct McpClient {
    #[allow(dead_code)]
    pub name: String,
    pub url: String,
    pub api_key: Option<String>,
    client: reqwest::Client,
    session_id: Arc<Mutex<Option<String>>>,
}

#[derive(Debug, Clone)]
pub struct McpTool {
    pub name: String,
    pub title: String,
    pub description: String,
    pub server_name: String,
    pub input_schema: Value,
}

impl McpClient {
    pub fn new(name: String, url: String, api_key: Option<String>) -> Self {
        Self {
            name,
            url,
            api_key,
            client: reqwest::Client::new(),
            session_id: Arc::new(Mutex::new(None)),
        }
    }

    async fn send_json_rpc(&self, body: Value) -> Result<Value, String> {
        let mut builder = self.client.post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");

        if let Some(key) = &self.api_key {
            builder = builder.bearer_auth(key);
        }

        {
            let session_lock = self.session_id.lock().await;
            if let Some(sid) = session_lock.as_ref() {
                builder = builder.header("Mcp-Session-Id", sid.clone());
            }
        }

        match builder.json(&body).send().await {
            Ok(resp) => {
                if let Some(sid) = resp.headers().get("Mcp-Session-Id").and_then(|v| v.to_str().ok()) {
                    let mut session_lock = self.session_id.lock().await;
                    *session_lock = Some(sid.to_string());
                }

                let status = resp.status();
                if !status.is_success() {
                    let body_text = resp.text().await.unwrap_or_default();
                    return Err(format!("HTTP {}: {}", status, body_text));
                }

                let text = resp.text().await.map_err(|e| format!("Failed to read response body: {}", e))?;

                if text.trim().is_empty() {
                    return Ok(json!({}));
                }

                if let Ok(json) = serde_json::from_str::<Value>(&text) {
                    return Ok(json);
                }

                let json_text = Self::extract_sse_json(&text)?;
                serde_json::from_str(&json_text).map_err(|e| format!("Failed to parse response: {}. Body: {}", e, json_text))
            }
            Err(e) => Err(format!("Connection failed: {}", e)),
        }
    }

    fn extract_sse_json(text: &str) -> Result<String, String> {
        let lines: Vec<&str> = text.lines().collect();
        let mut i = 0;

        while i < lines.len() {
            let line = lines[i].trim();

            if line.starts_with("data:") {
                let data_part = line.strip_prefix("data:").unwrap_or("").trim();

                if data_part.starts_with('{') {
                    return Ok(data_part.to_string());
                }

                if data_part.is_empty() && i + 1 < lines.len() {
                    let next_line = lines[i + 1].trim();
                    if next_line.starts_with('{') {
                        return Ok(next_line.to_string());
                    }
                }
            }

            i += 1;
        }

        Err(format!("No JSON data found in SSE response. Body: {}", text))
    }

    async fn perform_handshake(&self) -> Result<(), String> {
        let initialize_request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "chatatui",
                    "version": "0.1.0"
                }
            }
        });

        self.send_json_rpc(initialize_request).await?;

        let session_lock = self.session_id.lock().await;
        if let Some(sid) = session_lock.as_ref() {
            let sse_url = format!("{}/sse", self.url.trim_end_matches('/'));
            let mut builder = self.client.get(&sse_url)
                .header("Accept", "text/event-stream")
                .header("Mcp-Session-Id", sid.clone());

            if let Some(key) = &self.api_key {
                builder = builder.bearer_auth(key);
            }

            let _ = builder.send().await;
        }
        drop(session_lock);

        let initialized_notification = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });

        self.send_json_rpc(initialized_notification).await?;

        Ok(())
    }

    pub async fn call_tool(&self, name: &str, args: Value) -> Result<String, String> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": args
            }
        });

        match self.send_json_rpc(request).await {
            Ok(json) => {
                if let Some(content) = json.get("result").and_then(|r| r.get("content")).and_then(|c| c.as_array()).and_then(|arr| arr.first()).and_then(|obj| obj.get("text")).and_then(|t| t.as_str()) {
                    Ok(content.to_string())
                } else {
                    Err("Invalid tool call response format".to_string())
                }
            }
            Err(e) => Err(e),
        }
    }

    pub async fn list_tools(&self) -> Result<Vec<McpTool>, String> {
        self.perform_handshake().await?;

        let request = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        });

        match self.send_json_rpc(request).await {
            Ok(json) => {
                if let Some(tools) = json.get("result").and_then(|r| r.get("tools")).and_then(|t| t.as_array()) {
                    let server_name = self.name.clone();
                    let mcp_tools = tools
                        .iter()
                        .filter_map(|t| {
                            let name = t.get("name")?.as_str()?.to_string();
                            let title = t.get("title").and_then(|v| v.as_str()).map(|s| s.to_string()).unwrap_or_else(|| name.clone());
                            let description = t.get("description")?.as_str()?.to_string();
                            let input_schema = t.get("inputSchema").cloned().unwrap_or(json!({}));
                            Some(McpTool { name, title, description, server_name: server_name.clone(), input_schema })
                        })
                        .collect();
                    Ok(mcp_tools)
                } else {
                    Err("Invalid response format: missing tools in result".to_string())
                }
            }
            Err(e) => {
                if e.contains("No JSON data found in SSE response") {
                    Err("MCP server not responding properly — check if server is running".to_string())
                } else {
                    Err(e)
                }
            }
        }
    }
}
