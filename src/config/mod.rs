use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_api_url", alias = "ollama_url")]
    pub api_url: String,

    #[serde(
        default = "default_ollama_port",
        skip_serializing_if = "is_default_port"
    )]
    pub ollama_port: Option<u16>,

    #[serde(default = "default_model")]
    pub default_model: String,

    #[serde(alias = "ollama_api_key")]
    pub api_key: Option<String>,

    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
}

fn is_default_port(port: &Option<u16>) -> bool {
    port.is_none()
}

fn default_ollama_port() -> Option<u16> {
    None
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub api_key: Option<String>,
}

fn default_api_url() -> String {
    "http://localhost:11434".to_string()
}

fn default_model() -> String {
    "llama3".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_url: default_api_url(),
            ollama_port: None,
            default_model: default_model(),
            api_key: None,
            mcp_servers: Vec::new(),
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let mut config = Self::load_from_file();
        // If we got ollama_port from old config, merge it with api_url
        if let Some(port) = config.ollama_port {
            if !config.api_url.contains(':') || config.api_url.starts_with("http://localhost") {
                config.api_url = format!("{}:{}", config.api_url, port);
            }
        }
        config.ollama_port = None;
        config
    }

    fn load_from_file() -> Self {
        let config_path = get_config_path();

        if config_path.exists() {
            let config_content = fs::read_to_string(&config_path).unwrap_or_else(|_| {
                eprintln!("Warning: Could not read config file, using defaults");
                String::new()
            });

            if !config_content.is_empty() {
                match toml::from_str::<Config>(&config_content) {
                    Ok(config) => return config,
                    Err(e) => {
                        eprintln!("Warning: Could not parse config file: {}", e);
                        eprintln!("Config file path: {}", config_path.display());
                    }
                }
            }
        } else {
            // Create default config file
            let default_config = Config::default();
            if let Some(parent) = config_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Err(e) = fs::write(
                config_path,
                toml::to_string_pretty(&default_config).unwrap(),
            ) {
                eprintln!("Warning: Could not create default config file: {}", e);
            }
        }

        Config::default()
    }
}

fn get_config_path() -> PathBuf {
    if let Some(project_dirs) = directories::ProjectDirs::from("com", "chatatui", "chatatui") {
        project_dirs.config_dir().join("config.toml")
    } else {
        PathBuf::from("config.toml")
    }
}
