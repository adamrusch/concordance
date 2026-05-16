//! Ekklesia API request and response types.
//!
//! All paginated list endpoints return `{ "data": [...], "meta": { ... } }`.
//! Single-resource endpoints return the object directly.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── Pagination ────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct Page<T> {
    pub data: Vec<T>,
    pub meta: PageMeta,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageMeta {
    pub page: u32,
    pub limit: u32,
    pub total: u32,
    pub total_pages: u32,
    pub has_next_page: bool,
    pub has_previous_page: bool,
    pub thread_total: Option<u32>,
}

// ── Votes ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Vote {
    #[serde(rename = "_id")]
    pub id: String,
    pub slug: String,
    pub title: String,
    pub description: Option<String>,
    pub form: Option<String>,
    pub comments_enabled: Option<bool>,
    pub submission_start_date: Option<DateTime<Utc>>,
    pub submission_end_date: Option<DateTime<Utc>>,
    pub voting_start_date: Option<DateTime<Utc>>,
    pub voting_end_date: Option<DateTime<Utc>>,
    pub feedback_start_date: Option<DateTime<Utc>>,
    pub feedback_end_date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub filter_options: Vec<Value>,
    #[serde(default)]
    pub search_options: Vec<Value>,
    #[serde(default)]
    pub sort_options: Vec<Value>,
}

// ── Proposals ─────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Proposal {
    #[serde(rename = "_id")]
    pub id: String,
    pub vote_id: Option<String>,
    pub title: String,
    pub summary: Option<String>,
    pub status: String,
    pub proposer_id: Option<String>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub version: Option<u32>,
    pub comment_count: Option<u32>,
    pub meta_data: Option<Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateProposalRequest {
    pub vote_id: String,
    pub title: String,
    pub summary: String,
    pub status: String,
    pub treasury_donation_tx_hash: String,
    pub meta_data: Value,
}

// ── Comments ──────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Comment {
    #[serde(rename = "_id")]
    pub id: String,
    pub parent_id: Option<String>,
    pub content: String,
    pub created_at: Option<DateTime<Utc>>,
    pub reply_count: Option<u32>,
    pub like_count: Option<u32>,
    pub author: Option<CommentAuthor>,
}

#[derive(Debug, Deserialize)]
pub struct CommentAuthor {
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub author_type: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateCommentRequest {
    pub proposal_id: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
}

// ── API error response ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ApiErrorBody {
    pub message: Option<String>,
    pub status: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_deserializes_with_nested_meta() {
        let json = r#"{
            "data": [],
            "meta": {
                "page": 1,
                "limit": 20,
                "total": 0,
                "totalPages": 0,
                "hasNextPage": false,
                "hasPreviousPage": false
            }
        }"#;
        let page: Page<serde_json::Value> = serde_json::from_str(json).unwrap();
        assert_eq!(page.meta.total, 0);
        assert_eq!(page.meta.page, 1);
        assert!(!page.meta.has_next_page);
    }

    #[test]
    fn page_with_thread_total_in_meta() {
        // Comments endpoint includes threadTotal
        let json = r#"{
            "data": [],
            "meta": {
                "page": 1, "limit": 50, "total": 0, "threadTotal": 0,
                "totalPages": 0, "hasNextPage": false, "hasPreviousPage": false
            }
        }"#;
        let page: Page<serde_json::Value> = serde_json::from_str(json).unwrap();
        assert_eq!(page.meta.thread_total, Some(0));
    }

    #[test]
    fn page_without_thread_total_is_none() {
        let json = r#"{
            "data": [],
            "meta": {
                "page": 1, "limit": 20, "total": 5,
                "totalPages": 1, "hasNextPage": false, "hasPreviousPage": false
            }
        }"#;
        let page: Page<serde_json::Value> = serde_json::from_str(json).unwrap();
        assert!(page.meta.thread_total.is_none());
    }

    #[test]
    fn proposal_deserializes_minimal() {
        let json = r#"{
            "_id": "abc123",
            "title": "Test Proposal",
            "status": "live"
        }"#;
        let p: Proposal = serde_json::from_str(json).unwrap();
        assert_eq!(p.id, "abc123");
        assert_eq!(p.title, "Test Proposal");
        assert!(p.summary.is_none());
        assert!(p.meta_data.is_none());
    }

    #[test]
    fn comment_author_deserializes() {
        let json = r#"{
            "_id": "cmt1",
            "content": "Great proposal",
            "author": {"name": "Alice", "type": "user"}
        }"#;
        let c: Comment = serde_json::from_str(json).unwrap();
        assert_eq!(c.content, "Great proposal");
        assert_eq!(c.author.unwrap().name.unwrap(), "Alice");
    }

    #[test]
    fn vote_deserializes_with_optional_fields_absent() {
        let json = r#"{
            "_id": "vote1",
            "slug": "test-vote",
            "title": "Test Vote"
        }"#;
        let v: Vote = serde_json::from_str(json).unwrap();
        assert_eq!(v.id, "vote1");
        assert!(v.form.is_none());
        assert!(v.voting_start_date.is_none());
    }
}
