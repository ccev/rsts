use serde::Deserialize;
use std::collections::HashMap;
use std::fs;

#[derive(Deserialize, Clone)]
pub struct Config {
    pub martin_url: String,
    pub thread_count: Option<usize>,
    pub cache_size_gb: Option<u64>,
    pub tile_sources: HashMap<String, String>,
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        let path = if std::path::Path::new("rsts.toml").exists() {
            "rsts.toml"
        } else if std::path::Path::new("rsts.example.toml").exists() {
            "rsts.example.toml"
        } else {
            return Err(anyhow::anyhow!("Configuration file not found. Please create rsts.toml or provide rsts.example.toml."));
        };

        let content = fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }
}
