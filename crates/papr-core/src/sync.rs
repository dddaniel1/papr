//! Synchronisation over the Google Reader compatible API.
//!
//! Supports any GReader-compatible backend; today FreshRSS and Miniflux.
//! Protocol is identical (`ClientLogin`, `reader/api/0/edit-tag`,
//! `stream/contents/...`, `com.google/*` tags) — only the API root path
//! differs per provider, so the `Provider` enum centralises that mapping.
//!
//! Flow: `ClientLogin` for an auth token, push any queued local read/starred
//! changes via `edit-tag`, drain queued feed-/folder-level operations
//! (`subscription/edit` unsubscribe, `rename-tag`, `disable-tag`,
//! `mark-all-as-read`), pull the subscription list and the folder (`tag/list`)
//! list, converge the two subscription lists (subscribe local-only feeds, file
//! feeds into folders, push hand-set titles), then pull the recent reading-list
//! (to reconcile read/starred state, matched to local articles by URL).

use crate::db;
use crate::error::{AppError, AppResult};
use crate::ingestion::parse;
use reqwest::{Client, RequestBuilder};
use rusqlite::Connection;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use tokio::sync::Mutex;

/// The writer connection behind an async mutex — what every sync function reads
/// and writes through. The desktop app passes `state.db`; the CLI passes the
/// connection it opened. Paired with a shared [`Client`] for the HTTP calls.
type Db = Mutex<Connection>;

const READ_TAG: &str = "user/-/state/com.google/read";
const STARRED_TAG: &str = "user/-/state/com.google/starred";
const READING_LIST: &str = "user/-/state/com.google/reading-list";
/// GReader "label" stream prefix. Folders are labels server-side, so a feed is
/// filed by adding/removing `user/-/label/NAME` on its subscription.
const LABEL_PREFIX: &str = "user/-/label/";

/// The GReader label stream id for a folder name.
fn label_stream(name: &str) -> String {
    format!("{LABEL_PREFIX}{name}")
}

/// The `(add, remove)` label pair a folder change needs, or `None` when both
/// sides already agree. `local` wins on conflict — a feed the user has filed by
/// hand is not yanked back to the server's folder. Adding the new label is
/// enough for FreshRSS; the matching removal keeps other GReader servers from
/// leaving the feed filed under both.
fn folder_change(
    local: Option<&str>,
    server: Option<&str>,
) -> Option<(Option<String>, Option<String>)> {
    if local == server {
        return None;
    }
    Some((local.map(label_stream), server.map(label_stream)))
}

/// Which GReader-compatible backend the user is connected to. The wire
/// protocol is identical; only where the API root sits under the server URL
/// differs (FreshRSS mounts it at `/api/greader.php`, Miniflux serves it at
/// the server root).
#[derive(Clone, Copy)]
enum Provider {
    FreshRss,
    Miniflux,
}

impl Provider {
    /// Path segment to append to the user-supplied server URL to reach the
    /// GReader API root. Miniflux serves `/accounts/ClientLogin` and
    /// `/reader/api/0/...` straight off the server root, so its suffix is
    /// empty.
    fn path_suffix(self) -> &'static str {
        match self {
            Provider::FreshRss => "/api/greader.php",
            Provider::Miniflux => "",
        }
    }

    /// Parse the persisted setting. Missing / unknown → FreshRss, so older
    /// installs (where this setting didn't exist) keep working unchanged.
    fn from_setting(s: Option<&str>) -> Self {
        match s.unwrap_or("").trim() {
            "miniflux" => Provider::Miniflux,
            _ => Provider::FreshRss,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Provider::FreshRss => "freshrss",
            Provider::Miniflux => "miniflux",
        }
    }
}

/// Normalise a user-supplied server URL to its GReader API root for the
/// chosen provider. Idempotent: if the user already typed the full path,
/// don't append it again.
fn greader_base(url: &str, provider: Provider) -> String {
    let t = url.trim().trim_end_matches('/');
    let suffix = provider.path_suffix();
    if t.ends_with(suffix) || t.contains(&format!("{suffix}/")) {
        t.to_string()
    } else {
        format!("{t}{suffix}")
    }
}

/// An authenticated FreshRSS session.
struct Session {
    base: String,
    auth: String,
    token: String,
}

impl Session {
    fn get(&self, http: &Client, path: &str) -> RequestBuilder {
        http.get(format!("{}/reader/api/0/{path}", self.base))
            .header("Authorization", format!("GoogleLogin auth={}", self.auth))
    }
    fn post(&self, http: &Client, path: &str) -> RequestBuilder {
        http.post(format!("{}/reader/api/0/{path}", self.base))
            .header("Authorization", format!("GoogleLogin auth={}", self.auth))
    }
}

/// Exchange username + password for a long-lived auth token.
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

/// Build a session from an existing auth token by fetching a fresh write
/// (edit-tag) token. Fails fast if the auth token is no longer valid.
async fn session_with_token(http: &Client, base: &str, auth: String) -> AppResult<Session> {
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
        token,
    })
}

/// Log in with username + password and obtain a full session.
async fn login(http: &Client, base: &str, user: &str, pass: &str) -> AppResult<Session> {
    let auth = client_login(http, base, user, pass).await?;
    session_with_token(http, base, auth).await
}

#[derive(Deserialize)]
struct SubList {
    #[serde(default)]
    subscriptions: Vec<Sub>,
}
#[derive(Deserialize)]
struct Sub {
    /// Server-side subscription id (`feed/<n>`), shared with `unread-count` so
    /// the two responses can be joined.
    #[serde(default)]
    id: Option<String>,
    url: Option<String>,
    title: Option<String>,
    #[serde(default)]
    categories: Vec<SubCat>,
}
/// A GReader category ("label") a subscription belongs to. FreshRSS/Miniflux
/// folders surface here; we map the first named one onto a local folder.
#[derive(Deserialize)]
struct SubCat {
    #[serde(default)]
    id: String,
    #[serde(default)]
    label: Option<String>,
}
impl SubCat {
    /// Human folder name for this category. Prefer the explicit `label`,
    /// otherwise derive it from the `user/-/label/NAME` id. `None` for an
    /// unnamed category, so it is skipped rather than creating a blank folder.
    ///
    /// FreshRSS files every feed the user hasn't categorised under a built-in
    /// "Uncategorized" label. That isn't a real folder — mapping it onto a
    /// local one buries every top-level feed in a junk folder that doesn't
    /// match the server's own presentation — so it is treated as no folder.
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
    // Present when the stream has more pages; fed back as `c=` to page on.
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

/// `tag/list` response. FreshRSS returns folders as `user/-/label/*` entries
/// tagged `type: "folder"` and article tags as `type: "tag"`; older/Miniflux
/// servers may omit `type`, so it is optional.
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
    /// The folder name this tag denotes, or `None` for anything that isn't a
    /// real folder. A `type` of "tag" (a FreshRSS article label) or the built-in
    /// "Uncategorized" bucket is skipped, matching `SubCat::folder_name`.
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

/// `unread-count` response: one entry per feed (`feed/<n>`), per label, and a
/// `reading-list` total. We only need the per-feed counts, joined to local
/// feeds via the subscription ids.
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

/// Stored GReader connection. We persist the long-lived auth token rather
/// than the password — a leaked token is revocable server-side and can't be
/// replayed against the user's other accounts. `legacy_pass` holds a
/// plaintext password from an older install, awaiting one-time migration.
struct Creds {
    url: String,
    user: String,
    auth: Option<String>,
    legacy_pass: Option<String>,
    provider: Provider,
}

/// Stored GReader credentials, if a server is configured. The setting keys
/// are still named `freshrss_*` for backwards compatibility with installs
/// that predate multi-provider support — the values are provider-agnostic.
async fn creds(db: &Db) -> AppResult<Option<Creds>> {
    let conn = db.lock().await;
    let url = db::get_setting(&conn, "freshrss_url")?.unwrap_or_default();
    let user = db::get_setting(&conn, "freshrss_user")?.unwrap_or_default();
    let nonempty = |k| db::get_setting(&conn, k).map(|v| v.filter(|s| !s.is_empty()));
    let auth = nonempty("freshrss_auth")?;
    let legacy_pass = nonempty("freshrss_pass")?;
    let provider = Provider::from_setting(
        db::get_setting(&conn, "freshrss_provider")?.as_deref(),
    );
    if url.trim().is_empty() || user.is_empty() || (auth.is_none() && legacy_pass.is_none()) {
        return Ok(None);
    }
    Ok(Some(Creds { url, user, auth, legacy_pass, provider }))
}

/// The configured GReader server URL and provider, or `None` when not
/// connected.
pub async fn connected_url(db: &Db) -> AppResult<Option<(String, String)>> {
    Ok(creds(db).await?.map(|c| (c.url, c.provider.as_str().to_string())))
}

/// Persist a verified connection, storing the auth token and never the
/// password (any legacy stored password is also cleared).
async fn persist_session(
    db: &Db,
    url: &str,
    user: &str,
    auth: &str,
    provider: Provider,
) -> AppResult<()> {
    let conn = db.lock().await;
    db::set_setting(&conn, "freshrss_url", url.trim())?;
    db::set_setting(&conn, "freshrss_user", user)?;
    db::set_setting(&conn, "freshrss_auth", auth)?;
    db::set_setting(&conn, "freshrss_pass", "")?;
    db::set_setting(&conn, "freshrss_provider", provider.as_str())?;
    Ok(())
}

/// Verify credentials against the server and, on success, persist them.
pub async fn connect(
    db: &Db,
    http: &Client,
    url: &str,
    user: &str,
    pass: &str,
    provider: Option<&str>,
) -> AppResult<()> {
    let provider = Provider::from_setting(provider);
    let base = greader_base(url, provider);
    let session = login(http, &base, user, pass).await?; // verifies credentials
    persist_session(db, url, user, &session.auth, provider).await
}

/// Forget the stored GReader credentials.
pub async fn disconnect(db: &Db) -> AppResult<()> {
    let conn = db.lock().await;
    for key in [
        "freshrss_url",
        "freshrss_user",
        "freshrss_auth",
        "freshrss_pass",
        "freshrss_provider",
    ] {
        db::set_setting(&conn, key, "")?;
    }
    // Drop the cached server unread counts too — with no server there is no
    // authoritative figure, so the sidebar reverts to local counts.
    db::clear_server_unread(&conn)?;
    Ok(())
}

/// Run a full sync if a server is connected. Returns `true` when a sync
/// actually ran, so the caller can refresh the UI for the reconciled state.
pub async fn run_if_connected(db: &Db, http: &Client) -> AppResult<bool> {
    if creds(db).await?.is_some() {
        sync_now(db, http).await.map(|_| true)
    } else {
        Ok(false)
    }
}

/// Drain queued feed-/folder-level operations (unsubscribe, folder rename or
/// delete, bulk mark-read). Unlike the article queue these are destructive —
/// the local row they described is already gone — so a failed push is left in
/// `sync_ops` to retry on the next sync rather than dropped.
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
        let token = session.token.as_str();
        let ok = match op.kind.as_str() {
            db::OP_UNSUBSCRIBE => {
                let stream = format!("feed/{}", op.target);
                session
                    .post(http, "subscription/edit")
                    .form(&[("ac", "unsubscribe"), ("s", stream.as_str()), ("T", token)])
                    .send()
                    .await
                    .and_then(|r| r.error_for_status())
                    .is_ok()
            }
            db::OP_RENAME_FOLDER => {
                let from = label_stream(&op.target);
                let to = label_stream(op.value.as_deref().unwrap_or_default());
                session
                    .post(http, "rename-tag")
                    .form(&[("s", from.as_str()), ("dest", to.as_str()), ("T", token)])
                    .send()
                    .await
                    .and_then(|r| r.error_for_status())
                    .is_ok()
            }
            db::OP_DELETE_FOLDER => {
                let stream = label_stream(&op.target);
                session
                    .post(http, "disable-tag")
                    .form(&[("s", stream.as_str()), ("T", token)])
                    .send()
                    .await
                    .and_then(|r| r.error_for_status())
                    .is_ok()
            }
            db::OP_MARK_ALL_READ => {
                let ts = op.value.as_deref().unwrap_or("0");
                session
                    .post(http, "mark-all-as-read")
                    .form(&[("s", op.target.as_str()), ("ts", ts), ("T", token)])
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

/// The server's subscription view, built from `subscription/list` and diffed
/// against local state to decide what to push.
struct ServerSubs {
    urls: HashSet<String>,
    folder: HashMap<String, Option<String>>,
    title: HashMap<String, String>,
    /// Server subscription id (`feed/<n>`) → feed URL, to join the
    /// `unread-count` response (keyed by the same id) back onto local feeds.
    id_to_url: HashMap<String, String>,
}

/// Push the local subscription state onto the server: subscribe feeds the
/// server lacks, and — for feeds both sides carry — reconcile the folder label
/// and (when the user renamed it by hand) the title, local winning. `skip` is
/// the set of feed URLs with a pending unsubscribe whose push hasn't landed, so
/// a re-added feed isn't subscribed while its delete is still in flight.
/// `stale_folders` names folders with a queued rename/delete still live on the
/// server under that name; a feed still filed there is left alone rather than
/// fought.
async fn push_subscriptions(
    http: &Client,
    session: &Session,
    local: &[db::SyncFeed],
    server: &ServerSubs,
    skip: &HashSet<String>,
    stale_folders: &HashSet<String>,
) {
    let token = session.token.as_str();
    for feed in local {
        if skip.contains(&feed.url) {
            continue;
        }
        let stream = format!("feed/{}", feed.url);
        if !server.urls.contains(&feed.url) {
            // Local-only feed: subscribe it, filing it into its folder in the
            // same call when it has one (the server creates the label on demand).
            let label = feed.folder.as_deref().map(label_stream);
            let mut form = vec![("ac", "subscribe"), ("s", stream.as_str())];
            if let Some(label) = label.as_deref() {
                form.push(("a", label));
            }
            form.push(("T", token));
            if !session
                .post(http, "subscription/edit")
                .form(&form)
                .send()
                .await
                .and_then(|r| r.error_for_status())
                .is_ok()
            {
                log::warn!("sync: failed to subscribe server to {}", feed.url);
            }
            continue;
        }

        // Present on both sides: align folder membership. Skip a feed the server
        // still files under a folder whose rename/delete is queued — that op, not
        // a move, is what reconciles it.
        let server_folder = server.folder.get(&feed.url).cloned().flatten();
        if !server_folder.as_deref().is_some_and(|f| stale_folders.contains(f)) {
            if let Some((add, remove)) = folder_change(feed.folder.as_deref(), server_folder.as_deref())
            {
                let mut form = vec![("ac", "edit"), ("s", stream.as_str())];
                if let Some(add) = add.as_deref() {
                    form.push(("a", add));
                }
                if let Some(remove) = remove.as_deref() {
                    form.push(("r", remove));
                }
                form.push(("T", token));
                if !session
                    .post(http, "subscription/edit")
                    .form(&form)
                    .send()
                    .await
                    .and_then(|r| r.error_for_status())
                    .is_ok()
                {
                    log::warn!("sync: failed to file {} into its folder", feed.url);
                }
            }
        }

        // Align the title only when the user renamed the feed here, so a
        // server-side title the user never touched is left alone.
        if feed.custom_title {
            if let Some(server_title) = server.title.get(&feed.url) {
                if &feed.title != server_title
                    && !session
                        .post(http, "subscription/edit")
                        .form(&[
                            ("ac", "edit"),
                            ("s", stream.as_str()),
                            ("t", feed.title.as_str()),
                            ("T", token),
                        ])
                        .send()
                        .await
                        .and_then(|r| r.error_for_status())
                        .is_ok()
                {
                    log::warn!("sync: failed to rename {}", feed.url);
                }
            }
        }
    }
}

/// Pull the server's folder (label) list so folders that hold no feeds here yet
/// still appear locally. Best-effort: a server without `tag/list` (some Miniflux
/// builds) just skips it. `stale_folders` names folders with a pending local
/// delete/rename, still live on the server under that name, so the pull can't
/// resurrect one whose op hasn't reached the server.
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

/// Pull the server's authoritative per-feed unread counts and cache them on
/// `feeds.server_unread`, so the sidebar and badge show the server's figures
/// (which include items Papr hasn't fetched). The response is keyed by server
/// subscription id, joined to local feeds through `id_to_url`. Best-effort: a
/// server without `unread-count` (or a decode failure) leaves the local counts
/// in place. Feeds the server doesn't report have their cache cleared by
/// `apply_server_unread`.
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

/// A single article the server flags as unread or starred: its item id and the
/// canonical URL we match local articles on.
struct RemoteItem {
    id: String,
    url: String,
}

/// Fetch every item of a GReader stream, following `continuation` pages. `xt`,
/// when set, is an "exclude tag" filter (e.g. exclude read to get only unread).
///
/// Paging is capped: the sets we fetch this way (unread, starred) are bounded by
/// what the user hasn't yet read/has starred — normally a few hundred items — so
/// `MAX_PAGES` pages of `n=1000` (tens of thousands of items) is a generous
/// ceiling that also stops a pathological server from looping us forever.
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

/// Push queued changes, then pull subscriptions and read/starred state.
/// Returns the number of local articles whose state was reconciled.
pub async fn sync_now(db: &Db, http: &Client) -> AppResult<usize> {
    let creds = creds(db)
        .await?
        .ok_or_else(|| AppError::code("freshrssNotConnected"))?;
    let base = greader_base(&creds.url, creds.provider);
    let session = match &creds.auth {
        Some(auth) => session_with_token(http, &base, auth.clone()).await?,
        None => {
            // Legacy install: exchange the plaintext password for a token,
            // then migrate so the password is no longer kept on disk.
            let pass = creds.legacy_pass.as_deref().unwrap_or_default();
            let session = login(http, &base, &creds.user, pass).await?;
            persist_session(db, &creds.url, &creds.user, &session.auth, creds.provider).await?;
            session
        }
    };

    // 1 ── push: flush queued local read/starred changes. `take_sync_queue`
    // removes the rows up front, so any push that fails must be re-queued —
    // otherwise a network blip silently drops the user's change forever.
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
            .post(http, "edit-tag")
            .form(&[
                ("i", entry.remote_id.as_str()),
                (action, tag),
                ("T", session.token.as_str()),
            ])
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

    // 1b ── push queued feed-/folder-level operations (unsubscribe, folder
    // rename/delete, bulk mark-read). Draining before the pull means the
    // subscription list read next already reflects them.
    push_ops(db, http, &session).await;

    // 2 ── pull subscriptions: subscribe locally to any feed we don't have, and
    // adopt the server's folder for a feed not yet filed here. Feeds with a
    // pending unsubscribe are ignored so a delete that hasn't landed yet isn't
    // undone by this pull.
    let (pending_unsubs, stale_folders): (HashSet<String>, HashSet<String>) = {
        let conn = db.lock().await;
        let subs = db::pending_op_targets(&conn, db::OP_UNSUBSCRIBE)?.into_iter().collect();
        // A queued folder rename/delete leaves the old label live on the server;
        // treat it as stale everywhere so the pull doesn't recreate it and the
        // move-diff doesn't fight the queued op.
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

    // The server's view, used both for the local pull below and for the push
    // diff: per-URL folder and title, skipping feeds being unsubscribed.
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
        // `feed/<n>` → URL, so `unread-count` (keyed by the same id) can be
        // joined back to a local feed.
        if let Some(id) = sub.id.as_deref().and_then(|i| i.strip_prefix("feed/")) {
            server.id_to_url.insert(id.to_string(), url.to_string());
        }
    }
    {
        let conn = db.lock().await;
        for sub in subs.subscriptions {
            // Resolve the server-side folder (GReader "label") before moving
            // `url` out of `sub`, mapping it onto a local folder by name. A
            // stale (pending-rename/delete) label is ignored.
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
                // Reconcile the folder for a feed we already track, but only
                // when it isn't filed locally yet — don't yank a feed the user
                // has organised by hand back into the server's folder.
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

    // 2b ── push subscriptions: subscribe the server to any local feed it
    // doesn't have, file each feed into its folder and push a hand-set title,
    // so organising feeds in Papr propagates to the server instead of leaving
    // the two sides to drift. Best-effort and idempotent.
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

    // 2c ── pull the server's folder list so folders holding no feeds here yet
    // still show up. Best-effort: skipped if the server has no `tag/list`.
    pull_folders(db, http, &session, &stale_folders).await;

    // 2d ── pull the server's per-feed unread counts, making the sidebar and
    // badge server-authoritative. Best-effort.
    pull_unread_counts(db, http, &session, &server.id_to_url).await;

    // 3 ── reconcile read/starred state, treating the server as the source of
    // truth. Papr polls each subscribed feed's raw RSS independently and marks
    // every fetched item unread, so the local unread counts drift far above the
    // server's. Rather than pull the (unbounded) full history, fetch just the
    // server's *unread* and *starred* sets — both small — then mark every other
    // synced article read. This is what a pure GReader client effectively does,
    // and it fixes the runaway unread counts of issue #96 where the old
    // "reconcile the most recent 1000 items" pass never reached the long tail.
    let unread = fetch_stream_items(&session, http, READING_LIST, Some(READ_TAG)).await?;
    let starred = fetch_stream_items(&session, http, STARRED_TAG, None).await?;

    let unread_urls: std::collections::HashSet<String> =
        unread.iter().map(|i| i.url.clone()).collect();
    let starred_urls: std::collections::HashSet<String> =
        starred.iter().map(|i| i.url.clone()).collect();

    let reconciled = {
        let conn = db.lock().await;
        // Assign remote ids for the items we did fetch, so a later local edit on
        // one of them has an id to push. (Read articles in the long tail carry
        // no id until they next appear in an unread/starred response — pushing a
        // change on those waits, exactly as before this change.)
        for item in unread.iter().chain(starred.iter()) {
            if let Some(aid) = db::article_id_by_url(&conn, &item.url)? {
                db::set_remote_id(&conn, aid, &item.id)?;
            }
        }
        // Scope the read/starred sweep to feeds the server actually knows about,
        // so a local-only feed not yet mirrored server-side isn't wrongly marked
        // all-read. Articles with an un-pushed local edit are left untouched.
        let server_feed_ids = db::feed_ids_by_urls(&conn, &server.urls)?;
        db::reconcile_sync_state(&conn, &server_feed_ids, &unread_urls, &starred_urls)?
    };
    Ok(reconciled)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // Some servers omit the human label; derive it from the id instead.
        assert_eq!(
            cat("user/-/label/科技", None).folder_name().as_deref(),
            Some("科技")
        );
    }

    #[test]
    fn folder_name_skips_unnamed_categories() {
        // A state tag (not a label) or a blank label is not a folder.
        assert_eq!(cat("user/-/state/com.google/read", None).folder_name(), None);
        assert_eq!(cat("", Some("   ")).folder_name(), None);
    }

    #[test]
    fn folder_name_skips_freshrss_uncategorized() {
        // FreshRSS's built-in "Uncategorized" label is not a real folder, by
        // either label or id, and regardless of case.
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
        // A feed the user filed by hand is pushed even when the server had it
        // elsewhere or unfiled; a folder change clears the stale server label.
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

        // A FreshRSS article tag (type "tag") is not a folder.
        let article_tag = Tag { id: label_stream("Later"), kind: Some("tag".into()) };
        assert_eq!(article_tag.folder_name(), None);

        // System state tags and the built-in bucket are skipped.
        let state = Tag { id: READ_TAG.into(), kind: None };
        assert_eq!(state.folder_name(), None);
        let uncat = Tag { id: label_stream("Uncategorized"), kind: Some("folder".into()) };
        assert_eq!(uncat.folder_name(), None);
    }
}
