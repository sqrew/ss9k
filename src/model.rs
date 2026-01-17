//! Model download and path management for SS9K
//!
//! Handles downloading Whisper models from HuggingFace and
//! finding model files across multiple locations.

use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

/// Download a model from HuggingFace with progress bar
pub fn download_model(url: &str, dest: &PathBuf) -> Result<()> {
    println!("[SS9K] Downloading model from: {}", url);

    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }

    let response = reqwest::blocking::get(url)?;

    if !response.status().is_success() {
        anyhow::bail!("Download failed: HTTP {}", response.status());
    }

    let total_size = response.content_length().unwrap_or(0);

    let pb = ProgressBar::new(total_size);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[SS9K] {bar:40.cyan/blue} {bytes}/{total_bytes} ({eta})")?
            .progress_chars("##-"),
    );

    let mut file = File::create(dest)?;
    let mut downloaded: u64 = 0;
    let content = response.bytes()?;

    for chunk in content.chunks(8192) {
        file.write_all(chunk)?;
        downloaded += chunk.len() as u64;
        pb.set_position(downloaded);
    }

    pb.finish_with_message("Download complete!");
    println!("[SS9K] Model saved to: {:?}", dest);

    Ok(())
}

/// Get the preferred model install location
pub fn get_model_install_path(model_name: &str) -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ss9k")
        .join("models")
        .join(model_name)
}

/// Get the model path, checking multiple locations
pub fn get_model_path(model_name: &str) -> PathBuf {
    let candidates = [
        // 1. Current directory (for development)
        PathBuf::from("models").join(model_name),
        // 2. XDG data dir (Linux: ~/.local/share/ss9k)
        dirs::data_dir()
            .map(|p| p.join("ss9k").join("models").join(model_name))
            .unwrap_or_default(),
        // 3. Home dir fallback
        dirs::home_dir()
            .map(|p| p.join(".ss9k").join("models").join(model_name))
            .unwrap_or_default(),
    ];

    for path in candidates {
        if path.exists() {
            return path;
        }
    }

    // Return the preferred install location if none exist (for error message)
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ss9k")
        .join("models")
        .join(model_name)
}
