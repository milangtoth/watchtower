use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Portal {
    pub id: String,
    pub url: String,
    pub state: String,
    pub reason: String,
    pub tender_count: i64,
    pub last_success_at: Option<String>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: String,
    pub kind: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub result: Option<String>,
}

/// A run with the reason the worker recorded when it finished: what broke on a failed read,
/// what a repair changed. Dashboard only; the appendix fixes the shape of [`Run`].
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RunHistory {
    pub id: String,
    pub kind: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub result: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Document {
    pub file_name: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub path: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Place {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub postal_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country_code: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TenderStatus {
    Open,
    Closed,
    Withdrawn,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Procedure {
    Open,
    Restricted,
    Negotiated,
    Other,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Nature {
    Works,
    Services,
    Supplies,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Tender {
    pub id: Option<String>,
    pub url: Option<String>,
    pub title: Option<String>,
    pub buyer_name: Option<String>,
    pub publication_date: Option<NaiveDate>,
    pub submission_deadline: Option<DateTime<FixedOffset>>,
    pub status: TenderStatus,
    pub documents: Vec<Document>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub procedure_type: Option<Procedure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contract_nature: Option<Nature>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpv_codes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub place_of_performance: Option<Place>,
}

impl Tender {
    pub fn key(&self) -> Option<String> {
        self.id
            .as_ref()
            .map(|id| format!("id:{id}"))
            .or_else(|| self.url.as_ref().map(|url| format!("url:{url}")))
    }

    pub fn close_if_due(&mut self, now: DateTime<Utc>) {
        if self.status == TenderStatus::Open
            && self.submission_deadline.is_some_and(|date| date <= now)
        {
            self.status = TenderStatus::Closed;
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedTender {
    #[serde(flatten)]
    pub tender: Tender,
    pub last_changed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    pub pages: usize,
    pub entries: usize,
    /// True once the reader found positive evidence that the listing ended. `end_of_list`
    /// names it.
    pub terminal_page: bool,
    /// `total` (the listing total matched the rows), `empty` (the listing showed its empty
    /// state or a total of 0) or `last-page` (the next-page selector ran out).
    pub end_of_list: &'static str,
    pub total: Option<usize>,
    pub empty_state: bool,
    pub document_count: usize,
    /// Documents and ZIP members left out because they passed the brief's size limits: 250 MB
    /// per file and 1 GB expanded per archive. Files within the limits are still delivered.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped_documents: Vec<String>,
    /// Listing pages fetched, in order.
    pub listing_pages: Vec<String>,
    /// Pagination-looking links on listing pages that the recipe did not follow.
    pub unfollowed_links: Vec<String>,
    pub samples: Vec<String>,
    /// Every same-origin link on the HTML listing pages, compared with the last verified read.
    #[serde(skip)]
    pub listing_links: HashSet<String>,
}

/// How Watchtower reads a portal. Probing finds candidates, ranking orders them, and the model
/// decides which one a recipe is learned for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionMethod {
    Html,
    Json,
    Ocds,
    Feed,
    Sitemap,
    Ted,
}

impl ConnectionMethod {
    /// The name used in reasons shown to an operator.
    pub fn label(self) -> &'static str {
        match self {
            Self::Html => "HTML",
            Self::Json => "JSON",
            Self::Ocds => "OCDS",
            Self::Feed => "Feed",
            Self::Sitemap => "Sitemap",
            Self::Ted => "TED",
        }
    }

    /// Whether Watchtower has a reader for this method. TED has none yet, and its search
    /// needs POST, so the probe reports it as evidence but it is never chosen.
    pub fn has_reader(self) -> bool {
        self != Self::Ted
    }
}

/// One way to read a portal, found by probing without the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionCandidate {
    pub method: ConnectionMethod,
    /// URL the recipe would start from.
    pub entry_url: String,
    /// Where the candidate was found, such as "link rel=alternate", "robots.txt" or "start page".
    pub found_via: String,
    pub http_status: u16,
    pub content_type: Option<String>,
    /// Rows or entries parsed from one sample.
    pub sample_items: usize,
    /// Tender fields seen in the sample, such as "title", "deadline", "buyer" or "documents".
    pub fields_seen: Vec<String>,
    pub has_pagination: bool,
    /// Ranker output between 0 and 1.
    pub score: f32,
}

impl ConnectionCandidate {
    /// A candidate can be chosen or used as a fallback when a reader exists for its method and
    /// the probe sampled it successfully. Unusable candidates stay in the decision as evidence.
    pub fn is_usable(&self) -> bool {
        self.method.has_reader() && (200..300).contains(&self.http_status)
    }
}

/// The connection method chosen for a portal, stored per attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionDecision {
    pub chosen: ConnectionCandidate,
    /// Remaining candidates in ranked order, used as fallbacks during repair.
    pub alternatives: Vec<ConnectionCandidate>,
    /// The model's reason, shown on the dashboard.
    pub rationale: String,
    pub model: String,
    /// RFC 3339.
    pub decided_at: String,
    /// 1 for the first attach, 2 or more after a failed repair re-decided the method.
    pub attempt: u32,
}

/// The listing evidence kept from the last verified read, for comparison with the next one.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredEvidence {
    pub listing_pages: Vec<String>,
    pub unfollowed_links: Vec<String>,
}

impl From<&Evidence> for StoredEvidence {
    fn from(evidence: &Evidence) -> Self {
        Self {
            listing_pages: evidence.listing_pages.clone(),
            unfollowed_links: evidence.unfollowed_links.clone(),
        }
    }
}

/// What changed between the published snapshot and a candidate read. The worker builds it and
/// hands it to the model when a read needs an audit.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Review {
    pub previous_open_count: usize,
    /// Why this read needs an audit. Empty for a healthy repeat read.
    pub signals: Vec<String>,
    /// Previously open tenders that this read no longer lists.
    pub vanished: Vec<TenderSummary>,
    pub changes: Vec<FieldChange>,
    pub moved_files: Vec<MovedFile>,
    pub unfollowed_links: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TenderSummary {
    pub id: Option<String>,
    pub url: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldChange {
    pub tender: String,
    pub field: &'static str,
    pub before: serde_json::Value,
    pub after: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MovedFile {
    pub file_name: String,
    pub sha256: String,
    pub from: Vec<String>,
    pub to: String,
}

#[derive(Debug)]
pub struct CandidateRead {
    pub tenders: Vec<Tender>,
    pub evidence: Evidence,
}

#[derive(Debug, Clone)]
pub enum ReadFailure {
    Blocked(String),
    Incomplete(String),
    Temporary(String),
    /// A temporary failure where the portal said how long to wait, via `Retry-After`.
    Throttled {
        reason: String,
        retry_after: std::time::Duration,
    },
}

impl ReadFailure {
    /// Whether retrying later may succeed without any change to the recipe.
    pub fn is_temporary(&self) -> bool {
        matches!(self, Self::Temporary(_) | Self::Throttled { .. })
    }

    pub fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            Self::Throttled { retry_after, .. } => Some(*retry_after),
            _ => None,
        }
    }
}

impl std::fmt::Display for ReadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Blocked(reason)
            | Self::Incomplete(reason)
            | Self::Temporary(reason)
            | Self::Throttled { reason, .. } => f.write_str(reason),
        }
    }
}

impl std::error::Error for ReadFailure {}

#[derive(Debug, Clone, Copy)]
pub enum ModelOperation {
    Decide,
    Learn,
    Verify,
    Repair,
}

impl ModelOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Decide => "decide",
            Self::Learn => "learn",
            Self::Verify => "verify",
            Self::Repair => "repair",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ModelUsage {
    pub input: i64,
    pub cached_input: i64,
    pub output: i64,
}
