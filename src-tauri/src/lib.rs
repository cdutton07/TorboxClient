use std::{fs, io::ErrorKind, path::PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::Emitter;
use tauri_plugin_opener;
use human_bytes::human_bytes;
// event emission uses `AppHandle::emit`

const TORBOX_BASE_URL: &str = "https://api.torbox.app";
const SETTINGS_DIRECTORY: &str = "TorboxClient";
const SETTINGS_FILE: &str = "settings.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub bearer_token: String,
    pub default_save_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartDownloadRequest {
    pub magnet_link: Option<String>,
    pub torrent_file_name: Option<String>,
    pub torrent_file_bytes: Option<Vec<u8>>,
    pub destination_path: Option<String>,
    pub suggested_file_name: Option<String>,
    pub allow_zip: Option<bool>,
    pub as_queued: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartLinkRequest {
    pub magnet_link: Option<String>,
    pub torrent_file_name: Option<String>,
    pub torrent_file_bytes: Option<Vec<u8>>,
    pub suggested_file_name: Option<String>,
    pub allow_zip: Option<bool>,
    pub as_queued: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadResult {
    pub torrent_id: String,
    pub output_path: String,
    pub bytes_written: u64,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkRequestResult {
    pub torrent_id: String,
    pub download_url: String,
    pub detail: String,
}

#[derive(Debug, Deserialize)]
struct TorboxEnvelope<T> {
    success: bool,
    error: Option<String>,
    detail: Option<String>,
    data: Option<T>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressPayload {
    pub bytes_downloaded: u64,
    pub total_bytes: Option<u64>,
}

fn settings_path() -> Result<PathBuf, String> {
    // Persist settings to platform-appropriate app data locations:
    // - Windows: use Local Data dir (LOCALAPPDATA)
    // - macOS: use Application Support (data_dir)
    // - Linux/other: use config dir (XDG config)
    let base_dir = if cfg!(target_os = "windows") {
        dirs::data_local_dir()
    } else if cfg!(target_os = "macos") {
        dirs::data_dir()
    } else {
        dirs::config_dir()
    }
    .ok_or_else(|| "Unable to resolve a local app config directory.".to_string())?;

    Ok(base_dir.join(SETTINGS_DIRECTORY).join(SETTINGS_FILE))
}

fn load_settings_from_disk() -> Result<AppSettings, String> {
    let path = settings_path()?;

    match fs::read_to_string(&path) {
        Ok(contents) => serde_json::from_str(&contents)
            .map_err(|error| format!("Failed to parse settings file: {error}")),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(AppSettings::default()),
        Err(error) => Err(format!("Failed to read settings file: {error}")),
    }
}

fn save_settings_to_disk(settings: &AppSettings) -> Result<(), String> {
    let path = settings_path()?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create settings directory: {error}"))?;
    }

    let serialized = serde_json::to_string_pretty(settings)
        .map_err(|error| format!("Failed to serialize settings: {error}"))?;

    fs::write(&path, serialized).map_err(|error| format!("Failed to write settings file: {error}"))
}

fn build_torbox_client(token: &str) -> Result<reqwest::Client, String> {
    if token.trim().is_empty() {
        return Err("Bearer token is missing.".to_string());
    }

    reqwest::Client::builder()
        .user_agent("TorboxClient/0.1.0")
        .build()
        .map_err(|error| format!("Failed to create HTTP client: {error}"))
}

fn resolve_output_path(destination_path: &str, suggested_file_name: Option<&str>) -> PathBuf {
    let trimmed_destination = destination_path.trim();
    let destination = PathBuf::from(trimmed_destination);

    let looks_like_directory = trimmed_destination.ends_with('\\')
        || trimmed_destination.ends_with('/')
        || destination.is_dir();

    if looks_like_directory {
        let file_name = suggested_file_name
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("torbox-download.zip");

        return destination.join(file_name);
    }

    destination
}

fn extract_torrent_id(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.trim().is_empty() => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Object(map) => {
            for key in ["torrent_id", "torrentId", "id", "torrentID"] {
                if let Some(entry) = map.get(key) {
                    if let Some(found) = extract_torrent_id(entry) {
                        return Some(found);
                    }
                }
            }

            for entry in map.values() {
                if let Some(found) = extract_torrent_id(entry) {
                    return Some(found);
                }
            }

            None
        }
        Value::Array(items) => items.iter().find_map(extract_torrent_id),
        _ => None,
    }
}

fn extract_download_link(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) if !text.trim().is_empty() => Some(text.clone()),
        Value::Object(map) => {
            for key in ["download_link", "downloadLink", "link", "url", "data"] {
                if let Some(entry) = map.get(key) {
                    if let Some(found) = extract_download_link(Some(entry)) {
                        return Some(found);
                    }
                }
            }

            for entry in map.values() {
                if let Some(found) = extract_download_link(Some(entry)) {
                    return Some(found);
                }
            }

            None
        }
        Value::Array(items) => items.iter().find_map(|entry| extract_download_link(Some(entry))),
        _ => None,
    }
}

fn torbox_error_message<T>(envelope: TorboxEnvelope<T>, fallback: &str) -> String {
    envelope
        .detail
        .or(envelope.error)
        .unwrap_or_else(|| fallback.to_string())
}

async fn download_response_to_file(
    mut response: reqwest::Response,
    output_path: &PathBuf,
    total_bytes: Option<u64>,
    app: &tauri::AppHandle,
) -> Result<u64, String> {
    let mut file = tokio::fs::File::create(output_path)
        .await
        .map_err(|error| format!("Failed to create the output file: {error}"))?;
    let mut bytes_written = 0u64;

    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("Failed while streaming the download: {error}"))?
    {
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
            .await
            .map_err(|error| format!("Failed to write to disk: {error}"))?;
        bytes_written += chunk.len() as u64;

        // Emit a structured progress update
        let _ = app.emit("download-progress", ProgressPayload {
            bytes_downloaded: bytes_written,
            total_bytes,
        });
    }

    tokio::io::AsyncWriteExt::flush(&mut file)
        .await
        .map_err(|error| format!("Failed to finalize the download file: {error}"))?;

    Ok(bytes_written)
}

async fn download_from_url(
    client: &reqwest::Client,
    url: &str,
    output_path: &PathBuf,
    app: &tauri::AppHandle,
) -> Result<u64, String> {
    let _ = app.emit("download-status", format!("Starting download from {}", url));

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| format!("Failed to fetch the direct download link: {error}"))?;

    if !response.status().is_success() {
        return Err(format!("TorBox returned an error while opening the download link ({})", response.status()));
    }

    // Attempt to parse the Content-Length header for the total file size
    let total_bytes = response.content_length();

    // Broadcast initial state immediately so the frontend knows the total size before chunking begins
    let _ = app.emit("download-progress", ProgressPayload {
        bytes_downloaded: 0,
        total_bytes,
    });

    let bytes_written = download_response_to_file(response, output_path, total_bytes, app).await?;

    let _ = app.emit("download-status", "Finished download!".to_string());

    Ok(bytes_written)
}

async fn request_download_url(
    client: &reqwest::Client,
    token: &str,
    torrent_id: &str,
    allow_zip: bool,
    app: &tauri::AppHandle,
) -> Result<String, String> {
    let response = client
        .get(format!("{TORBOX_BASE_URL}/v1/api/torrents/requestdl"))
        .bearer_auth(token)
        .query(&[
            ("token", token),
            ("torrent_id", torrent_id),
            ("file_id", "0"),
            ("zip_link", if allow_zip { "true" } else { "false" }),
            ("redirect", "false"),
            ("append_name", "true"),
        ])
        .send()
        .await
        .map_err(|error| format!("Failed to request the direct download link: {error}"))?;

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();

    if content_type.contains("application/json") {
        let envelope: TorboxEnvelope<Value> = response
            .json()
            .await
            .map_err(|error| format!("Failed to decode TorBox response: {error}"))?;

        if !envelope.success {
            return Err(torbox_error_message(envelope, "TorBox did not provide a direct download link."));
        }

        let link = extract_download_link(envelope.data.as_ref())
            .ok_or_else(|| "TorBox did not provide a direct download link.".to_string())?;

        let _ = app.emit("download-status", format!("Received download URL from TorBox."));

        return Ok(link);
    }

    Ok(response.url().to_string())
}

async fn finalize_download(
    client: &reqwest::Client,
    token: &str,
    torrent_id: &str,
    destination_path: Option<&str>,
    suggested_file_name: Option<&str>,
    allow_zip: bool,
    app: &tauri::AppHandle,
) -> Result<DownloadResult, String> {
    let settings = load_settings_from_disk()?;
    let chosen_destination = destination_path
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(settings.default_save_path.trim());

    if chosen_destination.is_empty() {
        return Err("Choose a destination path before downloading.".to_string());
    }

    let output_path = resolve_output_path(chosen_destination, suggested_file_name);

    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create destination folder: {error}"))?;
    }

    let _ = app.emit("download-status", format!("Resolved output path: {}", output_path.display()));

    let download_url = request_download_url(client, token, torrent_id, allow_zip, app).await?;
    let bytes_written = download_from_url(client, &download_url, &output_path, app).await?;

    Ok(DownloadResult {
        torrent_id: torrent_id.to_string(),
        output_path: output_path.to_string_lossy().to_string(),
        bytes_written,
        detail: format!("Saved {} to {}", human_bytes(bytes_written as f64), output_path.display()),
    })
}

#[tauri::command]
fn load_settings() -> Result<AppSettings, String> {
    load_settings_from_disk()
}

#[tauri::command]
fn save_settings(settings: AppSettings) -> Result<AppSettings, String> {
    if settings.bearer_token.trim().is_empty() {
        return Err("Bearer token is required.".to_string());
    }

    if settings.default_save_path.trim().is_empty() {
        return Err("Default save path is required.".to_string());
    }

    save_settings_to_disk(&settings)?;
    Ok(settings)
}

#[tauri::command]
async fn start_download(app: tauri::AppHandle, request: StartDownloadRequest) -> Result<DownloadResult, String> {
    let settings = load_settings_from_disk()?;

    if settings.bearer_token.trim().is_empty() {
        return Err("Save a bearer token in settings before starting a download.".to_string());
    }

    let magnet_link = request.magnet_link.as_ref().and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    });

    let torrent_file_bytes = request.torrent_file_bytes.clone();

    if magnet_link.is_none() && torrent_file_bytes.is_none() {
        return Err("Provide either a magnet link or a torrent file.".to_string());
    }

    let client = build_torbox_client(&settings.bearer_token)?;

    let _ = app.emit("download-status", "Submitting torrent to TorBox (createtorrent)");
    let mut form = reqwest::multipart::Form::new()
        .text("allow_zip", request.allow_zip.unwrap_or(true).to_string())
        .text("as_queued", request.as_queued.unwrap_or(false).to_string())
        .text("add_only_if_cached", false.to_string());

    if let Some(name) = request
        .suggested_file_name
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        form = form.text("name", name.to_string());
    }

    if let Some(magnet) = magnet_link {
        form = form.text("magnet", magnet);
    }

    if let Some(bytes) = torrent_file_bytes {
        let file_name = request
            .torrent_file_name
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "upload.torrent".to_string());

        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(file_name)
            .mime_str("application/x-bittorrent")
            .map_err(|error| format!("Failed to prepare torrent file upload: {error}"))?;

        form = form.part("file", part);
    }

    let create_response = client
        .post(format!("{TORBOX_BASE_URL}/v1/api/torrents/createtorrent"))
        .bearer_auth(&settings.bearer_token)
        .multipart(form)
        .send()
        .await
        .map_err(|error| format!("Failed to create torrent: {error}"))?;

    let create_status = create_response.status();
    let create_content_type = create_response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();

    if create_content_type.contains("application/json") {
        let envelope: TorboxEnvelope<Value> = create_response
            .json()
            .await
            .map_err(|error| format!("Failed to decode TorBox response: {error}"))?;

        if !envelope.success {
            return Err(torbox_error_message(
                envelope,
                &format!("TorBox rejected the torrent request ({create_status})."),
            ));
        }

        let torrent_id = envelope
            .data
            .as_ref()
            .and_then(extract_torrent_id)
            .ok_or_else(|| "TorBox did not return a torrent id.".to_string())?;
        let _ = app.emit("download-status", format!("TorBox created torrent id {}", torrent_id));

        return finalize_download(
            &client,
            &settings.bearer_token,
            &torrent_id,
            request.destination_path.as_deref(),
            request.suggested_file_name.as_deref(),
            request.allow_zip.unwrap_or(true),
            &app,
        )
        .await;
    }

    if !create_status.is_success() {
        return Err(format!("TorBox rejected the torrent request ({create_status})."));
    }

    let torrent_id = create_response
        .text()
        .await
        .map_err(|error| format!("Failed to read TorBox response: {error}"))?;

    let torrent_id = torrent_id.trim();
    if torrent_id.is_empty() {
        return Err("TorBox did not return a torrent id.".to_string());
    }

    let _ = app.emit("download-status", format!("TorBox created torrent id {}", torrent_id));

    finalize_download(
        &client,
        &settings.bearer_token,
        torrent_id,
        request.destination_path.as_deref(),
        request.suggested_file_name.as_deref(),
        request.allow_zip.unwrap_or(true),
        &app,
    )
    .await
}

#[tauri::command]
async fn start_link_request(app: tauri::AppHandle, request: StartLinkRequest) -> Result<LinkRequestResult, String> {
    let settings = load_settings_from_disk()?;

    if settings.bearer_token.trim().is_empty() {
        return Err("Save a bearer token in settings before starting a link request.".to_string());
    }

    let magnet_link = request.magnet_link.as_ref().and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    });

    let torrent_file_bytes = request.torrent_file_bytes.clone();

    if magnet_link.is_none() && torrent_file_bytes.is_none() {
        return Err("Provide either a magnet link or a torrent file.".to_string());
    }

    let client = build_torbox_client(&settings.bearer_token)?;

    let _ = app.emit("download-status", "Submitting torrent to TorBox (createtorrent)");
    let mut form = reqwest::multipart::Form::new()
        .text("allow_zip", request.allow_zip.unwrap_or(true).to_string())
        .text("as_queued", request.as_queued.unwrap_or(false).to_string())
        .text("add_only_if_cached", false.to_string());

    if let Some(name) = request
        .suggested_file_name
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        form = form.text("name", name.to_string());
    }

    if let Some(magnet) = magnet_link {
        form = form.text("magnet", magnet);
    }

    if let Some(bytes) = torrent_file_bytes {
        let file_name = request
            .torrent_file_name
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "upload.torrent".to_string());

        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(file_name)
            .mime_str("application/x-bittorrent")
            .map_err(|error| format!("Failed to prepare torrent file upload: {error}"))?;

        form = form.part("file", part);
    }

    let create_response = client
        .post(format!("{TORBOX_BASE_URL}/v1/api/torrents/createtorrent"))
        .bearer_auth(&settings.bearer_token)
        .multipart(form)
        .send()
        .await
        .map_err(|error| format!("Failed to create torrent: {error}"))?;

    let create_status = create_response.status();
    let create_content_type = create_response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();

    if create_content_type.contains("application/json") {
        let envelope: TorboxEnvelope<Value> = create_response
            .json()
            .await
            .map_err(|error| format!("Failed to decode TorBox response: {error}"))?;

        if !envelope.success {
            return Err(torbox_error_message(
                envelope,
                &format!("TorBox rejected the torrent request ({create_status})."),
            ));
        }

        let torrent_id = envelope
            .data
            .as_ref()
            .and_then(extract_torrent_id)
            .ok_or_else(|| "TorBox did not return a torrent id.".to_string())?;
        let _ = app.emit("download-status", format!("TorBox created torrent id {}", torrent_id));

        let download_url = request_download_url(&client, &settings.bearer_token, &torrent_id, request.allow_zip.unwrap_or(true), &app).await?;

        return Ok(LinkRequestResult {
            torrent_id: torrent_id.to_string(),
            download_url,
            detail: "Successfully created torrent and requested download URL.".to_string(),
        })
    }

    if !create_status.is_success() {
        return Err(format!("TorBox rejected the torrent request ({create_status})."));
    }

    let torrent_id = create_response
        .text()
        .await
        .map_err(|error| format!("Failed to read TorBox response: {error}"))?;

    let torrent_id = torrent_id.trim();
    if torrent_id.is_empty() {
        return Err("TorBox did not return a torrent id.".to_string());
    }

    let _ = app.emit("download-status", format!("TorBox created torrent id {}", torrent_id));

    let download_url = request_download_url(&client, &settings.bearer_token, &torrent_id, request.allow_zip.unwrap_or(true), &app).await?;

    Ok(LinkRequestResult {
        torrent_id: torrent_id.to_string(),
        download_url,
        detail: "Successfully created torrent and requested download URL.".to_string(),
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
    .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![load_settings, save_settings, start_download, start_link_request])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
