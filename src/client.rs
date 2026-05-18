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

    /// List proposals in a vote cycle. Query-string assembly delegates to
    /// reqwest's `.query()` builder, which percent-encodes pair values
    /// correctly — important for `search`, where the agent can pass arbitrary
    /// free-text (spaces, `&`, etc).
    ///
    /// Spec ref: `docs/upstream/proposals-openapi.yaml`, operationId
    /// `listProposals`. `category` is a comma-separated list of category
    /// ObjectIds; `sort` must match one of the vote cycle's `sortOptions`;
    /// `direction` is `asc`/`desc`.
    #[allow(clippy::too_many_arguments)]
    pub async fn list_proposals(
        &self,
        vote_id: &str,
        status: Option<&str>,
        page: u32,
        limit: u32,
        search: Option<&str>,
        proposer: Option<&str>,
        category: Option<&str>,
        sort: Option<&str>,
        direction: Option<&str>,
    ) -> Result<Page<Proposal>> {
        let url = format!("{}/api/v0/proposals", self.base_url);
        let page_str = page.to_string();
        let limit_str = limit.to_string();
        let mut query: Vec<(&str, &str)> = vec![
            ("vote", vote_id),
            ("page", &page_str),
            ("limit", &limit_str),
        ];
        if let Some(s) = status {
            query.push(("status", s));
        }
        if let Some(s) = search {
            query.push(("search", s));
        }
        if let Some(p) = proposer {
            query.push(("proposer", p));
        }
        if let Some(c) = category {
            query.push(("category", c));
        }
        if let Some(s) = sort {
            query.push(("sort", s));
        }
        if let Some(d) = direction {
            query.push(("direction", d));
        }
        let resp = self.http.get(&url).query(&query).send().await?;
        self.parse(resp).await
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Minimal one-shot mock that captures the first request line and replies
    /// with a 200 carrying an empty paginated payload. Returns the bound URL
    /// and a handle the test reads the captured line from.
    async fn one_shot_capture() -> (String, Arc<Mutex<Option<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let captured_clone = Arc::clone(&captured);
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            let text = String::from_utf8_lossy(&buf[..n]).to_string();
            // Grab the request line (everything up to the first CRLF).
            let request_line = text.lines().next().unwrap_or("").to_string();
            *captured_clone.lock().unwrap() = Some(request_line);
            let body = r#"{"data":[],"meta":{"page":1,"limit":20,"total":0,"totalPages":0,"hasNextPage":false,"hasPreviousPage":false}}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(resp.as_bytes()).await.unwrap();
            socket.flush().await.unwrap();
        });
        (format!("http://{addr}"), captured)
    }

    /// Build a client + run `list_proposals` against the one-shot mock, then
    /// return the captured request line for query-string assertions.
    #[allow(clippy::too_many_arguments)]
    async fn capture_list_proposals_url(
        vote_id: &str,
        status: Option<&str>,
        page: u32,
        limit: u32,
        search: Option<&str>,
        proposer: Option<&str>,
        category: Option<&str>,
        sort: Option<&str>,
        direction: Option<&str>,
    ) -> String {
        let (base, captured) = one_shot_capture().await;
        let client = EkklesiaClient::new(base, "jwt").unwrap();
        let _ = client
            .list_proposals(
                vote_id, status, page, limit, search, proposer, category, sort, direction,
            )
            .await
            .expect("mock 200");
        let line = captured.lock().unwrap().clone();
        line.expect("captured request")
    }

    #[tokio::test]
    async fn list_proposals_omits_absent_params() {
        let line = capture_list_proposals_url(
            "abc123", None, 1, 20, None, None, None, None, None,
        )
        .await;
        assert!(line.starts_with("GET /api/v0/proposals?"), "got {line:?}");
        for absent in ["status=", "search=", "proposer=", "category=", "sort=", "direction="] {
            assert!(
                !line.contains(absent),
                "omitted param {absent:?} should not appear, got {line:?}"
            );
        }
        // Required params always present.
        assert!(line.contains("vote=abc123"));
        assert!(line.contains("page=1"));
        assert!(line.contains("limit=20"));
    }

    #[tokio::test]
    async fn list_proposals_encodes_search_with_spaces() {
        let line = capture_list_proposals_url(
            "abc123",
            None,
            1,
            20,
            Some("ai finance"),
            None,
            None,
            None,
            None,
        )
        .await;
        // Spaces must percent-encode to "+" (or "%20"); the bug is bare
        // unencoded spaces in the request line, which would corrupt it.
        assert!(
            line.contains("search=ai+finance") || line.contains("search=ai%20finance"),
            "expected percent-encoded search, got {line:?}"
        );
    }

    #[tokio::test]
    async fn list_proposals_encodes_reserved_chars_in_search() {
        // & is the query-pair separator; bare & in a value would split into
        // two pairs and corrupt the next param. The encoder must escape it.
        let line = capture_list_proposals_url(
            "abc123",
            None,
            1,
            20,
            Some("a&b=c"),
            None,
            None,
            None,
            None,
        )
        .await;
        assert!(
            line.contains("search=a%26b%3Dc"),
            "expected & and = escaped in search, got {line:?}"
        );
        // The downstream limit param must still be intact after the
        // potentially-corrupting search value.
        assert!(line.contains("limit=20"), "expected limit intact, got {line:?}");
    }

    #[tokio::test]
    async fn list_proposals_all_params_present() {
        let line = capture_list_proposals_url(
            "abc123",
            Some("withdrawn"),
            2,
            50,
            Some("hello"),
            Some("stake1abc"),
            Some("cat1,cat2"),
            Some("title"),
            Some("asc"),
        )
        .await;
        for pair in [
            "vote=abc123",
            "page=2",
            "limit=50",
            "status=withdrawn",
            "search=hello",
            "proposer=stake1abc",
            // commas are reserved but reqwest leaves them unencoded in query
            // values, which is spec-legal. Accept either form.
        ] {
            assert!(line.contains(pair), "missing {pair:?} in {line:?}");
        }
        assert!(line.contains("category=cat1%2Ccat2") || line.contains("category=cat1,cat2"));
        assert!(line.contains("sort=title"));
        assert!(line.contains("direction=asc"));
    }

    #[tokio::test]
    async fn list_proposals_encodes_drep_prefix_in_proposer() {
        // drep1 / stake1 bech32 addresses are alphanumeric — should pass
        // through untouched — but the test pins that contract so a future
        // overzealous encoder change doesn't double-escape them.
        let line = capture_list_proposals_url(
            "abc123",
            None,
            1,
            20,
            None,
            Some("drep1xyz0987"),
            None,
            None,
            None,
        )
        .await;
        assert!(
            line.contains("proposer=drep1xyz0987"),
            "expected drep1 address unmolested, got {line:?}"
        );
    }
}
