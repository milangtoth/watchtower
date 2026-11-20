use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

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
    pub terminal_page: bool,
    pub document_count: usize,
    pub samples: Vec<String>,
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
}

impl std::fmt::Display for ReadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Blocked(reason) | Self::Incomplete(reason) | Self::Temporary(reason) => {
                f.write_str(reason)
            }
        }
    }
}

impl std::error::Error for ReadFailure {}

#[derive(Debug, Clone, Copy)]
pub enum ModelOperation {
    Learn,
    Verify,
    Repair,
}

impl ModelOperation {
    pub fn as_str(self) -> &'static str {
        match self {
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
