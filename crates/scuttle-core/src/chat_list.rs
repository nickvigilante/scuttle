//! The chat list: pages from `GET /chats`, their order, and the watch socket's merge rules.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use coder_sdk::{ChatStatus, types};
use uuid::Uuid;

use crate::forge::PrRef;

/// Chats asked for per page, which is also the server's default.
pub const PAGE_SIZE: i64 = 50;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Load {
    #[default]
    Idle,
    Loading,
    Loaded,
    Failed(String),
}

/// Which server list a page of chats belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ListQuery {
    /// Unarchived chats, the server's default.
    Default,
    Archived,
    /// A full-text search across titles, PR titles, and message bodies.
    Search(String),
}

impl ListQuery {
    /// The `q` parameter for `GET /chats`.
    pub fn q(&self) -> Option<String> {
        match self {
            ListQuery::Default => None,
            ListQuery::Archived => Some("archived:true".into()),
            ListQuery::Search(text) => Some(search_q(text)),
        }
    }
}

/// The keys `GET /chats` accepts in `q` (`Chats` in `coderd/searchquery/search.go`).
pub const SEARCH_KEYS: [&str; 11] = [
    "archived",
    "has_unread",
    "status",
    "pr_status",
    "diff_url",
    "title",
    "pr_title",
    "repo",
    "source",
    "pr",
    "search",
];

/// `text` split at whitespace outside double quotes, the way the server splits `q`.
fn terms(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = None;
    let mut quoted = false;
    for (i, c) in text.char_indices() {
        if c == '"' {
            quoted = !quoted;
        }
        if c.is_whitespace() && !quoted {
            if let Some(s) = start.take() {
                out.push(&text[s..i]);
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        out.push(&text[s..]);
    }
    out
}

/// Whether `term` is `key:value` with a value and a key the server accepts, in any case.
fn is_operator(term: &str) -> bool {
    term.split_once(':').is_some_and(|(key, value)| {
        !value.is_empty() && SEARCH_KEYS.contains(&key.to_ascii_lowercase().as_str())
    })
}

/// The `q` for a search typed as `text`: each operator term as typed, then the other words as
/// one `search:"…"` term, since the server refuses a bare word. Text with no operator is all
/// one search. A quote inside the searched words would end the value early, so it becomes a
/// space, and so does a quote left open, which would otherwise swallow every term after it.
fn search_q(text: &str) -> String {
    let text = &close_quotes(text);
    let (operators, words): (Vec<&str>, Vec<&str>) =
        terms(text).into_iter().partition(|t| is_operator(t));
    if operators.is_empty() {
        return format!("search:\"{}\"", text.replace('"', " "));
    }
    let mut out: Vec<String> = operators.into_iter().map(str::to_owned).collect();
    if !words.is_empty() {
        out.push(format!("search:\"{}\"", words.join(" ").replace('"', " ")));
    }
    out.join(" ")
}

/// `text` with its last quote made a space when that quote is left open.
fn close_quotes(text: &str) -> String {
    let mut text = text.to_owned();
    if text.matches('"').count() % 2 == 1
        && let Some(open) = text.rfind('"')
    {
        text.replace_range(open..=open, " ");
    }
    text
}

#[derive(Debug, Default)]
pub struct Page {
    pub chats: Vec<types::CodersdkChat>,
    pub load: Load,
    /// Whether the last page came back short, so there is nothing more to load.
    pub exhausted: bool,
}

#[derive(Debug, Default)]
pub struct ChatList {
    pub main: Page,
    pub archived: Page,
    /// The results of the last server search, with the text searched for.
    pub search: Option<(String, Page)>,
    /// Whether the watch socket is connected, so the list is live.
    pub watch_live: bool,
}

/// The tabs of `/chats`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    All,
    Active,
    Unread,
    Archived,
}

impl Filter {
    pub const ALL: [Filter; 4] = [
        Filter::All,
        Filter::Active,
        Filter::Unread,
        Filter::Archived,
    ];

    pub fn next(self) -> Filter {
        match self {
            Filter::All => Filter::Active,
            Filter::Active => Filter::Unread,
            Filter::Unread => Filter::Archived,
            Filter::Archived => Filter::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "all",
            Filter::Active => "active",
            Filter::Unread => "unread",
            Filter::Archived => "archived",
        }
    }

    /// The server list the tab draws from; active and unread filter the default list locally.
    pub fn query(self) -> ListQuery {
        match self {
            Filter::Archived => ListQuery::Archived,
            _ => ListQuery::Default,
        }
    }
}

/// A pull request's state as `/chats` shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    Open,
    Draft,
    Merged,
    Closed,
}

impl PrState {
    /// The state as a word, as text icons spell it out.
    pub fn label(self) -> &'static str {
        match self {
            PrState::Open => "open",
            PrState::Draft => "draft",
            PrState::Merged => "merged",
            PrState::Closed => "closed",
        }
    }
}

/// The pull request attached to a chat: its number, when the server knows it, its state, and
/// its forge and repository, when its URL names them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrBadge {
    pub number: Option<i64>,
    pub state: PrState,
    pub reference: Option<PrRef>,
}

/// The pull request `chat`'s diff status names, read as the web UI's `getPRIconConfig` reads
/// it: no state, or an empty one, is no pull request (`!state`); then merged, then closed,
/// then draft, and any other state, compared as sent, open. The server leaves the state out
/// when the chat has no pull request.
pub fn pr_badge(chat: &types::CodersdkChat) -> Option<PrBadge> {
    let status = chat.diff_status.as_ref()?;
    let state = status
        .pull_request_state
        .as_deref()
        .filter(|s| !s.is_empty())?;
    let state = match state {
        "merged" => PrState::Merged,
        "closed" => PrState::Closed,
        _ if status.pull_request_draft == Some(true) => PrState::Draft,
        _ => PrState::Open,
    };
    Some(PrBadge {
        number: status.pr_number,
        state,
        reference: status.url.as_deref().and_then(crate::forge::parse_pr_url),
    })
}

/// One row of `/chats`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatRow {
    pub id: Uuid,
    /// 0 for a root chat, 1 for a subagent.
    pub depth: u8,
    pub title: String,
    pub status: Option<ChatStatus>,
    pub unread: bool,
    pub archived: bool,
    pub pinned: bool,
    /// A root's subagent count.
    pub children: usize,
    /// The status of the busiest subagent, shown while the root is collapsed.
    pub busiest_child: Option<ChatStatus>,
    pub updated_unix: Option<i64>,
    /// The server's one-sentence summary of the chat's last turn, on one line.
    pub summary: Option<String>,
    /// The attached pull request, from the chat's diff status.
    pub pr: Option<PrBadge>,
}

/// The status that says the most about a family: working, then waiting on the user, then
/// failed.
fn busiest(chats: &[types::CodersdkChat]) -> Option<ChatStatus> {
    let weight = |s: &ChatStatus| match s {
        ChatStatus::Running | ChatStatus::Interrupting => 3,
        ChatStatus::RequiresAction => 2,
        ChatStatus::Error => 1,
        _ => 0,
    };
    chats
        .iter()
        .filter_map(chat_status)
        .filter(|s| weight(s) > 0)
        .max_by_key(|s| weight(s))
}

fn row(chat: &types::CodersdkChat, id: Uuid, depth: u8) -> ChatRow {
    ChatRow {
        id,
        depth,
        title: chat
            .title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| "Untitled".into()),
        status: chat_status(chat),
        unread: chat.has_unread == Some(true),
        archived: chat.archived == Some(true),
        pinned: chat.pin_order.unwrap_or(0) > 0,
        children: 0,
        busiest_child: None,
        updated_unix: chat.updated_at.map(|t| t.timestamp()),
        summary: chat
            .last_turn_summary
            .as_deref()
            .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|s| !s.is_empty()),
        pr: pr_badge(chat),
    }
}

/// A chat's status, if it has one.
pub fn chat_status(chat: &types::CodersdkChat) -> Option<ChatStatus> {
    chat.status.as_ref().map(|s| ChatStatus::parse(s.as_str()))
}

/// Whether `status` means the agent is working or waiting on the user.
pub fn is_active(status: Option<&ChatStatus>) -> bool {
    matches!(
        status,
        Some(ChatStatus::Running | ChatStatus::Interrupting | ChatStatus::RequiresAction)
    )
}

/// Pinned chats first by pin order, then the most recently updated, as the server orders
/// `GET /chats` (`coderd/database/queries/chats.sql`, `GetChats`).
fn sort(chats: &mut [types::CodersdkChat]) {
    chats.sort_by(order);
}

fn order(a: &types::CodersdkChat, b: &types::CodersdkChat) -> Ordering {
    let pin = |c: &types::CodersdkChat| c.pin_order.filter(|p| *p > 0);
    let pinned = match (pin(a), pin(b)) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    };
    pinned
        .then(b.updated_at.cmp(&a.updated_at))
        .then(b.id.cmp(&a.id))
}

/// Moves `c.updated_at` forward to `new`'s, never back.
fn take_newer_updated_at(c: &mut types::CodersdkChat, new: &types::CodersdkChat) {
    if new.updated_at > c.updated_at {
        c.updated_at = new.updated_at;
    }
}

/// Merges the fields a watch event of `kind` owns into one loaded copy, after the web UI's
/// `mergeWatchedChatSummary`. `updated_at` guards the status and the row's bindings, so a
/// late or repeated event cannot undo a newer one; the title, summaries, diff status, and
/// context flags change without bumping `updated_at`, so their events always apply.
fn merge(c: &mut types::CodersdkChat, new: &types::CodersdkChat, kind: &str, open: Option<Uuid>) {
    let fresh = new.updated_at >= c.updated_at;
    match kind {
        "status_change" => {
            if fresh {
                c.status = new.status.clone();
                if open != new.id {
                    c.has_unread = Some(true);
                }
            }
            if c.status.as_ref().map(|s| s.as_str()) != Some("running") {
                c.queued_for_capacity = Some(false);
            }
        }
        // The payload is the row committed after entering `requires_action`.
        "action_required" if fresh => {
            if let Some(status) = new.status.clone() {
                c.status = Some(status);
            }
        }
        // Title generation can publish an older snapshot, so the title is always taken.
        "title_change" => c.title = new.title.clone(),
        "summary_change" => c.last_turn_summary = new.last_turn_summary.clone(),
        "chat_summary_change" => c.summary = new.summary.clone(),
        "diff_status_change" => c.diff_status = new.diff_status.clone(),
        // Watch payloads leave the resources out, so only the flags are merged.
        "context_dirty" => {
            if let Some(flags) = new.context.as_ref() {
                let context = c.context.get_or_insert_with(Default::default);
                context.dirty = flags.dirty;
                context.dirty_since = flags.dirty_since;
                context.error = flags.error.clone();
            }
        }
        _ => {}
    }
    if fresh {
        c.workspace_id = new.workspace_id.or(c.workspace_id);
        // A build belongs to its agent, so it is taken only with the same agent.
        if new.agent_id == c.agent_id {
            c.build_id = new.build_id.or(c.build_id);
        }
        c.last_model_config_id = new.last_model_config_id.or(c.last_model_config_id);
        c.updated_at = new.updated_at;
    }
}

impl ChatList {
    pub fn page(&self, query: &ListQuery) -> Option<&Page> {
        match query {
            ListQuery::Default => Some(&self.main),
            ListQuery::Archived => Some(&self.archived),
            ListQuery::Search(text) => self
                .search
                .as_ref()
                .filter(|(searched, _)| searched == text)
                .map(|(_, page)| page),
        }
    }

    fn page_mut(&mut self, query: &ListQuery) -> Option<&mut Page> {
        match query {
            ListQuery::Default => Some(&mut self.main),
            ListQuery::Archived => Some(&mut self.archived),
            ListQuery::Search(text) => self
                .search
                .as_mut()
                .filter(|(searched, _)| searched == text)
                .map(|(_, page)| page),
        }
    }

    /// Marks `query` as loading and returns the offset to fetch: the start, or with `more` the
    /// end of what is loaded. `None` while a load is in flight or when nothing is left. A new
    /// search replaces the previous one.
    pub fn begin_load(&mut self, query: &ListQuery, more: bool) -> Option<i64> {
        if let ListQuery::Search(text) = query
            && !more
        {
            self.search = Some((text.clone(), Page::default()));
        }
        let page = self.page_mut(query)?;
        if page.load == Load::Loading || (more && page.exhausted) {
            return None;
        }
        page.load = Load::Loading;
        if !more {
            return Some(0);
        }
        // Chats archived here stay listed, but the server no longer counts them.
        let listed = match query {
            ListQuery::Archived => page.chats.len(),
            _ => page
                .chats
                .iter()
                .filter(|c| c.archived != Some(true))
                .count(),
        };
        Some(listed as i64)
    }

    /// Applies a page fetched at `offset`. A short first page is the whole list. A full first
    /// page replaces the loaded rows that sort within it, so a chat deleted elsewhere goes, and
    /// keeps the rows past it that were scrolled to.
    pub fn apply_page(&mut self, query: &ListQuery, offset: i64, chats: Vec<types::CodersdkChat>) {
        self.apply_page_keeping(query, offset, chats, None);
    }

    /// Like [`ChatList::apply_page`], but a full first page never prunes the row holding
    /// `keep`, as a root or a subagent. The open chat's copy can sort inside the fresh page
    /// while the server ranks it past it, as after an unpin elsewhere, and its row stays.
    pub fn apply_page_keeping(
        &mut self,
        query: &ListQuery,
        offset: i64,
        chats: Vec<types::CodersdkChat>,
        keep: Option<Uuid>,
    ) {
        let full = chats.len() as i64 >= PAGE_SIZE;
        let search = matches!(query, ListQuery::Search(_));
        let Some(page) = self.page_mut(query) else {
            return;
        };
        if offset == 0 && (!full || search) {
            page.chats = chats;
        } else {
            if offset == 0
                && let Some(last) = chats.iter().min_by(|a, b| order(b, a))
            {
                let kept = |c: &types::CodersdkChat| {
                    keep.is_some_and(|k| {
                        c.id == Some(k) || c.children.iter().any(|s| s.id == Some(k))
                    })
                };
                page.chats.retain(|c| {
                    order(c, last) == Ordering::Greater
                        || chats.iter().any(|n| n.id == c.id)
                        || kept(c)
                });
            }
            for chat in chats {
                match page
                    .chats
                    .iter_mut()
                    .find(|c| c.id.is_some() && c.id == chat.id)
                {
                    Some(slot) => *slot = chat,
                    None => page.chats.push(chat),
                }
            }
        }
        page.exhausted = !full;
        page.load = Load::Loaded;
        // Search results keep the server's order: pinned first by `pin_order`, then
        // `updated_at` newest first, then `id`, with no ranking by match.
        if !search {
            sort(&mut page.chats);
        }
    }

    pub fn fail(&mut self, query: &ListQuery, message: String) {
        if let Some(page) = self.page_mut(query) {
            page.load = Load::Failed(message);
        }
    }

    fn pages(&self) -> impl Iterator<Item = &Page> {
        [&self.main, &self.archived]
            .into_iter()
            .chain(self.search.as_ref().map(|(_, page)| page))
    }

    /// The chat or subagent `id` from any loaded page.
    pub fn find(&self, id: Uuid) -> Option<&types::CodersdkChat> {
        self.pages()
            .flat_map(|page| page.chats.iter())
            .flat_map(|root| std::iter::once(root).chain(root.children.iter()))
            .find(|c| c.id == Some(id))
    }

    /// Runs `f` on every loaded copy of chat or subagent `id`; returns whether there was one.
    pub fn update_copies(&mut self, id: Uuid, mut f: impl FnMut(&mut types::CodersdkChat)) -> bool {
        let mut found = false;
        let pages = [&mut self.main, &mut self.archived]
            .into_iter()
            .chain(self.search.as_mut().map(|(_, page)| page));
        for page in pages {
            for root in page.chats.iter_mut() {
                if root.id == Some(id) {
                    f(root);
                    found = true;
                }
                for child in root.children.iter_mut() {
                    if child.id == Some(id) {
                        f(child);
                        found = true;
                    }
                }
            }
        }
        found
    }

    pub fn set_read(&mut self, id: Uuid, read: bool) {
        self.update_copies(id, |c| c.has_unread = Some(!read));
    }

    /// Re-sorts the lists after a local change to pin order or activity.
    pub fn resort(&mut self) {
        sort(&mut self.main.chats);
        sort(&mut self.archived.chats);
    }

    /// The highest pin order among every loaded chat, for optimistically pinning a new one
    /// after them, as the web UI does (`site/src/api/queries/chats.ts`,
    /// `getNextOptimisticPinOrder`).
    pub fn max_pin_order(&self) -> i64 {
        self.pages()
            .flat_map(|p| p.chats.iter())
            .filter_map(|c| c.pin_order)
            .max()
            .unwrap_or(0)
    }

    /// Whether the family of `id` (its root and every subagent) has an active member.
    pub fn family_running(&self, id: Uuid) -> bool {
        let root = self
            .find(id)
            .and_then(|c| c.parent_chat_id)
            .and_then(|parent| self.find(parent))
            .or_else(|| self.find(id));
        root.is_some_and(|r| {
            std::iter::once(r)
                .chain(r.children.iter())
                .any(|c| is_active(chat_status(c).as_ref()))
        })
    }

    /// The rows of `/chats` for `filter`, ranked by `query` on root titles, with the subagents
    /// of each root in `expanded` right below it. A root whose subagents match `query` follows
    /// the roots that match by title, opened on the matching subagents.
    pub fn rows(&self, filter: Filter, query: &str, expanded: &HashSet<Uuid>) -> Vec<ChatRow> {
        fn family(c: &types::CodersdkChat) -> impl Iterator<Item = &types::CodersdkChat> {
            std::iter::once(c).chain(c.children.iter())
        }
        let title = |c: &&types::CodersdkChat| c.title.clone().unwrap_or_default();
        let page = match filter {
            Filter::Archived => &self.archived,
            _ => &self.main,
        };
        let roots: Vec<&types::CodersdkChat> = page
            .chats
            .iter()
            .filter(|c| match filter {
                Filter::All => c.archived != Some(true),
                Filter::Active => {
                    c.archived != Some(true)
                        && family(c).any(|m| is_active(chat_status(m).as_ref()))
                }
                Filter::Unread => {
                    c.archived != Some(true) && family(c).any(|m| m.has_unread == Some(true))
                }
                Filter::Archived => true,
            })
            .collect();
        let mut ranked = crate::fuzzy::rank(query, roots.clone(), title);
        let mut matched: HashMap<Uuid, Vec<&types::CodersdkChat>> = HashMap::new();
        if !query.trim().is_empty() {
            for root in roots {
                let hits = crate::fuzzy::rank(query, root.children.iter().collect(), title);
                let Some(id) = root.id.filter(|_| !hits.is_empty()) else {
                    continue;
                };
                if !ranked.iter().any(|r| r.id == root.id) {
                    ranked.push(root);
                }
                matched.insert(id, hits);
            }
        }
        let mut rows = Vec::new();
        for root in ranked {
            let Some(id) = root.id else {
                continue;
            };
            let shown: Vec<&types::CodersdkChat> = if expanded.contains(&id) {
                root.children.iter().collect()
            } else {
                matched.remove(&id).unwrap_or_default()
            };
            rows.push(ChatRow {
                children: root.children.len(),
                busiest_child: shown.is_empty().then(|| busiest(&root.children)).flatten(),
                ..row(root, id, 0)
            });
            rows.extend(shown.into_iter().filter_map(|c| Some(row(c, c.id?, 1))));
        }
        rows
    }

    /// The server search results and their load state, while `query` is the searched text.
    pub fn search_rows(&self, query: &str) -> Option<(Vec<ChatRow>, &Load)> {
        let (searched, page) = self.search.as_ref()?;
        if searched != query {
            return None;
        }
        let rows = page
            .chats
            .iter()
            .filter_map(|c| {
                let id = c.id?;
                let mut r = row(c, id, 0);
                // The search index carries no children; a result also loaded in the main
                // list shows the same family marker that list draws.
                if let Some(loaded) = self.main.chats.iter().find(|m| m.id == Some(id)) {
                    r.children = loaded.children.len();
                    r.busiest_child = busiest(&loaded.children);
                }
                Some(r)
            })
            .collect();
        Some((rows, &page.load))
    }

    /// Whether any loaded chat or subagent is working, so the list's spinners must move.
    pub fn any_running(&self) -> bool {
        self.pages()
            .flat_map(|p| p.chats.iter())
            .flat_map(|root| std::iter::once(root).chain(root.children.iter()))
            .any(|c| {
                matches!(
                    chat_status(c),
                    Some(ChatStatus::Running | ChatStatus::Interrupting)
                )
            })
    }

    /// Merges one watch event about `chat`; `open` is the chat on screen, which is never
    /// marked unread. Returns whether a loaded chat took the event. A chat on no loaded page is
    /// left out: the next page or refetch that includes it brings its current state.
    pub fn apply_watch(
        &mut self,
        kind: &str,
        chat: &types::CodersdkChat,
        open: Option<Uuid>,
    ) -> bool {
        let Some(id) = chat.id else {
            return false;
        };
        let changed = match kind {
            "created" => self.insert(chat),
            "deleted" => self.archive(chat),
            "status_change"
            | "action_required"
            | "title_change"
            | "summary_change"
            | "chat_summary_change"
            | "diff_status_change"
            | "context_dirty" => self.update_copies(id, |c| merge(c, chat, kind, open)),
            _ => false,
        };
        if changed {
            sort(&mut self.main.chats);
            sort(&mut self.archived.chats);
        }
        changed
    }

    /// Whether `id` is a root or a subagent in the main list.
    fn in_main(&self, id: Uuid) -> bool {
        self.main
            .chats
            .iter()
            .flat_map(|root| std::iter::once(root).chain(root.children.iter()))
            .any(|c| c.id == Some(id))
    }

    /// A new chat, or one family member of an unarchived chat, which the server also publishes
    /// as `created`: a root goes to the top and a subagent under its parent. A subagent whose
    /// parent is not in the main list is left out, and arrives with its parent's page.
    fn insert(&mut self, chat: &types::CodersdkChat) -> bool {
        let Some(id) = chat.id else {
            return false;
        };
        // Watch payloads carry no subagents, so an unarchived root keeps its archived copy's.
        let archived_copy = self
            .archived
            .chats
            .iter()
            .position(|c| c.id == Some(id))
            .map(|i| self.archived.chats.remove(i));
        let unarchive = |c: &mut types::CodersdkChat| {
            c.archived = Some(false);
            take_newer_updated_at(c, chat);
        };
        let found = self.update_copies(id, unarchive) || archived_copy.is_some();
        if self.in_main(id) {
            return true;
        }
        match chat.parent_chat_id {
            Some(parent) => {
                let Some(root) = self.main.chats.iter_mut().find(|c| c.id == Some(parent)) else {
                    return found;
                };
                let mut chat = chat.clone();
                chat.archived = Some(false);
                root.children.insert(0, chat);
            }
            None => {
                // The archived copy's `has_unread` is the server's; a payload's is always false.
                let mut root = archived_copy.unwrap_or_else(|| chat.clone());
                unarchive(&mut root);
                self.main.chats.insert(0, root);
            }
        }
        true
    }

    /// Archives one family member; the server publishes `deleted` once for each
    /// (`coderd/x/chatd/chatd.go`, `setChatFamilyArchived`), and archiving also unpins and bumps
    /// `updated_at` (`coderd/database/queries/chats.sql`, `ArchiveChatByID`).
    fn archive(&mut self, chat: &types::CodersdkChat) -> bool {
        let Some(id) = chat.id else {
            return false;
        };
        let archive = |c: &mut types::CodersdkChat| {
            c.archived = Some(true);
            c.pin_order = Some(0);
            take_newer_updated_at(c, chat);
        };
        let mut changed = self.update_copies(id, archive);
        if self.archived.load != Load::Loaded
            || self.archived.chats.iter().any(|c| c.id == Some(id))
        {
            return changed;
        }
        let root = match self.main.chats.iter().find(|c| c.id == Some(id)) {
            Some(listed) => Some(listed.clone()),
            None if chat.parent_chat_id.is_none() => {
                let mut root = chat.clone();
                archive(&mut root);
                changed = true;
                Some(root)
            }
            None => None,
        };
        if let Some(root) = root {
            self.archived.chats.insert(0, root);
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn listed(id: Uuid, title: &str, updated: &str) -> types::CodersdkChat {
        types::CodersdkChat {
            id: Some(id),
            title: Some(title.into()),
            updated_at: Some(updated.parse().unwrap()),
            ..Default::default()
        }
    }

    fn titles(page: &Page) -> Vec<&str> {
        page.chats
            .iter()
            .filter_map(|c| c.title.as_deref())
            .collect()
    }

    fn with_status(mut c: types::CodersdkChat, status: &str) -> types::CodersdkChat {
        c.status = Some(types::CodersdkChatStatus(status.into()));
        c
    }

    fn subagent(id: Uuid, parent: Uuid, title: &str, updated: &str) -> types::CodersdkChat {
        let mut c = listed(id, title, updated);
        c.parent_chat_id = Some(parent);
        c.root_chat_id = Some(parent);
        c
    }

    #[test]
    fn pinned_chats_come_first_then_the_newest() {
        let mut list = ChatList::default();
        let mut pinned = listed(Uuid::new_v4(), "pinned", "2026-09-01T00:00:00Z");
        pinned.pin_order = Some(1);
        list.begin_load(&ListQuery::Default, false);
        list.apply_page(
            &ListQuery::Default,
            0,
            vec![
                listed(Uuid::new_v4(), "old", "2026-09-29T00:00:00Z"),
                pinned,
                listed(Uuid::new_v4(), "new", "2026-09-30T00:00:00Z"),
            ],
        );
        assert_eq!(titles(&list.main), ["pinned", "new", "old"]);
        assert_eq!(list.main.load, Load::Loaded);
        assert!(list.main.exhausted, "a short first page is the whole list");
    }

    #[test]
    fn later_pages_append_until_one_comes_back_short() {
        let mut list = ChatList::default();
        let page = |n: usize, day: u32| -> Vec<types::CodersdkChat> {
            (0..n)
                .map(|i| {
                    listed(
                        Uuid::new_v4(),
                        &format!("{day}-{i}"),
                        &format!("2026-09-{day:02}T00:{:02}:00Z", 59 - i),
                    )
                })
                .collect()
        };
        assert_eq!(list.begin_load(&ListQuery::Default, false), Some(0));
        assert_eq!(
            list.begin_load(&ListQuery::Default, true),
            None,
            "one load at a time"
        );
        list.apply_page(&ListQuery::Default, 0, page(50, 30));
        assert!(!list.main.exhausted);
        assert_eq!(list.begin_load(&ListQuery::Default, true), Some(50));
        assert_eq!(
            list.begin_load(&ListQuery::Default, true),
            None,
            "a burst of wheel ticks at the last row fetches the next page once"
        );
        list.apply_page(&ListQuery::Default, 50, page(3, 20));
        assert_eq!(list.main.chats.len(), 53);
        assert!(list.main.exhausted);
        assert_eq!(list.begin_load(&ListQuery::Default, true), None);
        list.fail(&ListQuery::Default, "boom".into());
        assert_eq!(list.main.load, Load::Failed("boom".into()));
    }

    #[test]
    fn a_fresh_status_change_is_taken_and_marks_other_chats_unread() {
        let (open, other) = (Uuid::new_v4(), Uuid::new_v4());
        let mut list = ChatList::default();
        list.apply_page(
            &ListQuery::Default,
            0,
            vec![
                listed(open, "open", "2026-09-30T10:00:00Z"),
                listed(other, "other", "2026-09-30T09:00:00Z"),
            ],
        );
        for id in [open, other] {
            let fresh = with_status(listed(id, "", "2026-09-30T11:00:00Z"), "running");
            assert!(list.apply_watch("status_change", &fresh, Some(open)));
        }
        let status = |list: &ChatList, id| list.find(id).and_then(chat_status);
        assert_eq!(status(&list, open), Some(ChatStatus::Running));
        assert_eq!(status(&list, other), Some(ChatStatus::Running));
        assert_eq!(list.find(open).and_then(|c| c.has_unread), None);
        assert_eq!(list.find(other).and_then(|c| c.has_unread), Some(true));
        let stale = with_status(listed(other, "", "2026-09-30T08:00:00Z"), "waiting");
        list.apply_watch("status_change", &stale, Some(open));
        assert_eq!(
            status(&list, other),
            Some(ChatStatus::Running),
            "an older payload is ignored"
        );
    }

    #[test]
    fn a_title_is_taken_even_from_an_older_payload() {
        let id = Uuid::new_v4();
        let mut list = ChatList::default();
        list.apply_page(
            &ListQuery::Default,
            0,
            vec![listed(id, "Untitled", "2026-09-30T10:00:00Z")],
        );
        list.apply_watch(
            "title_change",
            &listed(id, "Fix the watch test", "2026-09-30T09:00:00Z"),
            None,
        );
        assert_eq!(
            list.find(id).and_then(|c| c.title.as_deref()),
            Some("Fix the watch test")
        );
    }

    #[test]
    fn created_adds_a_root_at_the_top_or_a_subagent_under_its_parent() {
        let (root, child, fresh) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut list = ChatList::default();
        list.apply_page(
            &ListQuery::Default,
            0,
            vec![listed(root, "root", "2026-09-30T10:00:00Z")],
        );
        let mut sub = listed(child, "explore", "2026-09-30T10:01:00Z");
        sub.parent_chat_id = Some(root);
        list.apply_watch("created", &sub, None);
        list.apply_watch(
            "created",
            &listed(fresh, "fresh", "2026-09-30T12:00:00Z"),
            None,
        );
        assert_eq!(titles(&list.main), ["fresh", "root"]);
        assert_eq!(list.find(root).map(|c| c.children.len()), Some(1));
        assert!(list.find(child).is_some());
    }

    #[test]
    fn deleted_marks_the_chat_archived_and_created_unarchives_it() {
        let id = Uuid::new_v4();
        let mut list = ChatList::default();
        list.apply_page(
            &ListQuery::Default,
            0,
            vec![listed(id, "done", "2026-09-30T10:00:00Z")],
        );
        list.apply_page(&ListQuery::Archived, 0, vec![]);
        list.apply_watch("deleted", &listed(id, "done", "2026-09-30T10:00:00Z"), None);
        assert_eq!(list.find(id).and_then(|c| c.archived), Some(true));
        assert_eq!(titles(&list.archived), ["done"]);
        list.apply_watch("created", &listed(id, "done", "2026-09-30T10:00:00Z"), None);
        assert_eq!(list.main.chats[0].archived, Some(false));
        assert!(list.archived.chats.is_empty());
    }

    #[test]
    fn context_dirty_keeps_the_loaded_resources() {
        let id = Uuid::new_v4();
        let mut list = ChatList::default();
        let mut loaded = listed(id, "t", "2026-09-30T10:00:00Z");
        loaded.context = Some(types::CodersdkChatContext {
            resources: vec![types::CodersdkChatContextResource {
                skill_name: Some("deploy".into()),
                ..Default::default()
            }],
            ..Default::default()
        });
        list.apply_page(&ListQuery::Default, 0, vec![loaded]);
        let mut dirty = listed(id, "t", "2026-09-30T10:00:00Z");
        dirty.context = Some(types::CodersdkChatContext {
            dirty: Some(true),
            ..Default::default()
        });
        list.apply_watch("context_dirty", &dirty, None);
        let context = list.find(id).and_then(|c| c.context.as_ref()).unwrap();
        assert_eq!(context.dirty, Some(true));
        assert_eq!(context.resources.len(), 1);
    }

    #[test]
    fn a_running_subagent_makes_its_family_running() {
        let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
        let mut parent = listed(root, "root", "2026-09-30T10:00:00Z");
        parent.children = vec![with_status(
            listed(child, "sub", "2026-09-30T10:00:00Z"),
            "running",
        )];
        let mut list = ChatList::default();
        list.apply_page(&ListQuery::Default, 0, vec![parent]);
        assert!(list.family_running(root));
        assert!(list.family_running(child));
    }

    #[test]
    fn search_results_for_an_old_query_are_dropped_and_the_query_is_quoted() {
        let mut list = ChatList::default();
        list.begin_load(&ListQuery::Search("watch".into()), false);
        list.begin_load(&ListQuery::Search("reconnect".into()), false);
        list.apply_page(
            &ListQuery::Search("watch".into()),
            0,
            vec![listed(Uuid::new_v4(), "stale", "2026-09-30T10:00:00Z")],
        );
        assert!(
            list.page(&ListQuery::Search("reconnect".into()))
                .unwrap()
                .chats
                .is_empty()
        );
        assert_eq!(
            ListQuery::Search("say \"hi\"".into()).q().as_deref(),
            Some("search:\"say  hi \"")
        );
        assert_eq!(ListQuery::Archived.q().as_deref(), Some("archived:true"));
        assert_eq!(ListQuery::Default.q(), None);
    }

    fn q(text: &str) -> String {
        ListQuery::Search(text.into()).q().unwrap()
    }

    #[test]
    fn server_operators_go_into_q_as_typed() {
        assert_eq!(q("status:running repo:coder"), "status:running repo:coder");
        assert_eq!(
            q("Status:error"),
            "Status:error",
            "the server lowercases keys"
        );
        assert_eq!(
            q("archived:true has_unread:true pr:12"),
            "archived:true has_unread:true pr:12"
        );
    }

    #[test]
    fn words_beside_operators_become_one_search_term() {
        assert_eq!(
            q("status:error flaky test"),
            "status:error search:\"flaky test\""
        );
        assert_eq!(q("flaky status:error"), "status:error search:\"flaky\"");
    }

    #[test]
    fn text_without_a_known_key_is_all_one_search() {
        assert_eq!(q("note: hi"), "search:\"note: hi\"");
        assert_eq!(q("foo:bar baz"), "search:\"foo:bar baz\"");
        assert_eq!(q("status:"), "search:\"status:\"");
    }

    #[test]
    fn a_quoted_operator_value_with_spaces_stays_one_term() {
        assert_eq!(
            q("title:\"fix ci\" status:running"),
            "title:\"fix ci\" status:running"
        );
    }

    #[test]
    fn an_unterminated_quote_in_an_operator_value_becomes_a_space_as_in_words() {
        // The stray quote would otherwise swallow `status:running` into the title.
        assert_eq!(
            q("title:\"fix ci status:running"),
            "status:running search:\"title: fix ci\""
        );
        assert_eq!(
            q("status:running title:\"fix ci"),
            "status:running search:\"title: fix ci\""
        );
        assert_eq!(
            q("title:\"fix ci\" repo:\"coder status:error"),
            "title:\"fix ci\" status:error search:\"repo: coder\""
        );
        assert_eq!(q("say \"hi"), "search:\"say  hi\"", "as in words");
    }

    #[test]
    fn search_rows_show_only_for_the_text_that_was_searched() {
        let mut list = ChatList::default();
        assert!(list.search_rows("watch").is_none());
        list.begin_load(&ListQuery::Search("watch".into()), false);
        let (rows, load) = list.search_rows("watch").unwrap();
        assert!(rows.is_empty());
        assert_eq!(*load, Load::Loading);
        list.apply_page(
            &ListQuery::Search("watch".into()),
            0,
            vec![listed(
                Uuid::new_v4(),
                "Mentions watch in a message",
                "2026-09-01T00:00:00Z",
            )],
        );
        let (rows, _) = list.search_rows("watch").unwrap();
        assert_eq!(rows[0].title, "Mentions watch in a message");
        assert!(
            list.search_rows("watch it").is_none(),
            "new text drops the results"
        );
    }

    #[test]
    fn search_rows_show_the_family_marker_from_the_main_list() {
        let (mut list, root, _child) = family();
        list.begin_load(&ListQuery::Search("flaky".into()), false);
        list.apply_page(
            &ListQuery::Search("flaky".into()),
            0,
            // The search index carries no children, unlike the loaded main list's copy.
            vec![listed(
                root,
                "Fix the flaky watch reconnect test",
                "2026-09-30T10:00:00Z",
            )],
        );
        let (rows, _) = list.search_rows("flaky").unwrap();
        assert_eq!(rows[0].children, 1);
        assert_eq!(rows[0].busiest_child, Some(ChatStatus::Running));
    }

    #[test]
    fn an_update_for_a_chat_on_no_loaded_page_changes_nothing() {
        let (known, unknown) = (Uuid::new_v4(), Uuid::new_v4());
        let mut list = ChatList::default();
        list.apply_page(
            &ListQuery::Default,
            0,
            vec![listed(known, "known", "2026-09-30T10:00:00Z")],
        );
        // A loaded archived page takes an archived root it lacks; see
        // `archiving_an_unlisted_root_adds_it_to_a_loaded_archived_page`.
        let stranger = with_status(
            listed(unknown, "stranger", "2026-09-30T12:00:00Z"),
            "running",
        );
        for kind in [
            "status_change",
            "title_change",
            "summary_change",
            "chat_summary_change",
            "diff_status_change",
            "context_dirty",
            "action_required",
            "deleted",
            "some_future_kind",
        ] {
            assert!(!list.apply_watch(kind, &stranger, None), "{kind}");
        }
        assert!(list.find(unknown).is_none());
        assert_eq!(titles(&list.main), ["known"]);
        assert!(list.archived.chats.is_empty());
        let id_less = types::CodersdkChat::default();
        assert!(!list.apply_watch("created", &id_less, None));
        assert_eq!(list.main.chats.len(), 1);
    }

    #[test]
    fn archiving_a_family_marks_every_copy_and_unpins_the_root() {
        let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
        let mut parent = listed(root, "root", "2026-09-30T10:00:00Z");
        parent.pin_order = Some(2);
        parent.children = vec![subagent(child, root, "sub", "2026-09-30T10:00:00Z")];
        let mut list = ChatList::default();
        list.apply_page(&ListQuery::Default, 0, vec![parent.clone()]);
        list.begin_load(&ListQuery::Search("root".into()), false);
        list.apply_page(&ListQuery::Search("root".into()), 0, vec![parent]);
        // The server publishes `deleted` once per family member, child first here.
        assert!(list.apply_watch(
            "deleted",
            &subagent(child, root, "sub", "2026-09-30T10:05:00Z"),
            None
        ));
        assert!(list.apply_watch(
            "deleted",
            &listed(root, "root", "2026-09-30T10:05:00Z"),
            None
        ));
        let root_copies = [
            &list.main.chats[0],
            &list.search.as_ref().unwrap().1.chats[0],
        ];
        for copy in root_copies {
            assert_eq!(copy.archived, Some(true));
            assert_eq!(copy.pin_order, Some(0), "archiving unpins");
            assert_eq!(copy.children[0].archived, Some(true));
        }
        assert!(
            list.archived.chats.is_empty(),
            "an archived list never loaded is fetched when it is opened"
        );
    }

    #[test]
    fn unarchiving_a_family_restores_its_subagents_whatever_the_event_order() {
        let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
        let mut parent = listed(root, "root", "2026-09-30T10:00:00Z");
        parent.archived = Some(true);
        let mut sub = subagent(child, root, "sub", "2026-09-30T10:00:00Z");
        sub.archived = Some(true);
        parent.children = vec![sub];
        let mut list = ChatList::default();
        list.apply_page(&ListQuery::Default, 0, vec![]);
        list.apply_page(&ListQuery::Archived, 0, vec![parent]);
        // Watch payloads never embed children, so the archived copy supplies them.
        assert!(list.apply_watch(
            "created",
            &subagent(child, root, "sub", "2026-09-30T10:06:00Z"),
            None
        ));
        assert!(list.apply_watch(
            "created",
            &listed(root, "root", "2026-09-30T10:06:00Z"),
            None
        ));
        assert!(list.archived.chats.is_empty());
        assert_eq!(titles(&list.main), ["root"]);
        let restored = &list.main.chats[0];
        assert_eq!(restored.archived, Some(false));
        assert_eq!(restored.children.len(), 1);
        assert_eq!(restored.children[0].archived, Some(false));
    }

    #[test]
    fn a_subagent_whose_parent_is_not_loaded_waits_for_the_parent() {
        let (root, child, other) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut list = ChatList::default();
        list.apply_page(
            &ListQuery::Default,
            0,
            vec![listed(other, "other", "2026-09-30T09:00:00Z")],
        );
        let sub = with_status(
            subagent(child, root, "explore", "2026-09-30T10:00:00Z"),
            "running",
        );
        assert!(!list.apply_watch("created", &sub, None));
        assert!(!list.apply_watch("status_change", &sub, None));
        assert!(list.find(child).is_none(), "never listed as a root");
        assert_eq!(titles(&list.main), ["other"]);
        assert!(!list.family_running(child));
        let mut parent = listed(root, "root", "2026-09-30T10:00:00Z");
        parent.children = vec![sub];
        list.apply_page(
            &ListQuery::Default,
            0,
            vec![parent, listed(other, "other", "2026-09-30T09:00:00Z")],
        );
        assert_eq!(list.find(child).and_then(|c| c.parent_chat_id), Some(root));
        assert!(list.family_running(root));
    }

    #[test]
    fn duplicate_and_out_of_order_events_settle_on_the_newest_state() {
        let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
        let mut list = ChatList::default();
        list.apply_page(
            &ListQuery::Default,
            0,
            vec![listed(Uuid::new_v4(), "older", "2026-09-30T09:00:00Z")],
        );
        let created = listed(root, "root", "2026-09-30T10:00:00Z");
        let sub = subagent(child, root, "sub", "2026-09-30T10:00:00Z");
        for _ in 0..2 {
            list.apply_watch("created", &created, None);
            list.apply_watch("created", &sub, None);
        }
        assert_eq!(titles(&list.main), ["root", "older"], "no duplicate row");
        assert_eq!(list.find(root).map(|c| c.children.len()), Some(1));

        let mut newer = with_status(listed(root, "root", "2026-09-30T10:10:00Z"), "waiting");
        newer.workspace_id = Some(Uuid::new_v4());
        let mut older = with_status(listed(root, "root", "2026-09-30T10:05:00Z"), "running");
        older.workspace_id = Some(Uuid::new_v4());
        list.apply_watch("status_change", &newer, None);
        list.set_read(root, true);
        list.apply_watch("status_change", &older, None);
        list.apply_watch(
            "action_required",
            &with_status(older.clone(), "requires_action"),
            None,
        );
        let chat = list.find(root).unwrap();
        assert_eq!(chat_status(chat), Some(ChatStatus::Waiting));
        assert_eq!(chat.workspace_id, newer.workspace_id);
        assert_eq!(
            chat.has_unread,
            Some(false),
            "a stale event marks nothing unread"
        );
        assert_eq!(
            chat.updated_at, newer.updated_at,
            "updated_at never moves back"
        );

        // The same event twice is taken once.
        list.apply_watch("status_change", &newer, None);
        list.apply_watch("status_change", &newer, None);
        assert_eq!(
            chat_status(list.find(root).unwrap()),
            Some(ChatStatus::Waiting)
        );
        assert_eq!(list.main.chats.len(), 2);

        let fresh_action = with_status(
            listed(root, "root", "2026-09-30T10:20:00Z"),
            "requires_action",
        );
        list.apply_watch("action_required", &fresh_action, None);
        assert_eq!(
            chat_status(list.find(root).unwrap()),
            Some(ChatStatus::RequiresAction)
        );
    }

    fn window(n: usize, day: u32) -> Vec<types::CodersdkChat> {
        (0..n)
            .map(|i| {
                listed(
                    Uuid::new_v4(),
                    &format!("{day}-{i}"),
                    &format!("2026-09-{day:02}T00:{:02}:00Z", 59 - i),
                )
            })
            .collect()
    }

    #[test]
    fn loading_more_counts_only_the_rows_the_server_still_lists() {
        let mut list = ChatList::default();
        let first = window(50, 30);
        let gone = first[3].clone();
        list.apply_page(&ListQuery::Default, 0, first);
        list.apply_watch("deleted", &gone, None);
        assert_eq!(list.begin_load(&ListQuery::Default, true), Some(49));
    }

    #[test]
    fn a_short_refetch_drops_chats_deleted_elsewhere() {
        let mut list = ChatList::default();
        let first = window(50, 30);
        let kept: Vec<_> = first[..10].to_vec();
        list.apply_page(&ListQuery::Default, 0, first);
        list.apply_page(&ListQuery::Default, 50, window(3, 20));
        assert!(list.main.exhausted);
        list.apply_page(&ListQuery::Default, 0, kept);
        assert_eq!(list.main.chats.len(), 10);
        assert!(list.main.exhausted);
    }

    #[test]
    fn a_full_refetch_prunes_its_window_and_keeps_the_rows_past_it() {
        let mut list = ChatList::default();
        let first = window(50, 30);
        list.apply_page(&ListQuery::Default, 0, first.clone());
        list.apply_page(&ListQuery::Default, 50, window(3, 20));
        let mut fresh = first;
        fresh.remove(5);
        fresh.insert(0, listed(Uuid::new_v4(), "newest", "2026-09-30T01:00:00Z"));
        list.apply_page(&ListQuery::Default, 0, fresh);
        let titles = titles(&list.main);
        assert_eq!(titles.len(), 53);
        assert_eq!(titles[0], "newest");
        assert!(!titles.contains(&"30-5"), "deleted elsewhere");
        assert_eq!(titles[50..], ["20-0", "20-1", "20-2"], "past the window");
        assert!(!list.main.exhausted, "rows past the window may have moved");
    }

    #[test]
    fn archiving_an_unlisted_root_adds_it_to_a_loaded_archived_page() {
        let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
        let mut list = ChatList::default();
        list.apply_page(&ListQuery::Default, 0, vec![]);
        list.apply_page(&ListQuery::Archived, 0, vec![]);
        let mut payload = listed(root, "elsewhere", "2026-09-30T10:00:00Z");
        payload.pin_order = Some(3);
        assert!(list.apply_watch("deleted", &payload, None));
        // A subagent of an unlisted root has no copy to mark.
        let sub = subagent(child, root, "sub", "2026-09-30T10:00:00Z");
        assert!(!list.apply_watch("deleted", &sub, None));
        assert_eq!(titles(&list.archived), ["elsewhere"]);
        assert_eq!(list.archived.chats[0].archived, Some(true));
        assert_eq!(list.archived.chats[0].pin_order, Some(0));
        assert!(list.main.chats.is_empty());
    }

    #[test]
    fn archive_and_unarchive_keep_the_trusted_copy_and_the_newer_updated_at() {
        let id = Uuid::new_v4();
        let mut list = ChatList::default();
        let mut loaded = listed(id, "done", "2026-09-30T10:00:00Z");
        loaded.has_unread = Some(true);
        list.apply_page(&ListQuery::Default, 0, vec![]);
        list.apply_page(&ListQuery::Archived, 0, vec![]);
        list.apply_page(&ListQuery::Default, 0, vec![loaded]);
        let archived_at = listed(id, "done", "2026-09-30T11:00:00Z");
        list.apply_watch("deleted", &archived_at, None);
        assert_eq!(list.find(id).unwrap().updated_at, archived_at.updated_at);
        list.main.chats.clear();
        let mut unarchived = listed(id, "done", "2026-09-30T12:00:00Z");
        unarchived.has_unread = Some(false);
        list.apply_watch("created", &unarchived, None);
        let restored = &list.main.chats[0];
        assert_eq!(restored.archived, Some(false));
        assert_eq!(
            restored.has_unread,
            Some(true),
            "the payload's unread is not trusted"
        );
        assert_eq!(restored.updated_at, unarchived.updated_at);
    }

    #[test]
    fn action_required_takes_the_payloads_status() {
        let id = Uuid::new_v4();
        let mut list = ChatList::default();
        list.apply_page(
            &ListQuery::Default,
            0,
            vec![listed(id, "t", "2026-09-30T10:00:00Z")],
        );
        let payload = with_status(listed(id, "t", "2026-09-30T11:00:00Z"), "error");
        list.apply_watch("action_required", &payload, None);
        assert_eq!(chat_status(list.find(id).unwrap()), Some(ChatStatus::Error));
    }

    #[test]
    fn pinned_ties_fall_back_to_the_newest() {
        let mut list = ChatList::default();
        let mut older = listed(Uuid::new_v4(), "older", "2026-09-29T00:00:00Z");
        older.pin_order = Some(1);
        let mut newer = listed(Uuid::new_v4(), "newer", "2026-09-30T00:00:00Z");
        newer.pin_order = Some(1);
        list.apply_page(&ListQuery::Default, 0, vec![older, newer]);
        assert_eq!(titles(&list.main), ["newer", "older"]);
    }

    #[test]
    fn an_open_subagent_keeps_its_roots_row_across_a_full_refetch() {
        let mut list = ChatList::default();
        let (root, sub) = (Uuid::new_v4(), Uuid::new_v4());
        let mut parent = listed(root, "root", "2026-09-30T08:00:00Z");
        parent.pin_order = Some(1);
        parent.children = vec![subagent(sub, root, "sub", "2026-09-30T08:00:00Z")];
        list.begin_load(&ListQuery::Default, false);
        list.apply_page(&ListQuery::Default, 0, vec![parent]);
        let newer: Vec<_> = (0..50)
            .map(|n| {
                listed(
                    Uuid::new_v4(),
                    &format!("N{n}"),
                    &format!("2026-09-30T09:{n:02}:00Z"),
                )
            })
            .collect();
        list.begin_load(&ListQuery::Default, false);
        list.apply_page_keeping(&ListQuery::Default, 0, newer, Some(sub));
        assert!(
            list.main.chats.iter().any(|c| c.id == Some(root)),
            "the open subagent's root keeps its row"
        );
        assert!(list.find(sub).is_some());
        assert_eq!(list.main.chats.len(), 51);
    }

    fn family() -> (ChatList, Uuid, Uuid) {
        let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
        let mut parent = listed(
            root,
            "Fix the flaky watch reconnect test",
            "2026-09-30T10:00:00Z",
        );
        let mut sub = with_status(listed(child, "explore", "2026-09-30T10:00:00Z"), "running");
        sub.parent_chat_id = Some(root);
        sub.has_unread = Some(true);
        parent.children = vec![sub];
        let mut archived = listed(
            Uuid::new_v4(),
            "Old reconnect spike",
            "2026-09-27T10:00:00Z",
        );
        archived.archived = Some(true);
        let mut list = ChatList::default();
        list.apply_page(
            &ListQuery::Default,
            0,
            vec![
                parent,
                listed(
                    Uuid::new_v4(),
                    "Draft the M2 design",
                    "2026-09-30T09:00:00Z",
                ),
            ],
        );
        list.apply_page(&ListQuery::Archived, 0, vec![archived]);
        (list, root, child)
    }

    fn row_titles(rows: &[ChatRow]) -> Vec<&str> {
        rows.iter().map(|r| r.title.as_str()).collect()
    }

    #[test]
    fn filters_pick_all_active_unread_and_archived_chats() {
        let (list, _, _) = family();
        let none = HashSet::new();
        assert_eq!(
            row_titles(&list.rows(Filter::All, "", &none)),
            ["Fix the flaky watch reconnect test", "Draft the M2 design"]
        );
        assert_eq!(
            row_titles(&list.rows(Filter::Active, "", &none)),
            ["Fix the flaky watch reconnect test"],
            "a running subagent makes its root active"
        );
        assert_eq!(
            row_titles(&list.rows(Filter::Unread, "", &none)),
            ["Fix the flaky watch reconnect test"]
        );
        assert_eq!(
            row_titles(&list.rows(Filter::Archived, "", &none)),
            ["Old reconnect spike"]
        );
        assert_eq!(
            row_titles(&list.rows(Filter::All, "draft", &none)),
            ["Draft the M2 design"]
        );
        assert_eq!(Filter::Archived.next(), Filter::All);
        assert_eq!(Filter::Archived.query(), ListQuery::Archived);
    }

    #[test]
    fn a_collapsed_root_shows_its_count_and_busiest_subagent_and_expands_below() {
        let (list, root, child) = family();
        let rows = list.rows(Filter::All, "", &HashSet::new());
        assert_eq!(rows[0].children, 1);
        assert_eq!(rows[0].busiest_child, Some(ChatStatus::Running));
        let expanded = HashSet::from([root]);
        let rows = list.rows(Filter::All, "", &expanded);
        assert_eq!(rows[1].id, child);
        assert_eq!(rows[1].depth, 1);
        assert_eq!(
            rows[0].busiest_child, None,
            "an expanded root shows its subagents instead"
        );
        assert!(list.any_running());
    }

    #[test]
    fn a_matching_subagent_shows_under_its_root() {
        let (mut list, root, child) = family();
        let mut other = listed(Uuid::new_v4(), "plan", "2026-09-30T10:00:00Z");
        other.parent_chat_id = Some(root);
        list.main.chats[0].children.push(other);
        let rows = list.rows(Filter::All, "explore", &HashSet::new());
        assert_eq!(
            row_titles(&rows),
            ["Fix the flaky watch reconnect test", "explore"],
            "the root opens on the matching subagent only"
        );
        assert_eq!((rows[1].id, rows[1].depth), (child, 1));
        assert_eq!(rows[0].busiest_child, None);
        let rows = list.rows(Filter::All, "draft", &HashSet::new());
        assert_eq!(
            row_titles(&rows),
            ["Draft the M2 design"],
            "root ranking is unchanged"
        );
        let rows = list.rows(Filter::All, "explore", &HashSet::from([root]));
        assert_eq!(rows.len(), 3, "a root the user opened shows every subagent");
    }

    #[test]
    fn a_row_carries_the_last_turn_summary_on_one_line() {
        let id = Uuid::new_v4();
        let chat = |summary: &str| -> types::CodersdkChat {
            serde_json::from_value(serde_json::json!({"id": id, "title": "t",
                "last_turn_summary": summary, "children": [], "files": [],
                "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}))
            .unwrap()
        };
        assert_eq!(
            row(&chat("Fixing the CI\n  now"), id, 0).summary.as_deref(),
            Some("Fixing the CI now")
        );
        assert_eq!(row(&chat("  "), id, 0).summary, None);
    }

    #[test]
    fn a_row_carries_its_pull_request_in_the_web_uis_order() {
        let pr = |state: Option<&str>, draft: bool, number: Option<i64>| {
            let mut c = listed(Uuid::new_v4(), "t", "2026-09-30T10:00:00Z");
            c.diff_status = Some(types::CodersdkChatDiffStatus {
                pull_request_state: state.map(str::to_owned),
                pull_request_draft: Some(draft),
                pr_number: number,
                ..Default::default()
            });
            pr_badge(&c)
        };
        let badge = |number, state| {
            Some(PrBadge {
                number,
                state,
                reference: None,
            })
        };
        assert_eq!(
            pr(Some("merged"), true, Some(12)),
            badge(Some(12), PrState::Merged),
            "merged wins over draft"
        );
        assert_eq!(
            pr(Some("closed"), true, Some(78)),
            badge(Some(78), PrState::Closed)
        );
        assert_eq!(
            pr(Some("open"), true, Some(56)),
            badge(Some(56), PrState::Draft)
        );
        assert_eq!(
            pr(Some("open"), false, Some(34)),
            badge(Some(34), PrState::Open)
        );
        assert_eq!(
            pr(Some("locked"), false, None),
            badge(None, PrState::Open),
            "any other state reads as open"
        );
        assert_eq!(
            pr(None, true, Some(9)),
            None,
            "no state means no pull request"
        );
        assert_eq!(pr(Some(""), false, Some(9)), None, "an empty state is none");
        assert_eq!(
            pr(Some(" "), false, Some(9)),
            badge(Some(9), PrState::Open),
            "as the web UI's !state reads it, a blank state is a state"
        );
        assert_eq!(
            pr(Some(" merged"), false, Some(9)),
            badge(Some(9), PrState::Open),
            "the state is compared as sent"
        );
        let plain = listed(Uuid::new_v4(), "t", "2026-09-30T10:00:00Z");
        assert_eq!(pr_badge(&plain), None);
        let mut merged = plain.clone();
        merged.diff_status = Some(types::CodersdkChatDiffStatus {
            pull_request_state: Some("merged".into()),
            pr_number: Some(12),
            ..Default::default()
        });
        assert_eq!(
            row(&merged, Uuid::new_v4(), 0).pr,
            badge(Some(12), PrState::Merged)
        );
        assert_eq!(
            [
                PrState::Open,
                PrState::Draft,
                PrState::Merged,
                PrState::Closed
            ]
            .map(PrState::label),
            ["open", "draft", "merged", "closed"]
        );
    }

    #[test]
    fn a_pull_request_reads_its_forge_and_repository_from_its_url() {
        let with_url = |url: Option<&str>| {
            let mut c = listed(Uuid::new_v4(), "t", "2026-09-30T10:00:00Z");
            c.diff_status = Some(types::CodersdkChatDiffStatus {
                pull_request_state: Some("open".into()),
                pr_number: Some(5),
                url: url.map(str::to_owned),
                ..Default::default()
            });
            pr_badge(&c).unwrap().reference
        };
        assert_eq!(
            with_url(Some(
                "https://gitlab.example.com/g/sub/p/-/merge_requests/5"
            )),
            Some(PrRef {
                forge: crate::forge::Forge::GitLab,
                owner: "g/sub".into(),
                repo: "p".into(),
                number: 5,
            })
        );
        assert_eq!(with_url(Some("https://example.com/somewhere")), None);
        assert_eq!(with_url(None), None);
    }

    #[test]
    fn a_diff_status_change_updates_the_rows_pull_request() {
        let id = Uuid::new_v4();
        let mut list = ChatList::default();
        list.apply_page(
            &ListQuery::Default,
            0,
            vec![listed(id, "t", "2026-09-30T10:00:00Z")],
        );
        let pr = |list: &ChatList| list.rows(Filter::All, "", &HashSet::new())[0].pr.clone();
        assert_eq!(pr(&list), None);
        let mut event = listed(id, "t", "2026-09-30T10:01:00Z");
        event.diff_status = Some(types::CodersdkChatDiffStatus {
            pull_request_state: Some("open".into()),
            pr_number: Some(7),
            ..Default::default()
        });
        assert!(list.apply_watch("diff_status_change", &event, None));
        assert_eq!(
            pr(&list),
            Some(PrBadge {
                number: Some(7),
                state: PrState::Open,
                reference: None,
            })
        );
        event.diff_status = Some(types::CodersdkChatDiffStatus {
            pull_request_state: Some("merged".into()),
            pr_number: Some(7),
            ..Default::default()
        });
        assert!(list.apply_watch("diff_status_change", &event, None));
        assert_eq!(pr(&list).map(|b| b.state), Some(PrState::Merged));
    }
}
