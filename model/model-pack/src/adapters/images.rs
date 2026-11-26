use crate::domain::{ImageRef, MAX_IMAGE_BYTES, image_mime_type};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};

fn read_image(image: &ImageRef) -> Result<Vec<u8>> {
    let metadata = std::fs::metadata(&image.path)
        .with_context(|| format!("cannot read attached image '{}'", image.name))?;
    ensure!(
        metadata.len() <= MAX_IMAGE_BYTES as u64,
        "image exceeds 5 MiB"
    );
    let bytes = std::fs::read(&image.path)
        .with_context(|| format!("cannot read attached image '{}'", image.name))?;
    ensure!(
        image_mime_type(&bytes) == Some(image.mime_type.as_str()),
        "invalid attached image format"
    );
    Ok(bytes)
}

pub(super) fn encoded(image: &ImageRef) -> Result<String> {
    Ok(STANDARD.encode(read_image(image)?))
}

pub(super) fn data_url(image: &ImageRef) -> Result<String> {
    let bytes = read_image(image)?;
    let prefix_len = "data:".len() + image.mime_type.len() + ";base64,".len();
    let capacity = base64::encoded_len(bytes.len(), true)
        .and_then(|encoded| encoded.checked_add(prefix_len))
        .expect("integer overflow when calculating buffer size");
    let mut url = String::with_capacity(capacity);
    url.push_str("data:");
    url.push_str(&image.mime_type);
    url.push_str(";base64,");
    STANDARD.encode_string(bytes, &mut url);
    Ok(url)
}
