use serde_json::{json, Value};
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct ChatClient {
    api_url: String,
    model: String,
    api_key: Option<String>,
}

#[derive(serde::Deserialize, Debug)]
pub struct ModelsResponse {
    pub data: Vec<Model>,
}

#[derive(serde::Deserialize, Debug)]
pub struct Model {
    pub id: String,
}

impl ChatClient {
    pub fn new(api_url: String, model: String, api_key: Option<String>) -> Self {
        Self {
            api_url,
            model,
            api_key,
        }
    }

    pub fn set_model(&mut self, model: String) {
        self.model = model;
    }

    pub async fn list_models(&self) -> Result<Vec<String>, String> {
        let endpoint = if self.api_url.ends_with('/') {
            format!("{}v1/models", self.api_url)
        } else {
            format!("{}/v1/models", self.api_url)
        };

        let client = reqwest::Client::new();
        let mut builder = client.get(&endpoint);

        if let Some(key) = &self.api_key {
            builder = builder.bearer_auth(key);
        }

        match builder.send().await {
            Ok(resp) => {
                if !resp.status().is_success() {
                    return Err(format!(
                        "HTTP {}: {}",
                        resp.status(),
                        resp.text().await.unwrap_or_default()
                    ));
                }
                match resp.json::<ModelsResponse>().await {
                    Ok(models_resp) => Ok(models_resp.data.iter().map(|m| m.id.clone()).collect()),
                    Err(e) => Err(format!("Failed to parse models response: {}", e)),
                }
            }
            Err(e) => Err(format!("Connection failed: {}", e)),
        }
    }

    pub async fn send_chat_message(
        &self,
        history: Vec<Value>,
        tools: Vec<Value>,
        tx: mpsc::Sender<crate::app::AsyncEvent>,
    ) {
        let model = self.model.clone();
        let api_url = self.api_url.clone();
        let api_key = self.api_key.clone();

        tokio::spawn(async move {
            let endpoint = if api_url.ends_with('/') {
                format!("{}v1/chat/completions", api_url)
            } else {
                format!("{}/v1/chat/completions", api_url)
            };

            let messages = history;

            let mut request_body = json!({
                "model": model,
                "messages": messages,
                "stream": true
            });

            if !tools.is_empty() {
                request_body["tools"] = Value::Array(tools);
            }

            let client = reqwest::Client::new();
            let mut builder = client.post(&endpoint);

            if let Some(key) = &api_key {
                builder = builder.bearer_auth(key);
            }

            let response = builder.json(&request_body).send().await;

            match response {
                Ok(resp) => {
                    let status = resp.status();
                    let response_text = resp.text().await.unwrap_or_default();

                    if !status.is_success() {
                        let error_msg = if response_text.is_empty() {
                            format!("API Error: HTTP {}", status)
                        } else if let Ok(json) =
                            serde_json::from_str::<serde_json::Value>(&response_text)
                        {
                            if let Some(msg) = json
                                .get("error")
                                .and_then(|e| e.get("message").and_then(|m| m.as_str()))
                            {
                                format!("API Error (HTTP {}): {}", status, msg)
                            } else {
                                format!(
                                    "API Error (HTTP {}): {}",
                                    status,
                                    response_text.lines().take(3).collect::<Vec<_>>().join(" ")
                                )
                            }
                        } else {
                            format!(
                                "API Error (HTTP {}): {}",
                                status,
                                response_text.lines().take(3).collect::<Vec<_>>().join(" ")
                            )
                        };

                        let _ = tx.send(crate::app::AsyncEvent::ChatError(error_msg)).await;
                        let _ = tx.send(crate::app::AsyncEvent::ChatFinished).await;
                        return;
                    }

                    let mut accumulated_tool_calls: std::collections::HashMap<String, (String, String)> = std::collections::HashMap::new();

                    for line in response_text.lines() {
                        if line.starts_with("data: ") {
                            let json_str = &line[6..];
                            if json_str == "[DONE]" {
                                continue;
                            }
                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(json_str) {
                                // Parse content chunks
                                if let Some(content) =
                                    json["choices"][0]["delta"]["content"].as_str()
                                {
                                    let _ = tx
                                        .send(crate::app::AsyncEvent::ChatChunk(content.to_string()))
                                        .await;
                                }

                                // Parse tool_calls from delta
                                if let Some(tool_calls_array) = json["choices"][0]["delta"]["tool_calls"].as_array() {
                                    for tc in tool_calls_array {
                                        if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                                            let func_name = tc.get("function").and_then(|f| f.get("name")).and_then(|n| n.as_str()).unwrap_or("");
                                            let func_args = tc.get("function").and_then(|f| f.get("arguments")).and_then(|a| a.as_str()).unwrap_or("");

                                            let entry = accumulated_tool_calls.entry(id.to_string()).or_insert((func_name.to_string(), String::new()));
                                            entry.1.push_str(func_args);
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Emit tool calls if any were accumulated
                    for (id, (name, args_str)) in accumulated_tool_calls {
                        if let Ok(args) = serde_json::from_str::<Value>(&args_str) {
                            let args_display = serde_json::to_string_pretty(&args).unwrap_or_else(|_| args_str.clone());
                            let pending = crate::app::PendingToolCall {
                                id,
                                tool_name: name,
                                args,
                                server_name: String::new(), // Will be filled in by the app
                                args_display,
                            };
                            let _ = tx.send(crate::app::AsyncEvent::ToolCallRequested(pending)).await;
                        }
                    }
                }
                Err(e) => {
                    let _ = tx
                        .send(crate::app::AsyncEvent::ChatError(format!(
                            "Connection Error: {}",
                            e
                        )))
                        .await;
                }
            }
            let _ = tx.send(crate::app::AsyncEvent::ChatFinished).await;
        });
    }
}
