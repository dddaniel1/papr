//! IPC orchestration for manual third-party article archives.
use crate::{
    error::{AppError, AppResult},
    state::AppState,
};
use papr_core::{
    archive::{self, ArchiveResult, ArchiveSettings, Notion, NotionConfig, Service, TargetType},
    db,
};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

#[tauri::command]
pub async fn get_archive_settings(state: State<'_, AppState>) -> AppResult<ArchiveSettings> {
    archive::settings(&*state.read().await)
}

#[tauri::command]
pub async fn choose_obsidian_vault(app: AppHandle) -> AppResult<Option<String>> {
    let (send, receive) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("Choose Obsidian vault")
        .pick_folder(move |path| {
            let _ = send.send(path);
        });
    let path = receive
        .await
        .map_err(|_| AppError::other("The vault directory picker closed unexpectedly."))?;
    path.map(|p| {
        p.into_path()
            .map(|p| p.to_string_lossy().into_owned())
            .map_err(|_| AppError::other("Choose a local vault directory."))
    })
    .transpose()
}

#[tauri::command]
pub async fn validate_obsidian(vault_path: String, folder: String) -> AppResult<()> {
    tokio::task::spawn_blocking(move || {
        archive::obsidian_directory(&vault_path, &folder, false).map(|_| ())
    })
    .await
    .map_err(|_| AppError::other("Could not validate the vault directory."))?
}

#[tauri::command]
pub async fn configure_obsidian(
    state: State<'_, AppState>,
    vault_path: String,
    folder: String,
) -> AppResult<ArchiveSettings> {
    let _archive = state.archive_lock.lock().await;
    archive::configure_obsidian(&*state.db.lock().await, &vault_path, &folder)
}

async fn notion_draft(
    state: &AppState,
    token: Option<&str>,
    target_type: TargetType,
    target_id: &str,
) -> AppResult<Notion> {
    let config = NotionConfig::load(&*state.read().await, token, target_type, target_id)?;
    Notion::new(config)
}

#[tauri::command]
pub async fn validate_notion(
    state: State<'_, AppState>,
    token: Option<String>,
    target_type: TargetType,
    target_id: String,
) -> AppResult<()> {
    let _archive = state.archive_lock.lock().await;
    notion_draft(&state, token.as_deref(), target_type, &target_id)
        .await?
        .validate()
        .await
        .map(|_| ())
}

#[tauri::command]
pub async fn configure_notion(
    state: State<'_, AppState>,
    token: Option<String>,
    target_type: TargetType,
    target_id: String,
) -> AppResult<ArchiveSettings> {
    let _archive = state.archive_lock.lock().await;
    let notion = notion_draft(&state, token.as_deref(), target_type, &target_id).await?;
    notion.validate().await?;
    notion.save_config(&*state.db.lock().await)
}

#[tauri::command]
pub async fn disconnect_archive_service(
    state: State<'_, AppState>,
    service: Service,
) -> AppResult<ArchiveSettings> {
    let _archive = state.archive_lock.lock().await;
    archive::disconnect(&*state.db.lock().await, service)
}

#[tauri::command]
pub async fn archive_article(
    state: State<'_, AppState>,
    article_id: i64,
    service: Service,
) -> AppResult<ArchiveResult> {
    let _archive = state.archive_lock.lock().await;
    let (article, highlights, settings) = {
        let conn = state.read().await;
        (
            db::get_article(&conn, article_id)?,
            db::list_highlights(&conn, article_id)?,
            archive::settings(&conn)?,
        )
    };
    let location = match service {
        Service::Obsidian => {
            if !settings.obsidian.configured {
                return Err(AppError::other(
                    "Configure an Obsidian vault before archiving.",
                ));
            }
            let vault = settings.obsidian.vault_path;
            let folder = settings.obsidian.folder;
            let destination = archive::obsidian_destination(&vault, &folder)?;
            let previous = Some(db::reserve_obsidian_archive(
                &*state.db.lock().await,
                article_id,
                &destination,
                &archive::obsidian_filename(&article),
            )?);
            let location = tokio::task::spawn_blocking(move || {
                archive::write_obsidian(&vault, &folder, &article, &highlights, previous.as_deref())
            })
            .await
            .map_err(|_| AppError::other("The Obsidian export task could not finish."))??;
            let filename = std::path::Path::new(&location)
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| AppError::other("Invalid archive filename."))?;
            db::save_archive_location(
                &*state.db.lock().await,
                article_id,
                "obsidian",
                &destination,
                filename,
            )?;
            location
        }
        Service::Notion => {
            if !settings.notion.configured {
                return Err(AppError::other(
                    "Configure a Notion destination before archiving.",
                ));
            }
            let config = NotionConfig::stored(&*state.read().await)?;
            let notion = Notion::new(config)?;
            let target = notion.validate().await?;
            let reserved = db::reserve_notion_archive(
                &*state.db.lock().await,
                article_id,
                &target.destination,
            )?;
            let page_id = notion.ensure_page(&target, &reserved).await?;
            // Persist before body upload: all subsequent retries overwrite this same page.
            db::save_archive_location(
                &*state.db.lock().await,
                article_id,
                "notion",
                &target.destination,
                &page_id,
            )?;
            notion
                .export(&target, &page_id, &article, &highlights)
                .await?
        }
    };
    Ok(ArchiveResult { location })
}
