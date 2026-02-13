use image::DynamicImage;
use reqwest::Client;
use std::error::Error;

pub async fn fetch_image(client: &Client, url: &str) -> Result<DynamicImage, Box<dyn Error + Send + Sync>> {
    let resp = client.get(url).send().await?;
    let bytes = resp.bytes().await?;
    let img = image::load_from_memory(&bytes)?;
    Ok(img)
}