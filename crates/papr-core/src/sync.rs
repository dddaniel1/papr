//! Feed synchronisation for GReader-compatible services and Feedly.
//!
//! FreshRSS, Miniflux and Inoreader share the Google Reader protocol. FreshRSS
//! and Miniflux authenticate with ClientLogin; Inoreader uses OAuth bearer
//! tokens. Feedly exposes its own v3 API, implemented below as a separate
//! adapter so its collection, stream and marker semantics stay explicit.
//!
//! The persisted setting keys keep their historical `freshrss_*` names. They
//! now store provider-neutral connection state so existing installations keep
//! working without a migration.

use crate::db;
use crate::error::{AppError, AppResult};
use crate::ingestion::parse;
use reqwest::{Client, RequestBuilder};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use tokio::sync::Mutex;

type Db = Mutex<Connection>;

const READ_TAG: &str = "user/-/state/com.google/read";
const STARRED_TAG: &str = "user/-/state/com.google/starred";
const READING_LIST: &str = "user/-/state/com.google/reading-list";
const LABEL_PREFIX: &str = "user/-/label/";
const INOREADER_BASE: &str = "https://www.inoreader.com";
const FEEDLY_BASE: &str = "https://api.feedly.com";
/// Inoreader requires the redirect URI to be registered exactly in the app.
/// A fixed loopback port keeps that registration stable across launches.
pub const INOREADER_REDIRECT_URI: &str = "http://127.0.0.1:19812/oauth/callback";
const FEEDLY_UNFILED_LABEL: &str = "Papr";

fn label_stream(name: &str) -> String {
    format!("{LABEL_PREFIX}{name}")
}

fn folder_change(
    local: Option<&str>,
    server: Option<&str>,
) -> Option<(Option<String>, Option<String>)> {
    if local == server {
        return None;
    }
    Some((local.map(label_stream), server.map(label_stream)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    FreshRss,
    Miniflux,
    Inoreader,
    Feedly,
}

impl Provider {
    fn path_suffix(self) -> &'static str {
        match self {
            Provider::FreshRss => "/api/greader.php",
            Provider::Miniflux | Provider::Inoreader => "",
            Provider::Feedly => "",
        }
    }

    fn from_setting(s: Option<&str>) -> Self {
        match s.unwrap_or("").trim().to_ascii_lowercase().as_str() {
            "miniflux" => Provider::Miniflux,
            "inoreader" => Provider::Inoreader,
            "feedly" => Provider::Feedly,
            _ => Provider::FreshRss,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Provider::FreshRss => "freshrss",
            Provider::Miniflux => "miniflux",
            Provider::Inoreader => "inoreader",
            Provider::Feedly => "feedly",
        }
    }

}

fn greader_base(url: &str, provider: Provider) -> String {
    if provider == Provider::Inoreader {
        return INOREADER_BASE.to_string();
    }
    let t = url.trim().trim_end_matches('/');
    let suffix = provider.path_suffix();
    if suffix.is_empty() || t.ends_with(suffix) || t.contains(&format!("{suffix}/")) {
        t.to_string()
    } else {
        format!("{t}{suffix}")
    }
}

fn feedly_base(url: &str) -> String {
    let t = url.trim().trim_end_matches('/');
    if t.is_empty() {
        FEEDLY_BASE.to_string()
    } else {
        t.to_string()
    }
}

fn encode_path(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn feedly_feed_id(url: &str) -> String {
    format!("feed/{url}")
}

fn feedly_feed_url(id: &str) -> String {
    id.strip_prefix("feed/").unwrap_or(id).to_string()
}

fn feedly_collection_is_unfiled(label: &str) -> bool {
    label.eq_ignore_ascii_case(FEEDLY_UNFILED_LABEL)
        || label.eq_ignore_ascii_case("Uncategorized")
}

struct Session {
    base: String,
    auth: String,
    token: Option<String>,
    bearer: bool,
}

impl Session {
    fn authorize(&self, req: RequestBuilder) -> RequestBuilder {
        if self.bearer {
            req.bearer_auth(&self.auth)
        } else {
            req.header("Authorization", format!("GoogleLogin auth={}", self.auth))
        }
    }

    fn get(&self, http: &Client, path: &str) -> RequestBuilder {
        self.authorize(http.get(format!("{}/reader/api/0/{path}", self.base)))
    }

    fn post(&self, http: &Client, path: &str) -> RequestBuilder {
        self.authorize(http.post(format!("{}/reader/api/0/{path}", self.base)))
    }

    fn post_form<'a>(
        &'a self,
        http: &Client,
        path: &str,
        mut form: Vec<(&'a str, &'a str)>,
    ) -> RequestBuilder {
        if let Some(token) = self.token.as_deref() {
            form.push(("T", token));
        }
        self.post(http, path).form(&form)
    }
}

async fn client_login(http: &Client, base: &str, user: &str, pass: &str) -> AppResult<String> {
    let resp = http
        .post(format!("{base}/accounts/ClientLogin"))
        .form(&[("Email", user), ("Passwd", pass)])
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(AppError::code("freshrssLoginFailed"));
    }
    let body = resp.text().await?;
    body.lines()
        .find_map(|l| l.strip_prefix("Auth="))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::code("freshrssNoToken"))
}

async fn session_with_token(
    http: &Client,
    base: &str,
    auth: String,
    provider: Provider,
) -> AppResult<Session> {
    if provider == Provider::Inoreader {
        return Ok(Session {
            base: base.to_string(),
            auth,
            token: None,
            bearer: true,
        });
    }
    let token = http
        .get(format!("{base}/reader/api/0/token"))
        .header("Authorization", format!("GoogleLogin auth={auth}"))
        .send()
        .await?
        .error_for_status()
        .map_err(|_| AppError::code("freshrssLoginFailed"))?
        .text()
        .await?
        .trim()
        .to_string();
    Ok(Session {
        base: base.to_string(),
        auth,
        token: Some(token),
        bearer: false,
    })
}

async fn login(http: &Client, base: &str, user: &str, pass: &str) -> AppResult<Session> {
    let auth = client_login(http, base, user, pass).await?;
    session_with_token(http, base, auth, Provider::FreshRss).await
}

#[derive(Deserialize)]
struct SubList {
    #[serde(default)]
    subscriptions: Vec<Sub>,
}
#[derive(Deserialize)]
struct Sub {
    #[serde(default)]
    id: Option<String>,
    url: Option<String>,
    title: Option<String>,
    #[serde(default)]
    categories: Vec<SubCat>,
}
#[derive(Deserialize)]
struct SubCat {
    #[serde(default)]
    id: String,
    #[serde(default)]
    label: Option<String>,
}
impl SubCat {
    fn folder_name(&self) -> Option<String> {
        self.label
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| {
                self.id
                    .rsplit_once("/label/")
                    .map(|(_, n)| n.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
            .filter(|n| !n.eq_ignore_ascii_case("Uncategorized"))
    }
}

#[derive(Deserialize)]
struct Contents {
    #[serde(default)]
    items: Vec<Item>,
    #[serde(default)]
    continuation: Option<String>,
}
#[derive(Deserialize)]
struct Item {
    id: String,
    #[serde(default)]
    canonical: Vec<Href>,
    #[serde(default)]
    alternate: Vec<Href>,
}
#[derive(Deserialize)]
struct Href {
    href: String,
}

#[derive(Deserialize)]
struct TagList {
    #[serde(default)]
    tags: Vec<Tag>,
}
#[derive(Deserialize)]
struct Tag {
    #[serde(default)]
    id: String,
    #[serde(default, rename = "type")]
    kind: Option<String>,
}
impl Tag {
    fn folder_name(&self) -> Option<String> {
        let name = self.id.strip_prefix(LABEL_PREFIX)?.trim();
        if name.is_empty() || name.eq_ignore_ascii_case("Uncategorized") {
            return None;
        }
        if self.kind.as_deref().is_some_and(|k| k != "folder") {
            return None;
        }
        Some(name.to_string())
    }
}

#[derive(Deserialize)]
struct UnreadCounts {
    #[serde(default)]
    unreadcounts: Vec<UnreadCount>,
}
#[derive(Deserialize)]
struct UnreadCount {
    #[serde(default)]
    id: String,
    #[serde(default)]
    count: i64,
}

struct Creds {
    url: String,
    user: String,
    auth: Option<String>,
    legacy_pass: Option<String>,
    provider: Provider,
    refresh_token: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    token_expires_at: Option<i64>,
}

async fn creds(db: &Db) -> AppResult<Option<Creds>> {
    let conn = db.lock().await;
    let url = db::get_setting(&conn, "freshrss_url")?.unwrap_or_default();
    let user = db::get_setting(&conn, "freshrss_user")?.unwrap_or_default();
    let nonempty = |k| db::get_setting(&conn, k).map(|v| v.filter(|s| !s.is_empty()));
    let auth = nonempty("freshrss_auth")?;
    let legacy_pass = nonempty("freshrss_pass")?;
    let provider = Provider::from_setting(db::get_setting(&conn, "freshrss_provider")?.as_deref());
    let refresh_token = nonempty("sync_refresh_token")?;
    let client_id = nonempty("inoreader_client_id")?;
    let client_secret = nonempty("inoreader_client_secret")?;
    let token_expires_at = nonempty("sync_token_expires_at")?
        .and_then(|v| v.parse::<i64>().ok());
    if url.trim().is_empty() || user.is_empty() || (auth.is_none() && legacy_pass.is_none()) {
        return Ok(None);
    }
    Ok(Some(Creds {
        url,
        user,
        auth,
        legacy_pass,
        provider,
        refresh_token,
        client_id,
        client_secret,
        token_expires_at,
    }))
}

pub async fn connected_url(db: &Db) -> AppResult<Option<(String, String)>> {
    Ok(creds(db).await?.map(|c| (c.url, c.provider.as_str().to_string())))
}

#[allow(clippy::too_many_arguments)]
async fn persist_connection(
    db: &Db,
    url: &str,
    user: &str,
    auth: &str,
    provider: Provider,
    refresh_token: Option<&str>,
    client_id: Option<&str>,
    client_secret: Option<&str>,
    token_expires_at: Option<i64>,
) -> AppResult<()> {
    let conn = db.lock().await;
    let old_url = db::get_setting(&conn, "freshrss_url")?.unwrap_or_default();
    let old_user = db::get_setting(&conn, "freshrss_user")?.unwrap_or_default();
    let old_provider = Provider::from_setting(
        db::get_setting(&conn, "freshrss_provider")?.as_deref(),
    );
    if !old_url.trim().is_empty()
        && (old_provider != provider
            || old_url.trim() != url.trim()
            || old_user != user)
    {
        db::clear_sync_identity(&conn)?;
    }
    db::set_setting(&conn, "freshrss_url", url.trim())?;
    db::set_setting(&conn, "freshrss_user", user)?;
    db::set_setting(&conn, "freshrss_auth", auth)?;
    db::set_setting(&conn, "freshrss_pass", "")?;
    db::set_setting(&conn, "freshrss_provider", provider.as_str())?;
    db::set_setting(&conn, "sync_refresh_token", refresh_token.unwrap_or(""))?;
    db::set_setting(&conn, "inoreader_client_id", client_id.unwrap_or(""))?;
    db::set_setting(&conn, "inoreader_client_secret", client_secret.unwrap_or(""))?;
    db::set_setting(
        &conn,
        "sync_token_expires_at",
        &token_expires_at.map(|v| v.to_string()).unwrap_or_default(),
    )?;
    Ok(())
}

async fn persist_session(
    db: &Db,
    url: &str,
    user: &str,
    auth: &str,
    provider: Provider,
) -> AppResult<()> {
    persist_connection(db, url, user, auth, provider, None, None, None, None).await
}

pub async fn connect(
    db: &Db,
    http: &Client,
    url: &str,
    user: &str,
    pass: &str,
    provider: Option<&str>,
) -> AppResult<()> {
    let provider = Provider::from_setting(provider);
    if provider == Provider::Feedly {
        return connect_feedly(db, http, pass).await;
    }
    if provider == Provider::Inoreader {
        return connect_inoreader_token(db, http, pass).await;
    }
    let base = greader_base(url, provider);
    let session = login(http, &base, user, pass).await?;
    persist_session(db, url, user, &session.auth, provider).await
}

pub async fn connect_feedly(db: &Db, http: &Client, token: &str) -> AppResult<()> {
    let token = token.trim();
    if token.is_empty() {
        return Err(AppError::code("feedlyTokenRequired"));
    }
    let profile = feedly_profile(http, token).await?;
    persist_connection(
        db,
        FEEDLY_BASE,
        &profile.id,
        token,
        Provider::Feedly,
        None,
        None,
        None,
        None,
    )
    .await
}

pub async fn connect_inoreader_token(db: &Db, http: &Client, token: &str) -> AppResult<()> {
    let token = token.trim();
    if token.is_empty() {
        return Err(AppError::code("inoreaderTokenRequired"));
    }
    let user = inoreader_user_id(http, token).await?;
    persist_connection(
        db,
        INOREADER_BASE,
        &user,
        token,
        Provider::Inoreader,
        None,
        None,
        None,
        None,
    )
    .await
}

pub async fn disconnect(db: &Db) -> AppResult<()> {
    let conn = db.lock().await;
    for key in [
        "freshrss_url",
        "freshrss_user",
        "freshrss_auth",
        "freshrss_pass",
        "freshrss_provider",
        "sync_refresh_token",
        "inoreader_client_id",
        "inoreader_client_secret",
        "sync_token_expires_at",
    ] {
        db::set_setting(&conn, key, "")?;
    }
    db::clear_sync_identity(&conn)?;
    db::clear_server_unread(&conn)?;
    Ok(())
}

pub async fn run_if_connected(db: &Db, http: &Client) -> AppResult<bool> {
    if creds(db).await?.is_some() {
        sync_now(db, http).await.map(|_| true)
    } else {
        Ok(false)
    }
}

// ─────────────────────────── GReader flow ───────────────────────────

async fn push_ops(db: &Db, http: &Client, session: &Session) {
    let ops = {
        let conn = db.lock().await;
        match db::list_ops(&conn) {
            Ok(ops) => ops,
            Err(e) => {
                log::warn!("sync: could not read pending ops: {e}");
                return;
            }
        }
    };
    for op in ops {
        let ok = match op.kind.as_str() {
            db::OP_UNSUBSCRIBE => {
                let stream = format!("feed/{}", op.target);
                session
                    .post_form(http, "subscription/edit", vec![("ac", "unsubscribe"), ("s", stream.as_str())])
                    .send()
                    .await
                    .and_then(|r| r.error_for_status())
                    .is_ok()
            }
            db::OP_RENAME_FOLDER => {
                let from = label_stream(&op.target);
                let to = label_stream(op.value.as_deref().unwrap_or_default());
                session
                    .post_form(http, "rename-tag", vec![("s", from.as_str()), ("dest", to.as_str())])
                    .send()
                    .await
                    .and_then(|r| r.error_for_status())
                    .is_ok()
            }
            db::OP_DELETE_FOLDER => {
                let stream = label_stream(&op.target);
                session
                    .post_form(http, "disable-tag", vec![("s", stream.as_str())])
                    .send()
                    .await
                    .and_then(|r| r.error_for_status())
                    .is_ok()
            }
            db::OP_MARK_ALL_READ => {
                let ts = op.value.as_deref().unwrap_or("0");
                session
                    .post_form(
                        http,
                        "mark-all-as-read",
                        vec![("s", op.target.as_str()), ("ts", ts)],
                    )
                    .send()
                    .await
                    .and_then(|r| r.error_for_status())
                    .is_ok()
            }
            other => {
                log::warn!("sync: dropping unknown op kind {other}");
                true
            }
        };
        if ok {
            let conn = db.lock().await;
            if let Err(e) = db::delete_op(&conn, op.id) {
                log::warn!("sync: could not clear op #{}: {e}", op.id);
            }
        } else {
            log::warn!("sync: op #{} ({}) failed, will retry", op.id, op.kind);
        }
    }
}

struct ServerSubs {
    urls: HashSet<String>,
    folder: HashMap<String, Option<String>>,
    title: HashMap<String, String>,
    id_to_url: HashMap<String, String>,
}

async fn push_subscriptions(
    http: &Client,
    session: &Session,
    local: &[db::SyncFeed],
    server: &ServerSubs,
    skip: &HashSet<String>,
    stale_folders: &HashSet<String>,
) {
    for feed in local {
        if skip.contains(&feed.url) {
            continue;
        }
        let stream = format!("feed/{}", feed.url);
        if !server.urls.contains(&feed.url) {
            let label = feed.folder.as_deref().map(label_stream);
            let mut form = vec![("ac", "subscribe"), ("s", stream.as_str())];
            if let Some(label) = label.as_deref() {
                form.push(("a", label));
            }
            if session
                .post_form(http, "subscription/edit", form)
                .send()
                .await
                .and_then(|r| r.error_for_status())
                .is_err()
            {
                log::warn!("sync: failed to subscribe server to {}", feed.url);
            }
            continue;
        }

        let server_folder = server.folder.get(&feed.url).cloned().flatten();
        if !server_folder
            .as_deref()
            .is_some_and(|f| stale_folders.contains(f))
        {
            if let Some((add, remove)) =
                folder_change(feed.folder.as_deref(), server_folder.as_deref())
            {
                let mut form = vec![("ac", "edit"), ("s", stream.as_str())];
                if let Some(add) = add.as_deref() {
                    form.push(("a", add));
                }
                if let Some(remove) = remove.as_deref() {
                    form.push(("r", remove));
                }
                if session
                    .post_form(http, "subscription/edit", form)
                    .send()
                    .await
                    .and_then(|r| r.error_for_status())
                    .is_err()
                {
                    log::warn!("sync: failed to file {} into its folder", feed.url);
                }
            }
        }

        if feed.custom_title {
            if let Some(server_title) = server.title.get(&feed.url) {
                if &feed.title != server_title
                    && session
                        .post_form(
                            http,
                            "subscription/edit",
                            vec![("ac", "edit"), ("s", stream.as_str()), ("t", feed.title.as_str())],
                        )
                        .send()
                        .await
                        .and_then(|r| r.error_for_status())
                        .is_err()
                {
                    log::warn!("sync: failed to rename {}", feed.url);
                }
            }
        }
    }
}

async fn pull_folders(db: &Db, http: &Client, session: &Session, stale_folders: &HashSet<String>) {
    let list: TagList = match session
        .get(http, "tag/list?output=json")
        .send()
        .await
        .and_then(|r| r.error_for_status())
    {
        Ok(resp) => match resp.json().await {
            Ok(list) => list,
            Err(e) => {
                log::warn!("sync: tag/list decode failed: {e}");
                return;
            }
        },
        Err(e) => {
            log::warn!("sync: tag/list unavailable: {e}");
            return;
        }
    };
    let conn = db.lock().await;
    for name in list.tags.iter().filter_map(Tag::folder_name) {
        if stale_folders.contains(&name) {
            continue;
        }
        if let Err(e) = db::folder_id_by_name(&conn, &name) {
            log::warn!("sync: could not create folder {name}: {e}");
        }
    }
}

async fn pull_unread_counts(
    db: &Db,
    http: &Client,
    session: &Session,
    id_to_url: &HashMap<String, String>,
) {
    let resp = match session
        .get(http, "unread-count?output=json&all=1")
        .send()
        .await
        .and_then(|r| r.error_for_status())
    {
        Ok(resp) => resp,
        Err(e) => {
            log::warn!("sync: unread-count unavailable: {e}");
            return;
        }
    };
    let body: UnreadCounts = match resp.json().await {
        Ok(body) => body,
        Err(e) => {
            log::warn!("sync: unread-count decode failed: {e}");
            return;
        }
    };
    let conn = db.lock().await;
    let mut counts: HashMap<i64, i64> = HashMap::new();
    for entry in &body.unreadcounts {
        let Some(server_id) = entry.id.strip_prefix("feed/") else {
            continue;
        };
        let Some(url) = id_to_url.get(server_id) else {
            continue;
        };
        if let Ok(Some(feed_id)) = db::find_feed_by_url(&conn, url) {
            counts.insert(feed_id, entry.count);
        }
    }
    if let Err(e) = db::apply_server_unread(&conn, &counts) {
        log::warn!("sync: could not cache server unread counts: {e}");
    }
}

struct RemoteItem {
    id: String,
    url: String,
}

async fn fetch_stream_items(
    session: &Session,
    http: &Client,
    stream: &str,
    xt: Option<&str>,
) -> AppResult<Vec<RemoteItem>> {
    const PAGE: &str = "1000";
    const MAX_PAGES: usize = 30;
    let path = format!("stream/contents/{stream}");
    let mut out = Vec::new();
    let mut cont: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut params: Vec<(&str, &str)> = vec![("output", "json"), ("n", PAGE)];
        if let Some(xt) = xt {
            params.push(("xt", xt));
        }
        if let Some(c) = cont.as_deref() {
            params.push(("c", c));
        }
        let page: Contents = session
            .get(http, &path)
            .query(&params)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        for item in page.items {
            if let Some(url) = item
                .canonical
                .first()
                .or_else(|| item.alternate.first())
                .map(|h| h.href.clone())
            {
                out.push(RemoteItem { id: item.id, url });
            }
        }
        match page.continuation {
            Some(c) if !c.is_empty() => cont = Some(c),
            _ => break,
        }
    }
    Ok(out)
}

async fn inoreader_refresh(
    http: &Client,
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> AppResult<OAuthTokens> {
    let resp = http
        .post(format!("{INOREADER_BASE}/oauth2/token"))
        .form(&[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(AppError::code("inoreaderOauthFailed"));
    }
    Ok(resp.json().await?)
}

async fn inoreader_session(
    db: &Db,
    http: &Client,
    creds: &Creds,
) -> AppResult<Session> {
    let mut auth = creds
        .auth
        .clone()
        .ok_or_else(|| AppError::code("freshrssNotConnected"))?;
    let expires_soon = creds
        .token_expires_at
        .is_some_and(|at| at <= now_secs() + 60);
    if expires_soon {
        let (Some(client_id), Some(client_secret), Some(refresh_token)) = (
            creds.client_id.as_deref(),
            creds.client_secret.as_deref(),
            creds.refresh_token.as_deref(),
        ) else {
            return Err(AppError::code("inoreaderOauthFailed"));
        };
        let tokens = inoreader_refresh(http, client_id, client_secret, refresh_token).await?;
        auth = tokens.access_token.clone();
        let refresh = tokens
            .refresh_token
            .as_deref()
            .unwrap_or(refresh_token);
        persist_connection(
            db,
            INOREADER_BASE,
            &creds.user,
            &auth,
            Provider::Inoreader,
            Some(refresh),
            Some(client_id),
            Some(client_secret),
            Some(now_secs() + tokens.expires_in.unwrap_or(3600)),
        )
        .await?;
    }
    session_with_token(http, INOREADER_BASE, auth, Provider::Inoreader).await
}

async fn sync_greader_now(db: &Db, http: &Client, creds: Creds) -> AppResult<usize> {
    let base = greader_base(&creds.url, creds.provider);
    let session = match creds.provider {
        Provider::Inoreader => inoreader_session(db, http, &creds).await?,
        _ => match &creds.auth {
            Some(auth) => session_with_token(http, &base, auth.clone(), creds.provider).await?,
            None => {
                let pass = creds.legacy_pass.as_deref().unwrap_or_default();
                let session = login(http, &base, &creds.user, pass).await?;
                persist_session(db, &creds.url, &creds.user, &session.auth, creds.provider).await?;
                session
            }
        },
    };

    let queue = {
        let conn = db.lock().await;
        db::take_sync_queue(&conn)?
    };
    let mut failed: Vec<db::SyncEntry> = Vec::new();
    for entry in queue {
        let tag = if entry.field == "starred" {
            STARRED_TAG
        } else {
            READ_TAG
        };
        let action = if entry.value { "a" } else { "r" };
        let pushed = session
            .post_form(http, "edit-tag", vec![("i", entry.remote_id.as_str()), (action, tag)])
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .is_ok();
        if !pushed {
            failed.push(entry);
        }
    }
    if !failed.is_empty() {
        log::warn!("sync: {} change(s) failed to push, re-queued", failed.len());
        let conn = db.lock().await;
        for entry in &failed {
            let _ = db::requeue_sync(&conn, entry.article_id, &entry.field, entry.value);
        }
    }

    push_ops(db, http, &session).await;

    let (pending_unsubs, stale_folders): (HashSet<String>, HashSet<String>) = {
        let conn = db.lock().await;
        let subs = db::pending_op_targets(&conn, db::OP_UNSUBSCRIBE)?.into_iter().collect();
        let mut stale: HashSet<String> =
            db::pending_op_targets(&conn, db::OP_DELETE_FOLDER)?.into_iter().collect();
        stale.extend(db::pending_op_targets(&conn, db::OP_RENAME_FOLDER)?);
        (subs, stale)
    };
    let subs: SubList = session
        .get(http, "subscription/list?output=json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let mut server = ServerSubs {
        urls: HashSet::new(),
        folder: HashMap::new(),
        title: HashMap::new(),
        id_to_url: HashMap::new(),
    };
    for sub in &subs.subscriptions {
        let Some(url) = sub.url.as_deref().filter(|u| !u.is_empty()) else {
            continue;
        };
        if pending_unsubs.contains(url) {
            continue;
        }
        server.urls.insert(url.to_string());
        server
            .folder
            .insert(url.to_string(), sub.categories.iter().find_map(SubCat::folder_name));
        if let Some(title) = &sub.title {
            server.title.insert(url.to_string(), title.clone());
        }
        if let Some(id) = sub.id.as_deref().and_then(|i| i.strip_prefix("feed/")) {
            server.id_to_url.insert(id.to_string(), url.to_string());
        }
    }
    {
        let conn = db.lock().await;
        for sub in subs.subscriptions {
            let folder_id = sub
                .categories
                .iter()
                .find_map(SubCat::folder_name)
                .filter(|name| !stale_folders.contains(name))
                .map(|name| db::folder_id_by_name(&conn, &name))
                .transpose()?;
            let Some(feed_url) = sub.url.filter(|u| !u.is_empty()) else {
                continue;
            };
            if pending_unsubs.contains(&feed_url) {
                continue;
            }
            match db::find_feed_by_url(&conn, &feed_url)? {
                None => {
                    let title = sub.title.unwrap_or_else(|| feed_url.clone());
                    let st = parse::detect_source_type(&feed_url);
                    let _ = db::insert_feed(&conn, &feed_url, None, &title, None, st, folder_id);
                }
                Some(id) => {
                    if let Some(folder_id) = folder_id {
                        if db::feed_folder_id(&conn, id)?.is_none() {
                            db::move_feed(&conn, id, Some(folder_id))?;
                        }
                    }
                }
            }
        }
    }

    let local_feeds = {
        let conn = db.lock().await;
        db::feeds_for_sync(&conn)?
    };
    push_subscriptions(
        http,
        &session,
        &local_feeds,
        &server,
        &pending_unsubs,
        &stale_folders,
    )
    .await;

    pull_folders(db, http, &session, &stale_folders).await;
    pull_unread_counts(db, http, &session, &server.id_to_url).await;

    let unread = fetch_stream_items(&session, http, READING_LIST, Some(READ_TAG)).await?;
    let starred = fetch_stream_items(&session, http, STARRED_TAG, None).await?;

    let unread_urls: HashSet<String> = unread.iter().map(|i| i.url.clone()).collect();
    let starred_urls: HashSet<String> = starred.iter().map(|i| i.url.clone()).collect();

    let reconciled = {
        let conn = db.lock().await;
        for item in unread.iter().chain(starred.iter()) {
            if let Some(aid) = db::article_id_by_url(&conn, &item.url)? {
                db::set_remote_id(&conn, aid, &item.id)?;
            }
        }
        let server_feed_ids = db::feed_ids_by_urls(&conn, &server.urls)?;
        db::reconcile_sync_state(&conn, &server_feed_ids, &unread_urls, &starred_urls)?
    };
    Ok(reconciled)
}

// ─────────────────────────── Feedly flow ───────────────────────────

struct FeedlySession {
    base: String,
    token: String,
    user: String,
}

impl FeedlySession {
    fn get(&self, http: &Client, path: &str) -> RequestBuilder {
        http.get(format!("{}/v3/{path}", self.base))
            .bearer_auth(&self.token)
    }

    fn post(&self, http: &Client, path: &str) -> RequestBuilder {
        http.post(format!("{}/v3/{path}", self.base))
            .bearer_auth(&self.token)
    }

    fn put(&self, http: &Client, path: &str) -> RequestBuilder {
        http.put(format!("{}/v3/{path}", self.base))
            .bearer_auth(&self.token)
    }

    fn delete(&self, http: &Client, path: &str) -> RequestBuilder {
        http.delete(format!("{}/v3/{path}", self.base))
            .bearer_auth(&self.token)
    }
}

#[derive(Deserialize, Clone)]
struct FeedlyProfile {
    id: String,
}

#[derive(Deserialize, Clone)]
struct FeedlyCollection {
    id: String,
    label: String,
    #[serde(default)]
    feeds: Vec<FeedlyFeed>,
}

#[derive(Deserialize, Clone)]
struct FeedlyFeed {
    id: String,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Deserialize)]
struct FeedlyStream {
    #[serde(default)]
    items: Vec<FeedlyItem>,
    #[serde(default)]
    continuation: Option<String>,
}

#[derive(Deserialize)]
struct FeedlyItem {
    id: String,
    #[serde(default)]
    canonical: Vec<Href>,
    #[serde(default)]
    alternate: Vec<Href>,
}

async fn feedly_profile(http: &Client, token: &str) -> AppResult<FeedlyProfile> {
    let resp = http
        .get(format!("{FEEDLY_BASE}/v3/profile"))
        .bearer_auth(token)
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(AppError::code("feedlyLoginFailed"));
    }
    Ok(resp.json().await?)
}

async fn feedly_collections(
    session: &FeedlySession,
    http: &Client,
) -> AppResult<Vec<FeedlyCollection>> {
    let resp = session.get(http, "collections").send().await?;
    if !resp.status().is_success() {
        return Err(AppError::code("feedlyLoginFailed"));
    }
    Ok(resp.json().await?)
}

fn feedly_collection_by_label<'a>(
    collections: &'a [FeedlyCollection],
    label: &str,
) -> Option<&'a FeedlyCollection> {
    collections
        .iter()
        .find(|c| c.label.eq_ignore_ascii_case(label))
}

async fn feedly_create_collection(
    session: &FeedlySession,
    http: &Client,
    label: &str,
) -> AppResult<FeedlyCollection> {
    let resp = session
        .post(http, "collections")
        .json(&json!({ "label": label }))
        .send()
        .await?
        .error_for_status()?;
    let value: Value = resp.json().await?;
    let object = value
        .as_array()
        .and_then(|a| a.first())
        .cloned()
        .or_else(|| value.as_object().cloned().map(Value::Object))
        .ok_or_else(|| AppError::code("feedlyLoginFailed"))?;
    serde_json::from_value(object).map_err(|e| AppError::other(e.to_string()))
}

async fn feedly_ensure_collection(
    session: &FeedlySession,
    http: &Client,
    collections: &mut Vec<FeedlyCollection>,
    label: &str,
) -> AppResult<String> {
    if let Some(c) = feedly_collection_by_label(collections, label) {
        return Ok(c.id.clone());
    }
    let created = feedly_create_collection(session, http, label).await?;
    let id = created.id.clone();
    collections.push(created);
    Ok(id)
}

async fn feedly_mark_entries(
    session: &FeedlySession,
    http: &Client,
    action: &str,
    ids: &[String],
) -> bool {
    session
        .post(http, "markers")
        .json(&json!({
            "type": "entries",
            "action": action,
            "entryIds": ids,
        }))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .is_ok()
}

fn feedly_marker_action(field: &str, value: bool) -> &'static str {
    match (field, value) {
        ("starred", true) => "markAsSaved",
        ("starred", false) => "markAsUnsaved",
        (_, true) => "markAsRead",
        (_, false) => "keepUnread",
    }
}

async fn push_feedly_queue(db: &Db, http: &Client, session: &FeedlySession) {
    let queue = {
        let conn = db.lock().await;
        match db::take_sync_queue(&conn) {
            Ok(q) => q,
            Err(e) => {
                log::warn!("sync: could not read pending article changes: {e}");
                return;
            }
        }
    };
    let mut groups: HashMap<(&'static str, bool), Vec<db::SyncEntry>> = HashMap::new();
    for entry in queue {
        groups
            .entry((feedly_marker_action(&entry.field, entry.value), entry.value))
            .or_default()
            .push(entry);
    }
    for ((action, _), entries) in groups {
        for chunk in entries.chunks(1000) {
            let ids: Vec<String> = chunk.iter().map(|e| e.remote_id.clone()).collect();
            if !feedly_mark_entries(session, http, action, &ids).await {
                let conn = db.lock().await;
                for entry in chunk {
                    let _ = db::requeue_sync(&conn, entry.article_id, &entry.field, entry.value);
                }
            }
        }
    }
}

fn feedly_stream_target(
    session: &FeedlySession,
    target: &str,
    collections: &[FeedlyCollection],
) -> Option<(String, &'static str)> {
    if target == READING_LIST {
        return Some((format!("user/{}/category/global.all", session.user), "categories"));
    }
    if target == STARRED_TAG {
        return None;
    }
    if let Some(name) = target.strip_prefix(LABEL_PREFIX) {
        return feedly_collection_by_label(collections, name)
            .map(|c| (c.id.clone(), "categories"));
    }
    if target.starts_with("feed/") {
        return Some((target.to_string(), "feeds"));
    }
    None
}

async fn feedly_mark_stream(
    session: &FeedlySession,
    http: &Client,
    target: &str,
    collections: &[FeedlyCollection],
    as_of: i64,
) -> bool {
    let Some((stream, kind)) = feedly_stream_target(session, target, collections) else {
        return true;
    };
    let body = if kind == "feeds" {
        json!({ "type": "feeds", "action": "markAsRead", "feedIds": [stream], "asOf": as_of })
    } else {
        json!({ "type": "categories", "action": "markAsRead", "categoryIds": [stream], "asOf": as_of })
    };
    session
        .post(http, "markers")
        .json(&body)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .is_ok()
}

async fn push_feedly_ops(
    db: &Db,
    http: &Client,
    session: &FeedlySession,
    collections: &[FeedlyCollection],
) {
    let ops = {
        let conn = db.lock().await;
        match db::list_ops(&conn) {
            Ok(ops) => ops,
            Err(e) => {
                log::warn!("sync: could not read pending ops: {e}");
                return;
            }
        }
    };
    for op in ops {
        let ok = match op.kind.as_str() {
            db::OP_UNSUBSCRIBE => {
                let id = feedly_feed_id(&op.target);
                session
                    .delete(http, &format!("subscriptions/{}", encode_path(&id)))
                    .send()
                    .await
                    .map(|r| r.status().is_success() || r.status() == reqwest::StatusCode::NOT_FOUND)
                    .unwrap_or(false)
            }
            db::OP_RENAME_FOLDER => match feedly_collection_by_label(collections, &op.target) {
                Some(collection) if !collection.id.is_empty() => session
                    .post(http, "collections")
                    .json(&json!({ "id": collection.id, "label": op.value.as_deref().unwrap_or_default() }))
                    .send()
                    .await
                    .and_then(|r| r.error_for_status())
                    .is_ok(),
                _ => true,
            },
            db::OP_DELETE_FOLDER => match feedly_collection_by_label(collections, &op.target) {
                Some(collection) if !collection.id.is_empty() => session
                    .delete(http, &format!("collections/{}", encode_path(&collection.id)))
                    .send()
                    .await
                    .and_then(|r| r.error_for_status())
                    .is_ok(),
                _ => true,
            },
            db::OP_MARK_ALL_READ => {
                // The shared queue stores GReader's nanosecond cutoff; Feedly expects milliseconds.
                let as_of = op.value.as_deref()
                    .and_then(|v| v.parse::<i64>().ok())
                    .map(|ts| ts / 1_000_000)
                    .unwrap_or_else(|| now_secs() * 1000);
                feedly_mark_stream(session, http, &op.target, collections, as_of).await
            }
            other => {
                log::warn!("sync: dropping unknown op kind {other}");
                true
            }
        };
        if ok {
            let conn = db.lock().await;
            if let Err(e) = db::delete_op(&conn, op.id) {
                log::warn!("sync: could not clear op #{}: {e}", op.id);
            }
        } else {
            log::warn!("sync: op #{} ({}) failed, will retry", op.id, op.kind);
        }
    }
}

struct FeedlyServerSubs {
    urls: HashSet<String>,
    folder: HashMap<String, Option<String>>,
    folder_id: HashMap<String, String>,
    title: HashMap<String, String>,
    id_to_url: HashMap<String, String>,
}

async fn push_feedly_subscriptions(
    session: &FeedlySession,
    http: &Client,
    local: &[db::SyncFeed],
    server: &FeedlyServerSubs,
    collections: &mut Vec<FeedlyCollection>,
    skip: &HashSet<String>,
    stale_folders: &HashSet<String>,
) {
    for feed in local {
        if skip.contains(&feed.url) {
            continue;
        }
        let target_label = feed.folder.as_deref().unwrap_or(FEEDLY_UNFILED_LABEL);
        if stale_folders.contains(target_label) {
            continue;
        }
        let target_id = match feedly_ensure_collection(session, http, collections, target_label).await {
            Ok(id) => id,
            Err(e) => {
                log::warn!("sync: could not prepare Feedly collection {target_label}: {e}");
                continue;
            }
        };
        let feed_id = feedly_feed_id(&feed.url);
        let current_id = server.folder_id.get(&feed.url);
        if !server.urls.contains(&feed.url) || current_id != Some(&target_id) {
            if let Err(e) = session
                .put(http, &format!("collections/{}/feeds", encode_path(&target_id)))
                .json(&json!({ "id": feed_id, "title": feed.title }))
                .send()
                .await
                .and_then(|r| r.error_for_status())
            {
                log::warn!("sync: failed to add {} to Feedly collection: {e}", feed.url);
                continue;
            }
            if let Some(old_id) = current_id.filter(|id| *id != &target_id) {
                let _ = session
                    .delete(http, &format!("collections/{}/feeds/.mdelete", encode_path(old_id)))
                    .json(&json!([{ "id": feed_id }]))
                    .send()
                    .await
                    .and_then(|r| r.error_for_status());
            }
        } else if feed.custom_title {
            if let Err(e) = session
                .put(http, &format!("collections/{}/feeds", encode_path(&target_id)))
                .json(&json!({ "id": feed_id, "title": feed.title }))
                .send()
                .await
                .and_then(|r| r.error_for_status())
            {
                log::warn!("sync: failed to rename Feedly feed {}: {e}", feed.url);
            }
        }
    }
}

async fn fetch_feedly_items(
    session: &FeedlySession,
    http: &Client,
    stream: &str,
    unread_only: bool,
) -> AppResult<Vec<RemoteItem>> {
    const PAGE: &str = "1000";
    const MAX_PAGES: usize = 30;
    let mut out = Vec::new();
    let mut continuation: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut params = vec![("streamId", stream), ("count", PAGE)];
        if unread_only {
            params.push(("unreadOnly", "true"));
        }
        if let Some(c) = continuation.as_deref() {
            params.push(("continuation", c));
        }
        let page: FeedlyStream = session
            .get(http, "streams/contents")
            .query(&params)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        for item in page.items {
            if let Some(url) = item
                .canonical
                .first()
                .or_else(|| item.alternate.first())
                .map(|h| h.href.clone())
            {
                out.push(RemoteItem { id: item.id, url });
            }
        }
        match page.continuation {
            Some(c) if !c.is_empty() => continuation = Some(c),
            _ => break,
        }
    }
    Ok(out)
}

async fn pull_feedly_unread_counts(
    db: &Db,
    http: &Client,
    session: &FeedlySession,
    id_to_url: &HashMap<String, String>,
) {
    let resp = match session.get(http, "markers/counts").send().await {
        Ok(resp) if resp.status().is_success() => resp,
        Ok(resp) => {
            log::warn!("sync: Feedly marker counts unavailable: {}", resp.status());
            return;
        }
        Err(e) => {
            log::warn!("sync: Feedly marker counts unavailable: {e}");
            return;
        }
    };
    let body: UnreadCounts = match resp.json().await {
        Ok(body) => body,
        Err(e) => {
            log::warn!("sync: Feedly marker counts decode failed: {e}");
            return;
        }
    };
    let conn = db.lock().await;
    let mut counts: HashMap<i64, i64> = HashMap::new();
    for entry in &body.unreadcounts {
        let Some(server_id) = entry.id.strip_prefix("feed/") else {
            continue;
        };
        let Some(url) = id_to_url.get(server_id) else {
            continue;
        };
        if let Ok(Some(feed_id)) = db::find_feed_by_url(&conn, url) {
            counts.insert(feed_id, entry.count);
        }
    }
    if let Err(e) = db::apply_server_unread(&conn, &counts) {
        log::warn!("sync: could not cache Feedly unread counts: {e}");
    }
}

async fn sync_feedly_now(db: &Db, http: &Client, creds: Creds) -> AppResult<usize> {
    let auth = creds
        .auth
        .clone()
        .ok_or_else(|| AppError::code("freshrssNotConnected"))?;
    let session = FeedlySession {
        base: feedly_base(&creds.url),
        token: auth,
        user: creds.user.clone(),
    };

    push_feedly_queue(db, http, &session).await;

    let mut collections = feedly_collections(&session, http).await?;
    push_feedly_ops(db, http, &session, &collections).await;
    // Folder operations change collection IDs and labels used below.
    collections = feedly_collections(&session, http).await?;

    let (pending_unsubs, stale_folders): (HashSet<String>, HashSet<String>) = {
        let conn = db.lock().await;
        let subs = db::pending_op_targets(&conn, db::OP_UNSUBSCRIBE)?.into_iter().collect();
        let mut stale: HashSet<String> =
            db::pending_op_targets(&conn, db::OP_DELETE_FOLDER)?.into_iter().collect();
        stale.extend(db::pending_op_targets(&conn, db::OP_RENAME_FOLDER)?);
        (subs, stale)
    };

    let mut server = FeedlyServerSubs {
        urls: HashSet::new(),
        folder: HashMap::new(),
        folder_id: HashMap::new(),
        title: HashMap::new(),
        id_to_url: HashMap::new(),
    };
    for collection in &collections {
        let folder = if feedly_collection_is_unfiled(&collection.label) {
            None
        } else {
            Some(collection.label.clone())
        };
        for feed in &collection.feeds {
            let url = feedly_feed_url(&feed.id);
            if pending_unsubs.contains(&url) || server.urls.contains(&url) {
                continue;
            }
            server.urls.insert(url.clone());
            server.folder.insert(url.clone(), folder.clone());
            server.folder_id.insert(url.clone(), collection.id.clone());
            if let Some(title) = &feed.title {
                server.title.insert(url.clone(), title.clone());
            }
            server.id_to_url.insert(feedly_feed_url(&feed.id), url);
        }
    }

    {
        let conn = db.lock().await;
        for collection in &collections {
            let folder_id = if feedly_collection_is_unfiled(&collection.label) {
                None
            } else {
                Some(db::folder_id_by_name(&conn, &collection.label)?)
            };
            for feed in &collection.feeds {
                let feed_url = feedly_feed_url(&feed.id);
                if pending_unsubs.contains(&feed_url) {
                    continue;
                }
                match db::find_feed_by_url(&conn, &feed_url)? {
                    None => {
                        let title = feed.title.clone().unwrap_or_else(|| feed_url.clone());
                        let st = parse::detect_source_type(&feed_url);
                        let _ = db::insert_feed(&conn, &feed_url, None, &title, None, st, folder_id);
                    }
                    Some(id) => {
                        if let Some(folder_id) = folder_id {
                            if db::feed_folder_id(&conn, id)?.is_none() {
                                db::move_feed(&conn, id, Some(folder_id))?;
                            }
                        }
                    }
                }
            }
        }
    }

    let local_feeds = {
        let conn = db.lock().await;
        db::feeds_for_sync(&conn)?
    };
    push_feedly_subscriptions(
        &session,
        http,
        &local_feeds,
        &server,
        &mut collections,
        &pending_unsubs,
        &stale_folders,
    )
    .await;

    {
        let conn = db.lock().await;
        for collection in &collections {
            if !feedly_collection_is_unfiled(&collection.label)
                && !stale_folders.contains(&collection.label)
            {
                if let Err(e) = db::folder_id_by_name(&conn, &collection.label) {
                    log::warn!("sync: could not create Feedly folder {}: {e}", collection.label);
                }
            }
        }
    }

    pull_feedly_unread_counts(db, http, &session, &server.id_to_url).await;

    let all_stream = format!("user/{}/category/global.all", session.user);
    let saved_stream = format!("user/{}/tag/global.saved", session.user);
    let unread = fetch_feedly_items(&session, http, &all_stream, true).await?;
    let starred = fetch_feedly_items(&session, http, &saved_stream, false).await?;

    let unread_urls: HashSet<String> = unread.iter().map(|i| i.url.clone()).collect();
    let starred_urls: HashSet<String> = starred.iter().map(|i| i.url.clone()).collect();

    let reconciled = {
        let conn = db.lock().await;
        for item in unread.iter().chain(starred.iter()) {
            if let Some(aid) = db::article_id_by_url(&conn, &item.url)? {
                db::set_remote_id(&conn, aid, &item.id)?;
            }
        }
        let server_feed_ids = db::feed_ids_by_urls(&conn, &server.urls)?;
        db::reconcile_sync_state(&conn, &server_feed_ids, &unread_urls, &starred_urls)?
    };
    Ok(reconciled)
}

pub async fn sync_now(db: &Db, http: &Client) -> AppResult<usize> {
    let creds = creds(db)
        .await?
        .ok_or_else(|| AppError::code("freshrssNotConnected"))?;
    match creds.provider {
        Provider::Feedly => sync_feedly_now(db, http, creds).await,
        _ => sync_greader_now(db, http, creds).await,
    }
}

// ─────────────────────────── Inoreader OAuth ───────────────────────────

#[derive(Deserialize, Serialize, Clone)]
pub struct OAuthTokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

pub fn inoreader_auth_url(client_id: &str, state: &str) -> String {
    let mut url = url::Url::parse(&format!("{INOREADER_BASE}/oauth2/auth"))
        .expect("Inoreader auth URL is valid");
    url.query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", INOREADER_REDIRECT_URI)
        .append_pair("response_type", "code")
        .append_pair("scope", "read write")
        .append_pair("state", state);
    url.to_string()
}

pub async fn exchange_inoreader_code(
    http: &Client,
    client_id: &str,
    client_secret: &str,
    code: &str,
) -> AppResult<OAuthTokens> {
    let resp = http
        .post(format!("{INOREADER_BASE}/oauth2/token"))
        .form(&[
            ("code", code),
            ("redirect_uri", INOREADER_REDIRECT_URI),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(AppError::code("inoreaderOauthFailed"));
    }
    Ok(resp.json().await?)
}

async fn inoreader_user_id(http: &Client, token: &str) -> AppResult<String> {
    let resp = http
        .get(format!("{INOREADER_BASE}/reader/api/0/user-info"))
        .bearer_auth(token)
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(AppError::code("inoreaderOauthFailed"));
    }
    let value: Value = resp.json().await?;
    value
        .get("userId")
        .and_then(Value::as_str)
        .or_else(|| value.get("id").and_then(Value::as_str))
        .or_else(|| value.get("userProfileId").and_then(Value::as_str))
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::code("inoreaderOauthFailed"))
}

pub async fn finish_inoreader_oauth(
    db: &Db,
    http: &Client,
    tokens: OAuthTokens,
    client_id: &str,
    client_secret: &str,
) -> AppResult<()> {
    let user = inoreader_user_id(http, &tokens.access_token).await?;
    let expires_at = now_secs() + tokens.expires_in.unwrap_or(3600);
    persist_connection(
        db,
        INOREADER_BASE,
        &user,
        &tokens.access_token,
        Provider::Inoreader,
        tokens.refresh_token.as_deref(),
        Some(client_id),
        Some(client_secret),
        Some(expires_at),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn mock_response(status: &str, body: &str) -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0; 4096];
                let n = socket.read(&mut chunk).await.unwrap();
                assert!(n > 0);
                request.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&request);
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text[..end].lines().find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    }).unwrap_or(0);
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            socket.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8(request).unwrap()
        });
        (base, task)
    }

    #[tokio::test]
    async fn feedly_mark_all_read_sends_millisecond_cutoff() {
        let (base, request) = mock_response("200 OK", "{}").await;
        let session = FeedlySession { base, token: "test-token".into(), user: "u1".into() };
        let db = Mutex::new(db::open(std::path::Path::new(":memory:")).unwrap());
        {
            let conn = db.lock().await;
            db::enqueue_op(&conn, db::OP_MARK_ALL_READ, READING_LIST, Some("1700000000000000000")).unwrap();
        }
        push_feedly_ops(&db, &Client::new(), &session, &[]).await;
        assert!(db::list_ops(&*db.lock().await).unwrap().is_empty());
        let request = request.await.unwrap();
        assert!(request.starts_with("POST /v3/markers "));
        assert!(request.to_ascii_lowercase().contains("authorization: bearer test-token"));
        let body: Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["asOf"], 1_700_000_000_000_i64);
        assert_eq!(body["categoryIds"], json!(["user/u1/category/global.all"]));
    }

    #[tokio::test]
    async fn feedly_sync_stops_when_collections_cannot_be_loaded() {
        let (base, request) = mock_response("503 Service Unavailable", "{}").await;
        let db = Mutex::new(db::open(std::path::Path::new(":memory:")).unwrap());
        let creds = Creds {
            url: base, user: "u1".into(), auth: Some("test-token".into()),
            legacy_pass: None, provider: Provider::Feedly, refresh_token: None,
            client_id: None, client_secret: None, token_expires_at: None,
        };
        assert!(sync_feedly_now(&db, &Client::new(), creds).await.is_err());
        assert!(request.await.unwrap().starts_with("GET /v3/collections "));
    }

    fn cat(id: &str, label: Option<&str>) -> SubCat {
        SubCat {
            id: id.to_string(),
            label: label.map(str::to_string),
        }
    }

    #[test]
    fn folder_name_prefers_label() {
        assert_eq!(
            cat("user/-/label/Tech", Some("Tech")).folder_name().as_deref(),
            Some("Tech")
        );
    }

    #[test]
    fn folder_name_falls_back_to_label_id() {
        assert_eq!(
            cat("user/-/label/科技", None).folder_name().as_deref(),
            Some("科技")
        );
    }

    #[test]
    fn folder_name_skips_unnamed_categories() {
        assert_eq!(cat("user/-/state/com.google/read", None).folder_name(), None);
        assert_eq!(cat("", Some("   ")).folder_name(), None);
    }

    #[test]
    fn folder_name_skips_freshrss_uncategorized() {
        assert_eq!(
            cat("user/-/label/Uncategorized", Some("Uncategorized")).folder_name(),
            None
        );
        assert_eq!(cat("user/-/label/uncategorized", None).folder_name(), None);
    }

    #[test]
    fn folder_change_none_when_sides_agree() {
        assert_eq!(folder_change(None, None), None);
        assert_eq!(folder_change(Some("Tech"), Some("Tech")), None);
    }

    #[test]
    fn folder_change_adds_new_label_local_wins() {
        assert_eq!(
            folder_change(Some("Tech"), Some("News")),
            Some((Some(label_stream("Tech")), Some(label_stream("News"))))
        );
        assert_eq!(
            folder_change(Some("Tech"), None),
            Some((Some(label_stream("Tech")), None))
        );
    }

    #[test]
    fn tag_folder_name_only_accepts_labels() {
        let folder = Tag { id: label_stream("Tech"), kind: Some("folder".into()) };
        assert_eq!(folder.folder_name().as_deref(), Some("Tech"));
        let article_tag = Tag { id: label_stream("Later"), kind: Some("tag".into()) };
        assert_eq!(article_tag.folder_name(), None);
        let state = Tag { id: READ_TAG.into(), kind: None };
        assert_eq!(state.folder_name(), None);
        let uncat = Tag { id: label_stream("Uncategorized"), kind: Some("folder".into()) };
        assert_eq!(uncat.folder_name(), None);
    }

    #[test]
    fn provider_setting_round_trips_all_services() {
        for (raw, expected) in [
            ("freshrss", Provider::FreshRss),
            ("miniflux", Provider::Miniflux),
            ("inoreader", Provider::Inoreader),
            ("feedly", Provider::Feedly),
            ("unknown", Provider::FreshRss),
        ] {
            assert_eq!(Provider::from_setting(Some(raw)), expected);
            assert_eq!(Provider::from_setting(Some(expected.as_str())), expected);
        }
    }

    #[test]
    fn greader_base_is_idempotent() {
        assert_eq!(
            greader_base("https://rss.example", Provider::FreshRss),
            "https://rss.example/api/greader.php"
        );
        assert_eq!(
            greader_base("https://rss.example/api/greader.php/", Provider::FreshRss),
            "https://rss.example/api/greader.php"
        );
        assert_eq!(greader_base("https://rss.example", Provider::Miniflux), "https://rss.example");
        assert_eq!(greader_base("ignored", Provider::Inoreader), INOREADER_BASE);
    }

    #[test]
    fn feedly_feed_ids_map_to_urls() {
        let url = "https://example.com/feed.xml";
        assert_eq!(feedly_feed_id(url), "feed/https://example.com/feed.xml");
        assert_eq!(feedly_feed_url("feed/https://example.com/feed.xml"), url);
        assert_eq!(feedly_feed_url("https://example.com/feed.xml"), url);
    }

    #[test]
    fn feedly_marker_actions_cover_read_and_saved_state() {
        assert_eq!(feedly_marker_action("read", true), "markAsRead");
        assert_eq!(feedly_marker_action("read", false), "keepUnread");
        assert_eq!(feedly_marker_action("starred", true), "markAsSaved");
        assert_eq!(feedly_marker_action("starred", false), "markAsUnsaved");
    }

    #[test]
    fn feedly_stream_target_maps_global_and_feed_streams() {
        let session = FeedlySession {
            base: FEEDLY_BASE.into(),
            token: "token".into(),
            user: "user-1".into(),
        };
        let collections = vec![FeedlyCollection {
            id: "user/user-1/category/Tech".into(),
            label: "Tech".into(),
            feeds: Vec::new(),
        }];
        assert_eq!(
            feedly_stream_target(&session, READING_LIST, &collections),
            Some(("user/user-1/category/global.all".into(), "categories"))
        );
        assert_eq!(
            feedly_stream_target(&session, &label_stream("Tech"), &collections),
            Some(("user/user-1/category/Tech".into(), "categories"))
        );
        assert_eq!(
            feedly_stream_target(&session, "feed/https://example.com/rss", &collections),
            Some(("feed/https://example.com/rss".into(), "feeds"))
        );
    }

    #[test]
    fn inoreader_auth_url_encodes_redirect_and_scope() {
        let url = inoreader_auth_url("client 1", "state-1");
        assert!(url.contains("client_id=client+1"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A19812%2Foauth%2Fcallback"));
        assert!(url.contains("scope=read+write"));
        assert!(url.contains("state=state-1"));
    }
}
