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
        let content = fs::read_to_string("rsts.toml")?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }
}
