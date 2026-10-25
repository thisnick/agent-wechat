use crate::Desktop;
use std::io::{Seek, Write};
use tauri::State;
use tauri_plugin_dialog::DialogExt;

#[tauri::command]
pub async fn desktop_save_file(
    app: tauri::AppHandle,
    state: State<'_, Desktop>,
    id: String,
    name: String,
) -> Result<bool, String> {
    let mut upload = state
        .uploads
        .lock()
        .await
        .remove(&id)
        .ok_or("Download was cancelled")?;
    tokio::io::AsyncWriteExt::flush(&mut upload.file)
        .await
        .map_err(|_| "Cannot prepare the download")?;
    let name = std::path::Path::new(&name)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && name.len() <= 255)
        .unwrap_or("download")
        .to_owned();
    tauri::async_runtime::spawn_blocking(move || {
        let Some(selected) = app.dialog().file().set_file_name(name).blocking_save_file() else {
            return Ok(false);
        };
        let path = selected.into_path().map_err(|_| "Choose a local file")?;
        let parent = path.parent().ok_or("Choose a valid destination")?;
        let mut destination = tempfile::NamedTempFile::new_in(parent)
            .map_err(|_| "Cannot create the destination file")?;
        let mut source = upload
            .temp
            .reopen()
            .map_err(|_| "Cannot read the download")?;
        source.rewind().map_err(|_| "Cannot read the download")?;
        std::io::copy(&mut source, &mut destination).map_err(|_| "Cannot save the download")?;
        destination
            .as_file()
            .sync_all()
            .map_err(|_| "Cannot save the download")?;
        destination
            .persist(path)
            .map_err(|_| "Cannot replace the destination file")?;
        Ok(true)
    })
    .await
    .map_err(|_| "Cannot open the save dialog")?
}

#[tauri::command]
pub async fn desktop_export_key(
    app: tauri::AppHandle,
    state: State<'_, Desktop>,
    id: String,
) -> Result<bool, String> {
    let inner = state.inner.lock().await;
    if !inner
        .config
        .connections
        .iter()
        .any(|connection| connection.id == id)
    {
        return Err("Saved hub was not found".into());
    }
    let key = crate::credentials::read(&inner.config.installation_id, &id)
        .map_err(|error| error.to_string())?
        .ok_or("The saved access key is missing")?;
    drop(inner);
    tauri::async_runtime::spawn_blocking(move || {
        let Some(selected) = app
            .dialog()
            .file()
            .set_file_name("agent-wechat-token")
            .blocking_save_file()
        else {
            return Ok(false);
        };
        let path = selected.into_path().map_err(|_| "Choose a local file")?;
        let parent = path.parent().ok_or("Choose a valid destination")?;
        let mut file =
            tempfile::NamedTempFile::new_in(parent).map_err(|_| "Cannot create the token file")?;
        wechat_platform::protect(file.as_file()).map_err(|_| "Cannot protect the token file")?;
        writeln!(file, "{}", key.as_str()).map_err(|_| "Cannot save the token file")?;
        file.as_file()
            .sync_all()
            .map_err(|_| "Cannot save the token file")?;
        file.persist(path)
            .map_err(|_| "Cannot replace the token file")?;
        Ok(true)
    })
    .await
    .map_err(|_| "Cannot open the save dialog")?
}
