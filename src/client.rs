//! HTTP client for the Ekklesia REST API.
//!
//! Authentication uses both `Authorization: Bearer <jwt>` and `Cookie: token=<jwt>`
//! because the Ekklesia backend requires both headers to accept requests.
//! The `Origin` header is set to the instance base URL to satisfy CORS checks.

use reqwest::{
    Client,
    header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue},
};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::{
    api::{Comment, CreateCommentRequest, CreateProposalRequest, Page, Proposal, Vote},
    error::{Error, Result},
};

pub struct EkklesiaClient {
    http: Client,
    base_url: String,
    #[allow(dead_code)] // retained for future token-refresh support
    jwt: String,
}

impl EkklesiaClient {
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn new(base_url: impl Into<String>, jwt: impl Into<String>) -> Result<Self> {
        let jwt = jwt.into();
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let http = Client::builder()
            .default_headers(default_headers(&base_url, &jwt)?)
            .build()?;
        Ok(Self {
            http,
            base_url,
            jwt,
        })
    }

    // ── Votes ─────────────────────────────────────────────────────────────────

    pub async fn list_votes(&self, page: u32, limit: u32) -> Result<Page<Vote>> {
        self.get(&format!(
            "{}/api/v0/votes?page={page}&limit={limit}",
            self.base_url
        ))
        .await
    }

    pub async fn get_vote(&self, id: &str) -> Result<Vote> {
        self.get(&format!("{}/api/v0/votes/{id}", self.base_url))
            .await
    }

    // ── Proposals ─────────────────────────────────────────────────────────────

    pub async fn list_proposals(
        &self,
        vote_id: &str,
        status: Option<&str>,
        page: u32,
        limit: u32,
    ) -> Result<Page<Proposal>> {
        let mut url = format!(
            "{}/api/v0/proposals?vote={vote_id}&page={page}&limit={limit}",
            self.base_url
        );
        if let Some(s) = status {
            url.push_str("&status=");
            url.push_str(s);
        }
        self.get(&url).await
    }

    pub async fn get_proposal(&self, id: &str) -> Result<Proposal> {
        self.get(&format!("{}/api/v0/proposals/{id}", self.base_url))
            .await
    }

    pub async fn create_proposal(&self, req: &CreateProposalRequest) -> Result<Value> {
        self.post(&format!("{}/api/v0/proposals", self.base_url), req)
            .await
    }

    pub async fn update_proposal(&self, id: &str, req: &CreateProposalRequest) -> Result<Value> {
        self.put(&format!("{}/api/v0/proposals/{id}", self.base_url), req)
            .await
    }

    // ── Comments ──────────────────────────────────────────────────────────────

    pub async fn list_comments(
        &self,
        proposal_id: &str,
        page: u32,
        limit: u32,
    ) -> Result<Page<Comment>> {
        self.get(&format!(
            "{}/api/v0/comments?proposal={proposal_id}&page={page}&limit={limit}",
            self.base_url
        ))
        .await
    }

    pub async fn list_comment_replies(
        &self,
        comment_id: &str,
        page: u32,
        limit: u32,
    ) -> Result<Page<Comment>> {
        self.get(&format!(
            "{}/api/v0/comments/{comment_id}/replies?page={page}&limit={limit}",
            self.base_url
        ))
        .await
    }

    pub async fn create_comment(&self, req: &CreateCommentRequest) -> Result<Value> {
        self.post(&format!("{}/api/v0/comments", self.base_url), req)
            .await
    }

    // ── HTTP helpers ──────────────────────────────────────────────────────────

    async fn get<T: DeserializeOwned>(&self, url: &str) -> Result<T> {
        let resp = self.http.get(url).send().await?;
        self.parse(resp).await
    }

    async fn post<B: serde::Serialize, T: DeserializeOwned>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T> {
        let resp = self.http.post(url).json(body).send().await?;
        self.parse(resp).await
    }

    async fn put<B: serde::Serialize, T: DeserializeOwned>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T> {
        let resp = self.http.put(url).json(body).send().await?;
        self.parse(resp).await
    }

    async fn parse<T: DeserializeOwned>(&self, resp: reqwest::Response) -> Result<T> {
        let status = resp.status();
        if status.is_success() {
            Ok(resp.json::<T>().await?)
        } else {
            let code = status.as_u16();
            let body = resp.text().await.unwrap_or_default();
            // Pretty-print JSON error bodies so nested validation details are visible;
            // fall back to the raw body if it isn't JSON.
            let message = serde_json::from_str::<serde_json::Value>(&body)
                .map(|v| {
                    serde_json::to_string_pretty(&v).unwrap_or(body.clone())
                })
                .unwrap_or(body);
            Err(Error::Api {
                status: code,
                message,
            })
        }
    }
}

fn default_headers(base_url: &str, jwt: &str) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    let bearer = format!("Bearer {jwt}");
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&bearer).map_err(|e| Error::JwtInvalid(e.to_string()))?,
    );
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        "Cookie",
        HeaderValue::from_str(&format!("token={jwt}"))
            .map_err(|e| Error::JwtInvalid(e.to_string()))?,
    );
    headers.insert(
        "Origin",
        HeaderValue::from_str(base_url).map_err(|e| Error::Parse(e.to_string()))?,
    );
    Ok(headers)
}
