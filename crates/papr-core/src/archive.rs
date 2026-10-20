//! Manual, one-way article archives. Credentials stay in local SQLite settings.
use crate::{
    db,
    error::{AppError, AppResult},
    models::{ArticleDetail, Highlight},
};
use ego_tree::iter::Edge;
use reqwest::{Client, Method};
use rusqlite::Connection;
use scraper::{Html, Node};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::Write,
    path::{Component, Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Service {
    Obsidian,
    Notion,
}
#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TargetType {
    Page,
    Database,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObsidianSettings {
    pub vault_path: String,
    pub folder: String,
    pub configured: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotionSettings {
    pub configured: bool,
    pub target_type: TargetType,
    pub target_id: String,
    pub token_configured: bool,
}
#[derive(Serialize)]
pub struct ArchiveSettings {
    pub obsidian: ObsidianSettings,
    pub notion: NotionSettings,
}
#[derive(Serialize)]
pub struct ArchiveResult {
    pub location: String,
}

fn setting(conn: &Connection, key: &str) -> AppResult<String> {
    Ok(db::get_setting(conn, key)?.unwrap_or_default())
}
pub fn settings(conn: &Connection) -> AppResult<ArchiveSettings> {
    let vault_path = setting(conn, "archive_obsidian_vault")?;
    let target_id = setting(conn, "archive_notion_target")?;
    let token_configured = !setting(conn, "archive_notion_token")?.is_empty();
    Ok(ArchiveSettings {
        obsidian: ObsidianSettings {
            configured: !vault_path.is_empty(),
            vault_path,
            folder: setting(conn, "archive_obsidian_folder")?,
        },
        notion: NotionSettings {
            configured: token_configured && !target_id.is_empty(),
            token_configured,
            target_id,
            target_type: if setting(conn, "archive_notion_type")? == "database" {
                TargetType::Database
            } else {
                TargetType::Page
            },
        },
    })
}
pub fn disconnect(conn: &Connection, service: Service) -> AppResult<ArchiveSettings> {
    let tx = conn.unchecked_transaction()?;
    let keys: &[&str] = match service {
        Service::Obsidian => &["archive_obsidian_vault", "archive_obsidian_folder"],
        Service::Notion => &[
            "archive_notion_token",
            "archive_notion_type",
            "archive_notion_target",
        ],
    };
    for key in keys {
        db::set_setting(&tx, key, "")?;
    }
    tx.commit()?;
    settings(conn)
}
fn io_error(e: std::io::Error) -> AppError {
    AppError::other(format!("Obsidian file operation failed: {e}"))
}

/// Canonicalize each existing component before creating anything. Never traverse a symlink outside the vault.
pub fn obsidian_directory(vault: &str, folder: &str, create: bool) -> AppResult<PathBuf> {
    let vault = Path::new(vault);
    if !vault.is_absolute() || !vault.is_dir() {
        return Err(AppError::other(
            "Choose an absolute path to an existing Obsidian vault directory.",
        ));
    }
    let root = vault.canonicalize().map_err(io_error)?;
    let relative = Path::new(folder);
    if relative
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
        || folder.contains('\\')
    {
        return Err(AppError::other("The archive folder must be relative to the vault, without parent-directory components."));
    }
    let mut path = root.clone();
    for part in relative.components() {
        if part == Component::CurDir {
            continue;
        }
        path.push(part);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                let actual = path.canonicalize().map_err(io_error)?;
                if !actual.starts_with(&root) || !actual.is_dir() {
                    return Err(AppError::other(
                        "The archive folder must stay inside the vault, including symbolic links.",
                    ));
                }
                path = actual;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if create {
                    std::fs::create_dir(&path).map_err(io_error)?;
                }
            }
            Err(e) => return Err(io_error(e)),
        }
    }
    Ok(path)
}
pub fn configure_obsidian(
    conn: &Connection,
    vault: &str,
    folder: &str,
) -> AppResult<ArchiveSettings> {
    obsidian_directory(vault, folder, false)?;
    let directory = obsidian_directory(vault, folder, true)?;
    tempfile::NamedTempFile::new_in(&directory).map_err(io_error)?;
    let canonical = Path::new(vault).canonicalize().map_err(io_error)?;
    let tx = conn.unchecked_transaction()?;
    db::set_setting(&tx, "archive_obsidian_vault", &canonical.to_string_lossy())?;
    db::set_setting(&tx, "archive_obsidian_folder", folder)?;
    tx.commit()?;
    settings(conn)
}
pub fn obsidian_destination(vault: &str, folder: &str) -> AppResult<String> {
    Ok(obsidian_directory(vault, folder, false)?
        .to_string_lossy()
        .into_owned())
}
pub fn write_obsidian(
    vault: &str,
    folder: &str,
    article: &ArticleDetail,
    highlights: &[Highlight],
    previous: Option<&str>,
) -> AppResult<String> {
    let directory = obsidian_directory(vault, folder, true)?;
    let filename = previous
        .map(str::to_owned)
        .unwrap_or_else(|| format!("papr-article-{}.md", article.id));
    if Path::new(&filename).components().count() != 1
        || !matches!(
            Path::new(&filename).components().next(),
            Some(Component::Normal(_))
        )
    {
        return Err(AppError::other("Invalid saved archive filename."));
    }
    let target = directory.join(&filename);
    if let Ok(meta) = std::fs::symlink_metadata(&target) {
        if meta.file_type().is_symlink() || !meta.is_file() {
            return Err(AppError::other(
                "The article archive path is a symbolic link or is not a regular file.",
            ));
        }
    }
    let mut temp = tempfile::NamedTempFile::new_in(&directory).map_err(io_error)?;
    temp.write_all(markdown(article, highlights).as_bytes())
        .map_err(io_error)?;
    temp.as_file().sync_all().map_err(io_error)?;
    // Revalidate the directory immediately before the atomic replacement.
    if obsidian_directory(vault, folder, false)? != directory {
        return Err(AppError::other(
            "The vault directory changed during the export. Retry after checking its path.",
        ));
    }
    if previous.is_some() {
        temp.persist(&target).map_err(|e| io_error(e.error))?;
    } else {
        temp.persist_noclobber(&target)
            .map_err(|e| io_error(e.error))?;
    }
    Ok(target.to_string_lossy().into_owned())
}

#[derive(Debug, PartialEq)]
enum Part {
    Text(String),
    Image(String, String),
    Code(String),
    Table(Vec<Vec<String>>),
}
fn safe_url(raw: &str, base: Option<&str>) -> Option<String> {
    let url = url::Url::parse(raw)
        .or_else(|_| url::Url::parse(base.unwrap_or("")).and_then(|b| b.join(raw)))
        .ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.to_string())
}
fn article_parts(article: &ArticleDetail) -> Vec<Part> {
    let html = article
        .extracted_html
        .as_deref()
        .filter(|h| !h.trim().is_empty())
        .or(article.content_html.as_deref())
        .unwrap_or("");
    let fragment = Html::parse_fragment(html);
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut skip = 0usize;
    let mut pre = 0usize;
    let mut table_depth = 0usize;
    let mut inline_code = 0usize;
    let mut lists: Vec<usize> = Vec::new();
    let flush = |text: &mut String, parts: &mut Vec<Part>| {
        if !text.trim().is_empty() {
            parts.push(Part::Text(text.trim().to_string()));
        }
        text.clear();
    };
    for edge in fragment.tree.root().traverse() {
        match edge {
            Edge::Open(node) => match node.value() {
                Node::Element(el) => {
                    let name = el.name();
                    if matches!(name, "script" | "style" | "noscript" | "template") {
                        skip += 1;
                    }
                    if skip != 0 {
                        continue;
                    }
                    if name == "table" {
                        if table_depth == 0 && pre == 0 {
                            flush(&mut text, &mut parts);
                            if let Some(table) = scraper::ElementRef::wrap(node) {
                                let rows = scraper::Selector::parse("tr").unwrap();
                                let cells = scraper::Selector::parse("th, td").unwrap();
                                parts.push(Part::Table(
                                    table
                                        .select(&rows)
                                        .map(|row| {
                                            row.select(&cells)
                                                .map(|cell| {
                                                    crate::sanitize::html_to_text(&cell.html())
                                                })
                                                .collect()
                                        })
                                        .collect(),
                                ));
                            }
                        }
                        table_depth += 1;
                    }
                    if table_depth != 0 {
                        continue;
                    }
                    if name == "pre" {
                        if pre == 0 {
                            flush(&mut text, &mut parts);
                            if let Some(el) = scraper::ElementRef::wrap(node) {
                                parts.push(Part::Code(el.text().collect()));
                            }
                        }
                        pre += 1;
                    }
                    if pre != 0 {
                        continue;
                    }
                    match name {
                        "img" => {
                            if let Some(src) = el
                                .attr("src")
                                .and_then(|s| safe_url(s, article.url.as_deref()))
                            {
                                flush(&mut text, &mut parts);
                                parts.push(Part::Image(
                                    src,
                                    el.attr("alt").unwrap_or("").to_string(),
                                ));
                            }
                        }
                        "p" | "div" | "section" | "article" | "tr" | "pre" => {
                            text.push_str("\n\n");
                        }
                        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                            text.push_str("\n\n");
                            text.push_str(&"#".repeat(name[1..].parse::<usize>().unwrap_or(2)));
                            text.push(' ');
                        }
                        "br" => text.push_str("  \n"),
                        "ul" => {
                            lists.push(0);
                            text.push('\n');
                        }
                        "ol" => {
                            lists.push(el.attr("start").and_then(|s| s.parse().ok()).unwrap_or(1));
                            text.push('\n');
                        }
                        "li" => {
                            text.push('\n');
                            text.push_str(&"  ".repeat(lists.len().saturating_sub(1)));
                            match lists.last_mut() {
                                Some(n) if *n > 0 => {
                                    text.push_str(&format!("{}. ", n));
                                    *n += 1;
                                }
                                _ => text.push_str("- "),
                            }
                        }
                        "blockquote" => text.push_str("\n\n> "),
                        "strong" | "b" => text.push_str("**"),
                        "em" | "i" => text.push('*'),
                        "code" => {
                            inline_code = 1 - inline_code;
                            text.push('`');
                        }
                        "td" | "th" => text.push_str(" | "),
                        "a" if el
                            .attr("href")
                            .and_then(|u| safe_url(u, article.url.as_deref()))
                            .is_some() =>
                        {
                            text.push('[')
                        }
                        _ => {}
                    }
                }
                Node::Text(t) if skip == 0 && pre == 0 && table_depth == 0 => {
                    if inline_code > 0 {
                        text.push_str(t);
                    } else {
                        text.push_str(&escape_markdown(t));
                    }
                }
                _ => {}
            },
            Edge::Close(node) => {
                if let Node::Element(el) = node.value() {
                    let name = el.name();
                    if matches!(name, "script" | "style" | "noscript" | "template") {
                        skip = skip.saturating_sub(1);
                        continue;
                    }
                    if skip != 0 {
                        continue;
                    }
                    if name == "table" {
                        table_depth = table_depth.saturating_sub(1);
                        continue;
                    }
                    if table_depth != 0 {
                        continue;
                    }
                    if name == "pre" {
                        pre = pre.saturating_sub(1);
                        continue;
                    }
                    if pre != 0 {
                        continue;
                    }
                    match name {
                        "ul" | "ol" => {
                            lists.pop();
                            text.push('\n');
                        }
                        "p" | "div" | "section" | "article" | "tr" | "pre" | "h1" | "h2" | "h3"
                        | "h4" | "h5" | "h6" | "blockquote" => text.push_str("\n\n"),
                        "strong" | "b" => text.push_str("**"),
                        "em" | "i" => text.push('*'),
                        "code" => {
                            inline_code = 1 - inline_code;
                            text.push('`');
                        }
                        "a" => {
                            if let Some(href) = el
                                .attr("href")
                                .and_then(|u| safe_url(u, article.url.as_deref()))
                            {
                                text.push_str(&format!("](<{href}>)"));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    flush(&mut text, &mut parts);
    parts
}
fn ordered_highlights(highlights: &[Highlight]) -> Vec<&Highlight> {
    let mut ordered: Vec<_> = highlights.iter().collect();
    ordered.sort_by_key(|h| (h.text_offset, h.id));
    ordered
}
fn escape_markdown(text: &str) -> String {
    let mut escaped = String::new();
    for c in text.chars() {
        if "\\`*_{}[]<>#+-.!|>".contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}
fn metadata(article: &ArticleDetail) -> String {
    let mut text = format!("Feed: {}\n", article.feed_title);
    if let Some(author) = &article.author {
        text.push_str(&format!("Author: {author}\n"));
    }
    if let Some(published) = &article.published_at {
        text.push_str(&format!("Published: {published}\n"));
    }
    if let Some(url) = &article.url {
        text.push_str(&format!("Source: {url}\n"));
    }
    text.push_str(&format!("Papr article: {}\n", article.id));
    text
}
pub fn markdown(article: &ArticleDetail, highlights: &[Highlight]) -> String {
    let mut out = format!(
        "# {}\n\n{}\n",
        escape_markdown(&article.title),
        escape_markdown(&metadata(article))
    );
    for part in article_parts(article) {
        match part {
            Part::Text(t) => out.push_str(&t),
            Part::Table(rows) => {
                let width = rows.iter().map(Vec::len).max().unwrap_or(0);
                for (index, row) in rows.iter().enumerate() {
                    out.push('|');
                    for col in 0..width {
                        out.push_str(&format!(
                            " {} |",
                            row.get(col)
                                .map(String::as_str)
                                .unwrap_or("")
                                .replace('|', "\\|")
                        ));
                    }
                    out.push('\n');
                    if index == 0 {
                        out.push_str(&format!("|{}\n", " --- |".repeat(width)));
                    }
                }
            }
            Part::Code(t) => {
                let fence = "`".repeat(
                    t.split(|c| c != '`')
                        .map(str::len)
                        .max()
                        .unwrap_or(0)
                        .max(2)
                        + 1,
                );
                out.push_str(&format!("{fence}\n{t}\n{fence}"));
            }
            Part::Image(url, alt) => {
                out.push_str(&format!("![{}](<{url}>)", alt.replace(['[', ']'], "")))
            }
        }
        out.push_str("\n\n");
    }
    if !highlights.is_empty() {
        out.push_str("## Highlights and notes\n\n");
    }
    for h in ordered_highlights(highlights) {
        for line in h.quote.lines() {
            out.push_str(&format!("> {}\n", escape_markdown(line)));
        }
        if !h.note.is_empty() {
            out.push_str(&format!("\n{}\n", escape_markdown(&h.note)));
        }
        out.push('\n');
    }
    out
}

pub struct NotionConfig {
    token: String,
    pub target_type: TargetType,
    pub target_id: String,
}
impl NotionConfig {
    pub fn load(
        conn: &Connection,
        token: Option<&str>,
        target_type: TargetType,
        target: &str,
    ) -> AppResult<Self> {
        let token = token
            .filter(|t| !t.trim().is_empty())
            .map(|s| s.trim().to_string())
            .unwrap_or(setting(conn, "archive_notion_token")?);
        if token.is_empty() {
            return Err(AppError::other("Enter a Notion integration token."));
        }
        Ok(Self {
            token,
            target_type,
            target_id: notion_id(target)?,
        })
    }
    pub fn stored(conn: &Connection) -> AppResult<Self> {
        let status = settings(conn)?.notion;
        Self::load(conn, None, status.target_type, &status.target_id)
    }
    pub fn save(&self, conn: &Connection) -> AppResult<ArchiveSettings> {
        let tx = conn.unchecked_transaction()?;
        db::set_setting(&tx, "archive_notion_token", &self.token)?;
        db::set_setting(
            &tx,
            "archive_notion_type",
            if self.target_type == TargetType::Page {
                "page"
            } else {
                "database"
            },
        )?;
        db::set_setting(&tx, "archive_notion_target", &self.target_id)?;
        tx.commit()?;
        settings(conn)
    }
}
pub fn notion_id(input: &str) -> AppResult<String> {
    let raw = input.trim();
    let candidate = if raw.starts_with("https://") {
        let url = url::Url::parse(raw)
            .map_err(|_| AppError::other("Enter a valid Notion page or database URL."))?;
        let host = url.host_str().unwrap_or("");
        if !(host == "notion.so"
            || host.ends_with(".notion.so")
            || host == "notion.site"
            || host.ends_with(".notion.site"))
        {
            return Err(AppError::other("Use a Notion URL or a page/database ID."));
        }
        url.path_segments()
            .and_then(|mut s| s.next_back())
            .unwrap_or("")
            .to_string()
    } else {
        raw.to_string()
    };
    let compact = candidate.replace('-', "");
    let id = compact
        .get(compact.len().saturating_sub(32)..)
        .unwrap_or("");
    if id.len() != 32
        || !id.bytes().all(|c| c.is_ascii_hexdigit())
        || (raw == candidate && candidate.len() != 32 && candidate.len() != 36)
    {
        return Err(AppError::other(
            "Enter a valid Notion page or database ID (32 hexadecimal characters), or its URL.",
        ));
    }
    let id = id.to_ascii_lowercase();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &id[..8],
        &id[8..12],
        &id[12..16],
        &id[16..20],
        &id[20..]
    ))
}

pub struct Notion {
    #[cfg(test)]
    base_url: Option<String>,
    client: Client,
    config: NotionConfig,
}
pub struct NotionTarget {
    pub destination: String,
    parent: Value,
    properties: Value,
}
impl Notion {
    pub fn new(config: NotionConfig) -> AppResult<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(45))
            .connect_timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| AppError::other("Could not initialize the Notion connection."))?;
        Ok(Self {
            client,
            config,
            #[cfg(test)]
            base_url: None,
        })
    }
    async fn request(&self, method: Method, path: &str, body: Option<Value>) -> AppResult<Value> {
        tokio::time::sleep(Duration::from_millis(350)).await;
        let base = "https://api.notion.com/v1";
        #[cfg(test)]
        let base = self.base_url.as_deref().unwrap_or(base);
        let mut request = self
            .client
            .request(method, format!("{base}/{path}"))
            .bearer_auth(&self.config.token)
            .header("Notion-Version", "2026-03-11");
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|e| AppError::other(if e.is_timeout() {
            "Notion request timed out. Retry the archive; an existing destination will be reused."
        } else { "Could not reach Notion. Check your connection and retry." }))?;
        let status = response.status();
        if !status.is_success() {
            // Never surface remote messages or request debug output: either may contain credentials.
            let message = match status.as_u16() {
                401 => "Notion rejected the integration token. Reconnect with a valid token.",
                403 => "Notion denied access. Grant the integration read, insert and update content permissions.",
                404 => "Notion could not find the destination. Share the original page/database with your integration and check its ID.",
                429 => "Notion rate limit reached. Wait a moment and retry the archive.",
                400 => "Notion rejected the exported content or destination schema. Check the target properties and supported content limits.",
                _ => "Notion could not complete the request. Retry the archive after checking the service status.",
            };
            return Err(AppError::other(message));
        }
        response.json().await.map_err(|_| {
            AppError::other("Notion returned an unreadable response. Retry the archive.")
        })
    }
    pub async fn validate(&self) -> AppResult<NotionTarget> {
        let id = &self.config.target_id;
        if self.config.target_type == TargetType::Page {
            let page = self
                .request(Method::GET, &format!("pages/{id}"), None)
                .await?;
            if page["archived"] == true || page["in_trash"] == true {
                return Err(AppError::other(
                    "The Notion target page is in the trash. Restore it or choose another page.",
                ));
            }
            return Ok(NotionTarget {
                destination: format!("page:{id}"),
                parent: json!({"type":"page_id","page_id":id}),
                properties: json!({"title":{"type":"title"}}),
            });
        }
        let database = self
            .request(Method::GET, &format!("databases/{id}"), None)
            .await?;
        let sources = database["data_sources"].as_array().ok_or_else(|| {
            AppError::other("Notion did not return a data source for this database.")
        })?;
        if sources.len() != 1 {
            return Err(AppError::other("Choose a Notion database with exactly one data source; multiple data sources are ambiguous."));
        }
        let source_id = sources[0]["id"]
            .as_str()
            .ok_or_else(|| AppError::other("Notion returned an invalid data source."))?;
        let schema = self
            .request(Method::GET, &format!("data_sources/{source_id}"), None)
            .await?;
        let properties = schema["properties"].clone();
        title_property(&properties)?;
        Ok(NotionTarget {
            destination: format!("data_source:{source_id}"),
            parent: json!({"type":"data_source_id","data_source_id":source_id}),
            properties,
        })
    }
    pub fn save_config(&self, conn: &Connection) -> AppResult<ArchiveSettings> {
        self.config.save(conn)
    }
    async fn children(&self, id: &str) -> AppResult<Vec<Value>> {
        let mut all = Vec::new();
        let mut cursor = None::<String>;
        loop {
            let mut path = format!("blocks/{id}/children?page_size=100");
            if let Some(c) = &cursor {
                path.push_str("&start_cursor=");
                path.push_str(
                    &url::form_urlencoded::byte_serialize(c.as_bytes()).collect::<String>(),
                );
            }
            let page = self.request(Method::GET, &path, None).await?;
            let results = page["results"]
                .as_array()
                .ok_or_else(|| AppError::other("Notion returned an invalid block list."))?;
            all.extend(results.iter().cloned());
            if page["has_more"] != true {
                break;
            }
            let next = page["next_cursor"]
                .as_str()
                .ok_or_else(|| AppError::other("Notion omitted its next page cursor."))?
                .to_string();
            if cursor.as_ref() == Some(&next) {
                return Err(AppError::other("Notion repeated its pagination cursor."));
            }
            cursor = Some(next);
        }
        Ok(all)
    }
    /// Recover a create whose response (or local mapping write) was lost, using a persisted unique draft title.
    pub async fn ensure_page(&self, target: &NotionTarget, location: &str) -> AppResult<String> {
        let Some(marker) = location.strip_prefix("pending:") else {
            return Ok(location.to_string());
        };
        let existing = if self.config.target_type == TargetType::Page {
            self.children(&self.config.target_id)
                .await?
                .into_iter()
                .find(|b| b["child_page"]["title"].as_str() == Some(marker))
        } else {
            let source = target.parent["data_source_id"].as_str().unwrap();
            let title = title_property(&target.properties)?;
            let result = self.request(Method::POST, &format!("data_sources/{source}/query"), Some(json!({"filter":{"property":title,"title":{"equals":marker}},"page_size":1}))).await?;
            result["results"]
                .as_array()
                .and_then(|r| r.first())
                .cloned()
        };
        let page = match existing {
            Some(p) => p,
            None => {
                let title = title_property(&target.properties)?;
                self.request(Method::POST, "pages", Some(json!({"parent":target.parent,"properties":{title:{"title":rich_text(marker)}}}))).await?
            }
        };
        page["id"].as_str().map(str::to_string).ok_or_else(|| {
            AppError::other(
                "Notion did not return the archive page ID. Retry to recover the draft.",
            )
        })
    }
    pub async fn export(
        &self,
        target: &NotionTarget,
        page_id: &str,
        article: &ArticleDetail,
        highlights: &[Highlight],
    ) -> AppResult<String> {
        let old = self.children(page_id).await?;
        let blocks = notion_blocks(article, highlights);
        // Bound both the 100-child limit and the 500KB request-body limit.
        for batch in block_batches(&blocks) {
            self.request(
                Method::PATCH,
                &format!("blocks/{page_id}/children"),
                Some(json!({"children":batch})),
            )
            .await?;
        }
        self.request(
            Method::PATCH,
            &format!("pages/{page_id}"),
            Some(json!({"properties":page_properties(&target.properties, article)?})),
        )
        .await?;
        for block in old {
            let id = block["id"].as_str().ok_or_else(|| {
                AppError::other("Notion returned a block without an ID. Retry the archive.")
            })?;
            self.request(Method::DELETE, &format!("blocks/{id}"), None)
                .await?;
        }
        Ok(format!(
            "https://www.notion.so/{}",
            page_id.replace('-', "")
        ))
    }
}
fn title_property(properties: &Value) -> AppResult<&str> {
    properties
        .as_object()
        .and_then(|p| {
            p.iter()
                .find(|(_, v)| v["type"] == "title")
                .map(|(k, _)| k.as_str())
        })
        .ok_or_else(|| AppError::other("The Notion data source needs a title property."))
}
fn page_properties(schema: &Value, article: &ArticleDetail) -> AppResult<Value> {
    let title = title_property(schema)?;
    let mut props = json!({title:{"title":rich_text(&article.title)}});
    for (name, definition) in schema.as_object().into_iter().flatten() {
        let normalized = name.to_lowercase().replace([' ', '_', '-'], "");
        match (normalized.as_str(), definition["type"].as_str()) {
            ("source" | "sourceurl" | "url" | "link", Some("url")) => {
                props[name] = json!({"url":article.url.as_deref().and_then(|u| safe_url(u,None)).filter(|u|u.len()<=2000)});
            }
            ("feed" | "feedtitle", Some("rich_text")) => {
                props[name] = json!({"rich_text":rich_text(&article.feed_title)});
            }
            ("published" | "publishedat" | "publisheddate" | "date", Some("date")) => {
                let date = article
                    .published_at
                    .as_deref()
                    .and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok())
                    .map(|d| d.to_rfc3339());
                props[name] = json!({"date":date.map(|d|json!({"start":d}))});
            }
            _ => {}
        }
    }
    Ok(props)
}
fn split_text(text: &str, limit: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut count = 0;
    for c in text.chars() {
        let size = c.len_utf16();
        if count + size > limit {
            chunks.push(std::mem::take(&mut current));
            count = 0;
        }
        current.push(c);
        count += size;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}
fn rich_text(text: &str) -> Vec<Value> {
    split_text(text, 2000)
        .into_iter()
        .map(|s| json!({"type":"text","text":{"content":s}}))
        .collect()
}
fn text_blocks(kind: &str, text: &str, blocks: &mut Vec<Value>) {
    // One rich-text item per block also bounds payload size and preserves arbitrarily long quotes/notes.
    for chunk in split_text(text, 2000) {
        blocks.push(json!({"object":"block","type":kind,kind:{"rich_text":rich_text(&chunk)}}));
    }
}
fn markdown_text_blocks(kind: &str, text: &str, blocks: &mut Vec<Value>) {
    use pulldown_cmark::{Event, Parser, Tag, TagEnd};
    let mut rich = Vec::new();
    let (mut bold, mut italic, mut strike) = (false, false, false);
    let mut link: Option<String> = None;
    for event in Parser::new(text) {
        let content = match event {
            Event::Start(Tag::Strong) => {
                bold = true;
                continue;
            }
            Event::End(TagEnd::Strong) => {
                bold = false;
                continue;
            }
            Event::Start(Tag::Emphasis) => {
                italic = true;
                continue;
            }
            Event::End(TagEnd::Emphasis) => {
                italic = false;
                continue;
            }
            Event::Start(Tag::Strikethrough) => {
                strike = true;
                continue;
            }
            Event::End(TagEnd::Strikethrough) => {
                strike = false;
                continue;
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                link = safe_url(&dest_url, None).filter(|s| s.len() <= 2000);
                continue;
            }
            Event::End(TagEnd::Link) => {
                link = None;
                continue;
            }
            Event::Text(t) => (t.to_string(), false),
            Event::Code(t) => (t.to_string(), true),
            Event::SoftBreak | Event::HardBreak => ("\n".to_string(), false),
            Event::InlineHtml(t) | Event::Html(t) => (t.to_string(), false),
            _ => continue,
        };
        for chunk in split_text(&content.0, 2000) {
            rich.push(json!({"type":"text","text":{"content":chunk,"link":link.as_ref().map(|u|json!({"url":u}))},
                "annotations":{"bold":bold,"italic":italic,"strikethrough":strike,"code":content.1}}));
        }
    }
    // Keep each block comfortably below the request payload limit, including multibyte text.
    let mut start = 0;
    let mut bytes = 0;
    for i in 0..rich.len() {
        let size = rich[i].to_string().len();
        if i > start && (i - start >= 100 || bytes + size > 200_000) {
            blocks.push(json!({"object":"block","type":kind,kind:{"rich_text":&rich[start..i]}}));
            start = i;
            bytes = 0;
        }
        bytes += size;
    }
    if start < rich.len() {
        blocks.push(json!({"object":"block","type":kind,kind:{"rich_text":&rich[start..]}}));
    }
}
fn notion_blocks(article: &ArticleDetail, highlights: &[Highlight]) -> Vec<Value> {
    let mut blocks = Vec::new();
    text_blocks("paragraph", &metadata(article), &mut blocks);
    for part in article_parts(article) {
        match part {
            Part::Table(rows) => {
                let width = rows.iter().map(Vec::len).max().unwrap_or(0);
                // Wider/very large tables retain all text as rows without violating Notion's nested block limits.
                if width == 0 { continue; }
                if width > 100 || rows.iter().flatten().any(|c| c.encode_utf16().count() > 2000) {
                    for row in rows { text_blocks("paragraph", &row.join(" | "), &mut blocks); }
                } else {
                    for batch in rows.chunks(50) {
                        let children: Vec<_> = batch.iter().map(|row| json!({"object":"block","type":"table_row","table_row":{"cells":(0..width).map(|i| rich_text(row.get(i).map(String::as_str).unwrap_or(""))).collect::<Vec<_>>()}})).collect();
                        let table = json!({"object":"block","type":"table","table":{"table_width":width,"has_column_header":false,"has_row_header":false,"children":children}});
                        if table.to_string().len() > 400_000 {
                            for row in batch { text_blocks("paragraph", &row.join(" | "), &mut blocks); }
                        } else { blocks.push(table); }
                    }
                }
            },
            Part::Code(t) => for chunk in split_text(&t, 2000) {
                blocks.push(json!({"object":"block","type":"code","code":{"language":"plain text","rich_text":rich_text(&chunk)}}));
            },
            Part::Text(t) => for paragraph in t.split("\n\n").filter(|p| !p.trim().is_empty()) {
                let (kind, content) = if let Some(t) = paragraph.strip_prefix("# ") { ("heading_1",t) }
                    else if let Some(t) = paragraph.strip_prefix("## ") { ("heading_2",t) }
                    else if let Some(t) = paragraph.strip_prefix("### ") { ("heading_3",t) }
                    else { ("paragraph",paragraph) };
                if kind == "paragraph" && content.lines().any(|l| list_line(l).is_some()) {
                    for line in content.lines().filter(|l| !l.trim().is_empty()) {
                        let (kind, text) = list_line(line).unwrap_or(("paragraph", line));
                        markdown_text_blocks(kind, text, &mut blocks);
                    }
                } else { markdown_text_blocks(kind, content, &mut blocks); }
            },
            Part::Image(url, alt) if url.len() <= 2000 => blocks.push(json!({"object":"block","type":"image","image":{"type":"external","external":{"url":url},"caption":rich_text(&alt).into_iter().take(100).collect::<Vec<_>>()}})),
            Part::Image(url, _) => text_blocks("paragraph", &url, &mut blocks),
        }
    }
    if !highlights.is_empty() {
        text_blocks("heading_2", "Highlights and notes", &mut blocks);
    }
    for h in ordered_highlights(highlights) {
        text_blocks("quote", &h.quote, &mut blocks);
        text_blocks("paragraph", &h.note, &mut blocks);
    }
    blocks
}
#[cfg(test)]
mod tests {
    use super::*;
    fn article() -> ArticleDetail {
        ArticleDetail { id: 42, feed_id: 1, feed_title: "Example feed".into(), source_type: "rss".into(), title: "A [title]".into(),
            author: Some("Author".into()), url: Some("https://example.com/article".into()), content_html: Some("<p>Feed snippet</p>".into()),
            extracted_html: Some("<h2>Full body</h2><p>A <strong>bold</strong> <a href='/link'>link</a> &amp; text.</p><ol><li>First</li><li>Second</li></ol><pre><code>a &lt; b\n```</code></pre><table><tr><th>A</th><th>B</th></tr><tr><td>1</td><td>2</td></tr></table><img src='/original.jpg' alt='Photo'>".into()),
            image_url: None, published_at: Some("2026-01-02T03:04:05Z".into()), is_read: false, is_starred: false, read_later: false,
            ai_summary: None, translated_html: None, translated_lang: None, enclosures: vec![], tags: vec![] }
    }
    fn highlight(id: i64, offset: i64, quote: &str, note: &str) -> Highlight {
        Highlight {
            id,
            article_id: 42,
            quote: quote.into(),
            note: note.into(),
            text_offset: offset,
            prefix: String::new(),
            suffix: String::new(),
            color: "yellow".into(),
            created_at: String::new(),
        }
    }
    #[test]
    fn markdown_exports_full_body_structure_original_images_and_ordered_notes() {
        let exported = markdown(
            &article(),
            &[
                highlight(2, 20, "Later\nfull quote", "Note **literal**"),
                highlight(1, 0, "First quote", ""),
            ],
        );
        assert!(exported.contains("## Full body"));
        assert!(!exported.contains("Feed snippet"));
        assert!(exported.contains("1. First"));
        assert!(exported.contains("2. Second"));
        assert!(exported.contains("````\na < b\n```\n````"));
        assert!(exported.contains("| A | B |\n| --- | --- |"));
        assert!(exported.contains("https://example.com/original.jpg"));
        assert!(exported.find("First quote").unwrap() < exported.find("Later").unwrap());
        assert!(exported.contains("> Later\n> full quote"));
        assert!(exported.contains(r"Note \*\*literal\*\*"));
    }
    #[test]
    fn safe_vault_paths_and_stable_atomic_overwrite() {
        let vault = tempfile::tempdir().unwrap();
        let vault_path = vault.path().to_str().unwrap();
        assert!(obsidian_directory("relative", "", false).is_err());
        for path in [
            "../escape",
            "/absolute",
            "folder/../../escape",
            "folder\\escape",
        ] {
            assert!(obsidian_directory(vault_path, path, true).is_err());
        }
        let first = write_obsidian(vault_path, "Articles", &article(), &[], None).unwrap();
        assert!(write_obsidian(vault_path, "Articles", &article(), &[], None).is_err());
        let mut renamed = article();
        renamed.title = "Renamed".into();
        let second = write_obsidian(
            vault_path,
            "Articles",
            &renamed,
            &[],
            Some("papr-article-42.md"),
        )
        .unwrap();
        assert_eq!(first, second);
        assert!(std::fs::read_to_string(first)
            .unwrap()
            .contains("# Renamed"));
        assert_eq!(
            std::fs::read_dir(vault.path().join("Articles"))
                .unwrap()
                .count(),
            1
        );
        assert!(write_obsidian(
            vault_path,
            "Articles",
            &article(),
            &[],
            Some("../escape.md")
        )
        .is_err());
    }
    #[cfg(unix)]
    #[test]
    fn rejects_external_and_dangling_symlinks_and_file_links() {
        use std::os::unix::fs::symlink;
        let vault = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = vault.path().to_str().unwrap();
        symlink(outside.path(), vault.path().join("outside")).unwrap();
        assert!(obsidian_directory(root, "outside/new", true).is_err());
        assert!(!outside.path().join("new").exists());
        symlink(
            outside.path().join("missing"),
            vault.path().join("dangling"),
        )
        .unwrap();
        assert!(obsidian_directory(root, "dangling", true).is_err());
        std::fs::create_dir(vault.path().join("inside")).unwrap();
        symlink(vault.path().join("inside"), vault.path().join("alias")).unwrap();
        assert!(obsidian_directory(root, "alias", false).is_ok());
        let victim = outside.path().join("victim");
        std::fs::write(&victim, "keep").unwrap();
        symlink(&victim, vault.path().join("papr-article-42.md")).unwrap();
        assert!(write_obsidian(root, "", &article(), &[], None).is_err());
        assert_eq!(std::fs::read_to_string(victim).unwrap(), "keep");
    }
    #[test]
    fn notion_ids_and_schema_mapping_are_type_checked() {
        let id = "12345678-1234-1234-1234-123456789abc";
        assert_eq!(
            notion_id(&format!(
                "https://www.notion.so/Title-{}?v=ignored",
                id.replace('-', "")
            ))
            .unwrap(),
            id
        );
        assert_eq!(notion_id(id).unwrap(), id);
        assert!(notion_id("https://evil.com/12345678123412341234123456789abc").is_err());
        assert!(notion_id("abc").is_err());
        let schema = json!({"Name":{"type":"title"},"Source URL":{"type":"url"},"Feed":{"type":"rich_text"},"Published":{"type":"date"},"URL":{"type":"rich_text"},"Other":{"type":"number"}});
        let props = page_properties(&schema, &article()).unwrap();
        assert_eq!(props["Source URL"]["url"], "https://example.com/article");
        assert!(props.get("URL").is_none());
        assert!(props.get("Other").is_none());
        assert!(props["Published"]["date"]["start"].is_string());
        assert!(title_property(&json!({"Name":{"type":"rich_text"}})).is_err());
    }
    #[test]
    fn notion_preserves_unicode_long_quotes_notes_rich_text_and_api_limits() {
        let quote = "🦀漢字".repeat(5000);
        let note = "Long note ".repeat(2000);
        let blocks = notion_blocks(&article(), &[highlight(1, 0, &quote, &note)]);
        let quotes: String = blocks
            .iter()
            .filter(|b| b["type"] == "quote")
            .flat_map(|b| b["quote"]["rich_text"].as_array().unwrap())
            .map(|v| v["text"]["content"].as_str().unwrap())
            .collect();
        assert_eq!(quotes, quote);
        let full = serde_json::to_string(&blocks).unwrap();
        assert!(full.contains("\"bold\":true"));
        assert!(full.contains("https://example.com/link"));
        assert!(blocks.iter().any(|b| b["type"] == "table"));
        assert!(blocks.iter().any(|b| b["type"] == "code"));
        assert!(blocks.iter().any(|b| b["type"] == "numbered_list_item"));
        for batch in block_batches(&blocks) {
            assert!(batch.len() <= 100);
            assert!(json!({"children":batch}).to_string().len() < 500_000);
        }
        for chunk in split_text(&quote, 2000) {
            assert!(chunk.encode_utf16().count() <= 2000);
        }
    }
    async fn mock_notion(
        replies: Vec<(u16, Value)>,
    ) -> (Notion, tokio::task::JoinHandle<Vec<(String, Value)>>) {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let mut requests = Vec::new();
            for (status, body) in replies {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let header_end = loop {
                    let mut buf = [0u8; 4096];
                    let n = socket.read(&mut buf).await.unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(i) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let header = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
                let length: usize = header
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|n| n.parse().ok())
                    })
                    .unwrap_or(0);
                while bytes.len() < header_end + length {
                    let mut buf = [0u8; 4096];
                    let n = socket.read(&mut buf).await.unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buf[..n]);
                }
                let payload = if length == 0 {
                    Value::Null
                } else {
                    serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap()
                };
                requests.push((header.lines().next().unwrap().to_string(), payload));
                let body = body.to_string();
                let response = format!("HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
                socket.write_all(response.as_bytes()).await.unwrap();
            }
            requests
        });
        let mut notion = Notion::new(NotionConfig {
            token: "secret-token".into(),
            target_type: TargetType::Page,
            target_id: "target".into(),
        })
        .unwrap();
        notion.base_url = Some(format!("http://{address}"));
        (notion, task)
    }
    #[tokio::test]
    async fn notion_updates_append_before_delete_and_paginate() {
        let (notion, requests) = mock_notion(vec![
            (
                200,
                json!({"results":[{"id":"old-1"}],"has_more":true,"next_cursor":"next"}),
            ),
            (200, json!({"results":[{"id":"old-2"}],"has_more":false})),
            (200, json!({})),
            (200, json!({})),
            (200, json!({})),
            (200, json!({})),
        ])
        .await;
        let target = NotionTarget {
            destination: "page:target".into(),
            parent: json!({"page_id":"target"}),
            properties: json!({"title":{"type":"title"}}),
        };
        notion
            .export(&target, "existing", &article(), &[])
            .await
            .unwrap();
        let requests = requests.await.unwrap();
        assert!(requests[1].0.contains("start_cursor=next"));
        assert!(requests[2].0.starts_with("PATCH /blocks/existing/children"));
        assert!(requests[2].1["children"].is_array());
        assert!(requests[3].0.starts_with("PATCH /pages/existing"));
        assert!(requests[4].0.starts_with("DELETE /blocks/old-1"));
        assert!(requests[5].0.starts_with("DELETE /blocks/old-2"));
    }
    #[tokio::test]
    async fn notion_append_failure_keeps_old_blocks_and_hides_remote_secrets() {
        let (notion, requests) = mock_notion(vec![
            (200, json!({"results":[{"id":"old"}],"has_more":false})),
            (403, json!({"message":"secret-token must never be shown"})),
        ])
        .await;
        let target = NotionTarget {
            destination: "page:target".into(),
            parent: json!({"page_id":"target"}),
            properties: json!({"title":{"type":"title"}}),
        };
        let error = notion
            .export(&target, "existing", &article(), &[])
            .await
            .unwrap_err()
            .to_string();
        assert!(!error.contains("secret-token"));
        assert!(error.contains("permissions"));
        assert!(requests
            .await
            .unwrap()
            .iter()
            .all(|r| !r.0.starts_with("DELETE")));
    }
    #[tokio::test]
    async fn notion_recovers_pending_create_without_another_page() {
        let (notion, requests) = mock_notion(vec![
            (200,json!({"results":[{"id":"recovered","child_page":{"title":"papr-marker"}}],"has_more":false})),
        ]).await;
        let target = NotionTarget {
            destination: "page:target".into(),
            parent: json!({"page_id":"target"}),
            properties: json!({"title":{"type":"title"}}),
        };
        assert_eq!(
            notion
                .ensure_page(&target, "pending:papr-marker")
                .await
                .unwrap(),
            "recovered"
        );
        assert_eq!(requests.await.unwrap().len(), 1);
    }
    #[tokio::test]
    async fn notion_resolves_modern_database_and_creates_with_data_source_parent() {
        let (mut notion, requests) = mock_notion(vec![
            (200, json!({"data_sources":[{"id":"source"}]})),
            (200, json!({"properties":{"Name":{"type":"title"}}})),
            (200, json!({"results":[]})),
            (200, json!({"id":"created"})),
        ])
        .await;
        notion.config.target_type = TargetType::Database;
        let target = notion.validate().await.unwrap();
        assert_eq!(target.destination, "data_source:source");
        assert_eq!(
            notion
                .ensure_page(&target, "pending:papr-marker")
                .await
                .unwrap(),
            "created"
        );
        let requests = requests.await.unwrap();
        assert!(requests[2].0.starts_with("POST /data_sources/source/query"));
        assert_eq!(requests[3].1["parent"]["data_source_id"], "source");
        assert_eq!(
            requests[3].1["properties"]["Name"]["title"][0]["text"]["content"],
            "papr-marker"
        );
    }
    #[test]
    fn settings_hide_token_and_destinations_reuse_mappings() {
        let conn = db::open(Path::new(":memory:")).unwrap();
        conn.execute(
            "INSERT INTO feeds(id,feed_url,title) VALUES(1,'https://example.com/feed','Feed')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO articles(id,feed_id,guid,title) VALUES(42,1,'guid','Title')",
            [],
        )
        .unwrap();
        let config = NotionConfig::load(
            &conn,
            Some("secret-token"),
            TargetType::Page,
            "12345678123412341234123456789abc",
        )
        .unwrap();
        config.save(&conn).unwrap();
        assert!(!serde_json::to_string(&settings(&conn).unwrap())
            .unwrap()
            .contains("secret-token"));
        let unchanged =
            NotionConfig::load(&conn, Some(""), TargetType::Page, &config.target_id).unwrap();
        assert_eq!(unchanged.token, "secret-token");
        let reservation = db::reserve_notion_archive(&conn, 42, "page:one").unwrap();
        assert!(reservation.starts_with("pending:"));
        assert_eq!(
            db::reserve_notion_archive(&conn, 42, "page:one").unwrap(),
            reservation
        );
        db::save_archive_location(&conn, 42, "notion", "page:one", "existing-page").unwrap();
        assert_eq!(
            db::reserve_notion_archive(&conn, 42, "page:one").unwrap(),
            "existing-page"
        );
        assert_ne!(
            db::reserve_notion_archive(&conn, 42, "page:two").unwrap(),
            "existing-page"
        );
        disconnect(&conn, Service::Notion).unwrap();
        assert!(setting(&conn, "archive_notion_token").unwrap().is_empty());
        assert_eq!(
            db::archive_location(&conn, 42, "notion", "page:one")
                .unwrap()
                .unwrap(),
            "existing-page"
        );
    }
}
fn list_line(line: &str) -> Option<(&'static str, &str)> {
    let line = line.trim_start();
    if let Some(text) = line.strip_prefix("- ") {
        return Some(("bulleted_list_item", text));
    }
    let (number, text) = line.split_once(". ")?;
    (!number.is_empty() && number.bytes().all(|c| c.is_ascii_digit()))
        .then_some(("numbered_list_item", text))
}
fn block_batches(blocks: &[Value]) -> Vec<&[Value]> {
    let mut batches = Vec::new();
    let mut start = 0;
    let mut bytes = 32;
    for (index, block) in blocks.iter().enumerate() {
        let size = block.to_string().len() + 1;
        if index > start && (index - start >= 100 || bytes + size > 450_000) {
            batches.push(&blocks[start..index]);
            start = index;
            bytes = 32;
        }
        bytes += size;
    }
    if start < blocks.len() {
        batches.push(&blocks[start..]);
    }
    batches
}
