use image::DynamicImage;
use reqwest::Client;
use std::error::Error;

pub async fn fetch_image(url: &str) -> Result<DynamicImage, Box<dyn Error + Send + Sync>> {
    let client = Client::new();
    let resp = client.get(url).send().await?;
    let bytes = resp.bytes().await?;
    let img = image::load_from_memory(&bytes)?;
    Ok(img)
}