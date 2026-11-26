//! GitHub through its HTTP API (replaces the `gh` command-line tool).
//!
//! Blocking, like the rest of the core: run on a background thread. The
//! network sits behind [`Transport`], so tests use a fake. Responses are
//! parsed with serde into the types in [`crate::gh`] (kept so the UI did not
//! have to change), with missing fields optional.
//!
//! REST is used throughout, except the pull request list and details,
//! which take one GraphQL query each: REST has no review decision or
//! check roll-up, which would cost two or three more requests per pull
//! request.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::gh::{
    Author, BranchRef, Check, Comment, MergeMethod, NewPr, PrAction, PrDetail, PrFilter,
    PrSummary, RateLimited, RepoInfo, Review, RollupItem, Run, RunDetail,
};
use crate::git::Git;

pub const API: &str = "https://api.github.com";
pub const API_VERSION: &str = "2022-11-28";
pub const USER_AGENT: &str = "AnvegCode";

// ---------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    /// Header names in lower case.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Sends one HTTP request. Any status is a response, not an error; only a
/// failure to talk to the server is an `Err`.
pub trait Transport: Send + Sync {
    fn send(&self, request: &Request) -> Result<Response>;
}

/// The real network: ureq over rustls, trusting the system's certificates,
/// honouring `HTTPS_PROXY`, following redirects (without the token when the
/// redirect leaves the host).
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl UreqTransport {
    pub fn new() -> Self {
        let tls = ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::Rustls)
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .unversioned_rustls_crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .build();
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(120)))
            .tls_config(tls)
            .build()
            .new_agent();
        Self { agent }
    }
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new()
    }
}

/// Bodies are capped (CI logs can be large).
const MAX_BODY: u64 = 64 * 1024 * 1024;

impl Transport for UreqTransport {
    fn send(&self, request: &Request) -> Result<Response> {
        let mut builder = ureq::http::Request::builder()
            .method(request.method)
            .uri(&request.url);
        for (k, v) in &request.headers {
            builder = builder.header(k, v);
        }
        let http = builder
            .body(request.body.clone().unwrap_or_default())
            .context("building the request")?;
        let mut response = self
            .agent
            .run(http)
            .map_err(|e| anyhow!("Could not reach GitHub: {e}"))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_ascii_lowercase(),
                    String::from_utf8_lossy(v.as_bytes()).into_owned(),
                )
            })
            .collect();
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_BODY)
            .read_to_vec()
            .map_err(|e| anyhow!("Could not read GitHub's answer: {e}"))?;
        Ok(Response {
            status,
            headers,
            body,
        })
    }
}

// ---------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------

/// The token is missing, expired or revoked (HTTP 401).
#[derive(Debug)]
pub struct SignedOut;

impl std::fmt::Display for SignedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Not signed in to GitHub (the token is missing, expired or revoked).")
    }
}

impl std::error::Error for SignedOut {}

/// GitHub's error for one request, other than sign-in and rate limits.
#[derive(Debug, PartialEq, Eq)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

pub fn is_signed_out(e: &anyhow::Error) -> bool {
    e.downcast_ref::<SignedOut>().is_some()
}

pub fn api_status(e: &anyhow::Error) -> Option<u16> {
    e.downcast_ref::<ApiError>().map(|a| a.status)
}

/// GitHub's `{"message": …, "errors": [{"message"|"code"|"field": …}]}`.
fn error_message(body: &[u8]) -> String {
    let Ok(v) = serde_json::from_slice::<Value>(body) else {
        return String::from_utf8_lossy(body).trim().chars().take(300).collect();
    };
    let mut message = v["message"].as_str().unwrap_or("").to_owned();
    if let Some(errors) = v["errors"].as_array() {
        let details: Vec<String> = errors
            .iter()
            .filter_map(|e| {
                e["message"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| {
                        let code = e["code"].as_str()?;
                        Some(match e["field"].as_str() {
                            Some(field) => format!("{field} {code}"),
                            None => code.to_owned(),
                        })
                    })
                    .or_else(|| e.as_str().map(str::to_owned))
            })
            .collect();
        if !details.is_empty() {
            message = format!("{message}: {}", details.join("; "));
        }
    }
    message
}

/// Turn a non-success response into an error: 401 → [`SignedOut`]; 403 or
/// 429 that is a rate limit → [`RateLimited`]; anything else →
/// [`ApiError`] with a readable message.
pub fn check(response: Response) -> Result<Response> {
    if (200..300).contains(&response.status) {
        return Ok(response);
    }
    let message = error_message(&response.body);
    let rate_limited = response.status == 429
        || (response.status == 403
            && (response.header("x-ratelimit-remaining") == Some("0")
                || response.header("retry-after").is_some()
                || crate::gh::is_rate_limited(&message)));
    if response.status == 401 {
        return Err(SignedOut.into());
    }
    if rate_limited {
        let when = response
            .header("x-ratelimit-reset")
            .and_then(|r| r.parse::<i64>().ok())
            .map(|reset| {
                let mins = (reset - crate::time::now_millis() / 1000).max(0) / 60 + 1;
                format!(" Try again in about {mins} min.")
            })
            .unwrap_or_default();
        return Err(RateLimited(format!("GitHub's rate limit was reached.{when}")).into());
    }
    let message = match (response.status, message.is_empty()) {
        (404, _) => format!(
            "Not found on GitHub{}. The repository may be private to another account, or the token may lack access.",
            if message.is_empty() { String::new() } else { format!(" ({message})") }
        ),
        (_, true) => format!("GitHub answered HTTP {}.", response.status),
        _ => message,
    };
    Err(ApiError {
        status: response.status,
        message,
    }
    .into())
}

/// The `rel="next"` URL of a `Link` header.
pub fn next_link(link: &str) -> Option<String> {
    link.split(',').find_map(|part| {
        let (url, params) = part.split_once(';')?;
        let is_next = params
            .split(';')
            .any(|p| p.trim().replace(' ', "") == "rel=\"next\"");
        is_next.then(|| {
            url.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_owned()
        })
    })
}

// ---------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------

/// A token, never printed.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(…)")
    }
}

#[derive(Clone)]
pub struct Api {
    transport: Arc<dyn Transport>,
    token: Option<Secret>,
    base: String,
}

impl Api {
    pub fn new(transport: Arc<dyn Transport>, token: Option<Secret>) -> Self {
        Self {
            transport,
            token,
            base: api_base(),
        }
    }

    fn url(&self, path: &str) -> String {
        if path.starts_with("https://") || path.starts_with("http://") {
            path.to_owned()
        } else {
            format!("{}{path}", self.base)
        }
    }

    fn request(&self, method: &'static str, path: &str, accept: &str, body: Option<&Value>) -> Result<Response> {
        let mut headers = vec![
            ("Accept".to_owned(), accept.to_owned()),
            ("X-GitHub-Api-Version".to_owned(), API_VERSION.to_owned()),
            ("User-Agent".to_owned(), USER_AGENT.to_owned()),
        ];
        if let Some(token) = &self.token {
            headers.push(("Authorization".to_owned(), format!("Bearer {}", token.expose())));
        }
        let body = match body {
            Some(b) => {
                headers.push(("Content-Type".to_owned(), "application/json".to_owned()));
                Some(serde_json::to_vec(b)?)
            }
            None => None,
        };
        let response = self.transport.send(&Request {
            method,
            url: self.url(path),
            headers,
            body,
        })?;
        check(response)
    }

    pub fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let r = self.request("GET", path, "application/vnd.github+json", None)?;
        serde_json::from_slice(&r.body).with_context(|| format!("reading GitHub's answer to {path}"))
    }

    /// GET with a different `Accept` (a diff, a log), as text.
    pub fn get_text(&self, path: &str, accept: &str) -> Result<String> {
        Ok(self.request("GET", path, accept, None)?.text())
    }

    /// Follow `Link: rel="next"` until `max` items (arrays, or an object's
    /// array field `key`).
    pub fn get_paged<T: DeserializeOwned>(&self, path: &str, key: Option<&str>, max: usize) -> Result<Vec<T>> {
        let mut out = Vec::new();
        let mut next = Some(self.url(path));
        while let Some(url) = next.take() {
            let r = self.request("GET", &url, "application/vnd.github+json", None)?;
            let v: Value = serde_json::from_slice(&r.body)?;
            let items = match key {
                Some(k) => v[k].clone(),
                None => v,
            };
            let page: Vec<T> = serde_json::from_value(items).context("reading a page of results")?;
            let empty = page.is_empty();
            out.extend(page);
            if out.len() >= max || empty {
                out.truncate(max);
                break;
            }
            next = r.header("link").and_then(next_link);
        }
        Ok(out)
    }

    pub fn send_json<T: DeserializeOwned>(&self, method: &'static str, path: &str, body: &Value) -> Result<T> {
        let r = self.request(method, path, "application/vnd.github+json", Some(body))?;
        if r.body.is_empty() {
            return serde_json::from_value(Value::Null).context("empty answer");
        }
        serde_json::from_slice(&r.body).with_context(|| format!("reading GitHub's answer to {path}"))
    }

    /// POST/PATCH/PUT whose answer is not needed.
    pub fn send(&self, method: &'static str, path: &str, body: &Value) -> Result<()> {
        self.request(method, path, "application/vnd.github+json", Some(body))
            .map(drop)
    }

    /// One GraphQL query. Errors in the answer become an error.
    pub fn graphql<T: DeserializeOwned>(&self, query: &str, variables: Value) -> Result<T> {
        let v: Value = self.send_json("POST", "/graphql", &json!({ "query": query, "variables": variables }))?;
        if let Some(errors) = v["errors"].as_array().filter(|e| !e.is_empty()) {
            let message: Vec<&str> = errors.iter().filter_map(|e| e["message"].as_str()).collect();
            bail!(ApiError {
                status: 200,
                message: message.join("; "),
            });
        }
        serde_json::from_value(v["data"].clone()).context("reading GitHub's GraphQL answer")
    }
}

/// `https://api.github.com`; debug builds honour `ANVEG_GITHUB_API` (a
/// fake server for testing the app end to end).
fn api_base() -> String {
    if cfg!(debug_assertions)
        && let Ok(base) = std::env::var("ANVEG_GITHUB_API")
        && !base.is_empty()
    {
        return base.trim_end_matches('/').to_owned();
    }
    API.to_owned()
}

// ---------------------------------------------------------------------
// Which repository
// ---------------------------------------------------------------------

/// `owner/name` from a GitHub remote URL: `https://github.com/o/r(.git)`,
/// `git@github.com:o/r.git`, `ssh://git@github.com/o/r.git`, `git://…`.
/// Other hosts give `None`.
pub fn parse_remote(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    let rest = if let Some(r) = url.strip_prefix("git@github.com:") {
        r
    } else {
        let after_scheme = url.split_once("://").map(|(_, r)| r)?;
        let after_user = after_scheme.rsplit_once('@').map_or(after_scheme, |(_, r)| r);
        let (host, path) = after_user.split_once('/')?;
        let host = host.split(':').next().unwrap_or(host);
        if !host.eq_ignore_ascii_case("github.com") && !host.eq_ignore_ascii_case("www.github.com") {
            return None;
        }
        path
    };
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let mut parts = rest.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    if owner.is_empty() || name.is_empty() || parts.next().is_some() {
        return None;
    }
    Some((owner.to_owned(), name.to_owned()))
}

/// `owner/name` typed or pasted: `owner/name`, or any form of URL.
pub fn parse_slug(text: &str) -> Option<(String, String)> {
    let t = text.trim();
    if let Some(slug) = parse_remote(t) {
        return Some(slug);
    }
    let t = t
        .strip_prefix("github.com/")
        .or_else(|| t.strip_prefix("www.github.com/"))
        .unwrap_or(t);
    let t = t.trim_end_matches('/');
    let t = t.strip_suffix(".git").unwrap_or(t);
    let (owner, name) = t.split_once('/')?;
    let valid = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    (valid(owner) && valid(name)).then(|| (owner.to_owned(), name.to_owned()))
}

// ---------------------------------------------------------------------
// REST and GraphQL answers
// ---------------------------------------------------------------------

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct User {
    pub login: String,
    pub name: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Org {
    pub login: String,
}

/// A repository in the user's list (Open from GitHub).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RepoItem {
    pub full_name: String,
    pub name: String,
    pub description: Option<String>,
    pub private: bool,
    pub fork: bool,
    pub clone_url: String,
    pub html_url: String,
    pub updated_at: Option<String>,
    pub default_branch: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RestRepo {
    full_name: String,
    html_url: String,
    default_branch: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RestPull {
    number: u64,
    node_id: String,
    html_url: String,
    head: RestHead,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RestHead {
    #[serde(rename = "ref")]
    name: String,
    sha: String,
    repo: Option<RestRepo>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RestCheckRun {
    name: String,
    status: String,
    conclusion: Option<String>,
    html_url: Option<String>,
    details_url: Option<String>,
    app: Option<RestApp>,
    output: Option<RestOutput>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RestApp {
    name: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RestOutput {
    title: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RestStatus {
    context: String,
    state: String,
    target_url: Option<String>,
    description: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RestCombinedStatus {
    statuses: Vec<RestStatus>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RestRun {
    id: u64,
    display_title: String,
    name: Option<String>,
    status: Option<String>,
    conclusion: Option<String>,
    event: String,
    head_branch: Option<String>,
    created_at: Option<String>,
    html_url: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RestJob {
    id: u64,
    name: String,
    status: Option<String>,
    conclusion: Option<String>,
    html_url: Option<String>,
}

fn run_from_rest(r: RestRun) -> Run {
    Run {
        database_id: r.id,
        display_title: r.display_title,
        status: r.status.unwrap_or_default(),
        conclusion: r.conclusion.unwrap_or_default(),
        workflow_name: r.name.unwrap_or_default(),
        event: r.event,
        head_branch: r.head_branch.unwrap_or_default(),
        created_at: r.created_at,
        url: r.html_url,
    }
}

/// gh's check "bucket" from a check run's status and conclusion.
fn check_run_bucket(status: &str, conclusion: Option<&str>) -> &'static str {
    if !status.eq_ignore_ascii_case("completed") {
        return "pending";
    }
    match conclusion.unwrap_or("").to_lowercase().as_str() {
        "success" => "pass",
        "skipped" | "neutral" => "skipping",
        "cancelled" => "cancel",
        "" | "stale" => "pending",
        _ => "fail",
    }
}

fn status_bucket(state: &str) -> &'static str {
    match state.to_lowercase().as_str() {
        "success" => "pass",
        "failure" | "error" => "fail",
        _ => "pending",
    }
}

// GraphQL shapes, flattened into the gh-era types.

const ROLLUP: &str = "commits(last: 1) { nodes { commit { statusCheckRollup { contexts(first: 100) { nodes { __typename ... on CheckRun { name status conclusion } ... on StatusContext { context state } } } } } } }";

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GqlNodes<T> {
    nodes: Vec<T>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct GqlCommitNode {
    commit: GqlCommit,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct GqlCommit {
    status_check_rollup: Option<GqlRollup>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GqlRollup {
    contexts: GqlNodes<RollupItem>,
}

fn rollup(commits: GqlNodes<GqlCommitNode>) -> Vec<RollupItem> {
    commits
        .nodes
        .into_iter()
        .next()
        .and_then(|c| c.commit.status_check_rollup)
        .map(|r| r.contexts.nodes)
        .unwrap_or_default()
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct GqlPr {
    number: u64,
    title: String,
    author: Option<Author>,
    body: String,
    base_ref_name: String,
    head_ref_name: String,
    is_draft: bool,
    state: String,
    review_decision: Option<String>,
    url: String,
    updated_at: Option<String>,
    additions: u64,
    deletions: u64,
    changed_files: u64,
    reviews: GqlNodes<Review>,
    comments: GqlNodes<Comment>,
    commits: GqlNodes<GqlCommitNode>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GqlSearch {
    search: GqlNodes<GqlPr>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct GqlRepoPr {
    repository: Option<GqlRepoPrInner>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct GqlRepoPrInner {
    pull_request: Option<GqlPr>,
}

fn pr_summary(p: GqlPr) -> PrSummary {
    PrSummary {
        number: p.number,
        title: p.title,
        author: p.author,
        head_ref_name: p.head_ref_name,
        is_draft: p.is_draft,
        review_decision: p.review_decision,
        status_check_rollup: rollup(p.commits),
        updated_at: p.updated_at,
    }
}

fn pr_detail(p: GqlPr) -> PrDetail {
    PrDetail {
        number: p.number,
        title: p.title,
        author: p.author,
        body: p.body,
        base_ref_name: p.base_ref_name,
        head_ref_name: p.head_ref_name,
        is_draft: p.is_draft,
        state: p.state,
        review_decision: p.review_decision,
        url: p.url,
        reviews: p.reviews.nodes,
        comments: p.comments.nodes,
        status_check_rollup: rollup(p.commits),
        additions: p.additions,
        deletions: p.deletions,
        changed_files: p.changed_files,
    }
}

// ---------------------------------------------------------------------
// The operations (what gh did)
// ---------------------------------------------------------------------

/// A new repository (Publish to GitHub).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewRepo {
    /// `None`: the signed-in user's account; else an organization.
    pub org: Option<String>,
    pub name: String,
    pub description: String,
    pub private: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CreatedRepo {
    pub full_name: String,
    pub html_url: String,
    pub clone_url: String,
}

/// GitHub for one app session: the API plus git (to find the repository
/// from the `origin` remote, and to check out pull requests).
#[derive(Clone)]
pub struct GitHub {
    pub api: Api,
    pub git: Git,
}

impl GitHub {
    pub fn new(api: Api, git: Git) -> Self {
        Self { api, git }
    }

    /// `owner/name` of `dir`'s `origin` (or its first GitHub remote).
    pub fn slug(&self, dir: &Path) -> Result<(String, String)> {
        let remotes = self.git.remotes(dir).unwrap_or_default();
        let ordered = remotes
            .iter()
            .filter(|r| *r == "origin")
            .chain(remotes.iter().filter(|r| *r != "origin"));
        for remote in ordered {
            if let Ok(url) = self.git.remote_url(dir, remote)
                && let Some(slug) = parse_remote(&url)
            {
                return Ok(slug);
            }
        }
        bail!("This repository has no GitHub remote (origin).")
    }

    fn repo_path(&self, dir: &Path) -> Result<String> {
        let (o, r) = self.slug(dir)?;
        Ok(format!("/repos/{o}/{r}"))
    }

    pub fn user(&self) -> Result<User> {
        self.api.get("/user")
    }

    pub fn orgs(&self) -> Result<Vec<Org>> {
        self.api.get_paged("/user/orgs?per_page=100", None, 500)
    }

    /// The user's repositories, most recently updated first.
    pub fn user_repos(&self, max: usize) -> Result<Vec<RepoItem>> {
        self.api.get_paged(
            "/user/repos?per_page=100&sort=updated&affiliation=owner,collaborator,organization_member",
            None,
            max,
        )
    }

    pub fn repo_by_slug(&self, owner: &str, name: &str) -> Result<RepoItem> {
        self.api.get(&format!("/repos/{owner}/{name}"))
    }

    pub fn create_repo(&self, new: &NewRepo) -> Result<CreatedRepo> {
        let path = match &new.org {
            Some(org) => format!("/orgs/{org}/repos"),
            None => "/user/repos".to_owned(),
        };
        let body = json!({
            "name": new.name,
            "description": new.description,
            "private": new.private,
            "auto_init": false,
        });
        self.api.send_json("POST", &path, &body).map_err(|e| {
            if api_status(&e) == Some(422) && e.to_string().to_lowercase().contains("already exists") {
                anyhow!(
                    "A repository named “{}” already exists in {}. Choose another name.",
                    new.name,
                    new.org.as_deref().unwrap_or("your account")
                )
            } else {
                e
            }
        })
    }

    pub fn repo(&self, dir: &Path) -> Result<RepoInfo> {
        let r: RestRepo = self.api.get(&self.repo_path(dir)?)?;
        Ok(RepoInfo {
            name_with_owner: r.full_name,
            url: r.html_url,
            default_branch_ref: r.default_branch.map(|name| BranchRef { name }),
        })
    }

    pub fn pr_list(&self, dir: &Path, filter: PrFilter) -> Result<Vec<PrSummary>> {
        let (o, r) = self.slug(dir)?;
        let q = format!("repo:{o}/{r} is:pr {}", filter.search());
        let query = format!(
            "query($q: String!) {{ search(query: $q, type: ISSUE, first: 50) {{ nodes {{ ... on PullRequest {{ number title author {{ login }} headRefName isDraft reviewDecision updatedAt {ROLLUP} }} }} }} }}"
        );
        let found: GqlSearch = self.api.graphql(&query, json!({ "q": q }))?;
        Ok(found
            .search
            .nodes
            .into_iter()
            .filter(|p| p.number > 0)
            .map(pr_summary)
            .collect())
    }

    pub fn pr_view(&self, dir: &Path, number: u64) -> Result<PrDetail> {
        let (o, r) = self.slug(dir)?;
        let query = format!(
            "query($o: String!, $r: String!, $n: Int!) {{ repository(owner: $o, name: $r) {{ pullRequest(number: $n) {{ number title author {{ login }} body baseRefName headRefName isDraft state reviewDecision url additions deletions changedFiles reviews(last: 50) {{ nodes {{ author {{ login }} state body submittedAt }} }} comments(last: 50) {{ nodes {{ author {{ login }} body createdAt }} }} {ROLLUP} }} }} }}"
        );
        let found: GqlRepoPr = self.api.graphql(&query, json!({ "o": o, "r": r, "n": number }))?;
        found
            .repository
            .and_then(|r| r.pull_request)
            .map(pr_detail)
            .ok_or_else(|| anyhow!("Pull request #{number} was not found."))
    }

    fn pull(&self, dir: &Path, number: u64) -> Result<RestPull> {
        self.api.get(&format!("{}/pulls/{number}", self.repo_path(dir)?))
    }

    /// Check runs and commit statuses of the PR's head commit.
    pub fn pr_checks(&self, dir: &Path, number: u64) -> Result<Vec<Check>> {
        let base = self.repo_path(dir)?;
        let sha = self.pull(dir, number)?.head.sha;
        let runs: Vec<RestCheckRun> = self.api.get_paged(
            &format!("{base}/commits/{sha}/check-runs?per_page=100"),
            Some("check_runs"),
            500,
        )?;
        let combined: RestCombinedStatus = self.api.get(&format!("{base}/commits/{sha}/status"))?;
        let mut out: Vec<Check> = runs
            .into_iter()
            .map(|c| Check {
                bucket: check_run_bucket(&c.status, c.conclusion.as_deref()).to_owned(),
                state: c
                    .conclusion
                    .clone()
                    .unwrap_or_else(|| c.status.clone())
                    .to_uppercase(),
                link: c.html_url.or(c.details_url).unwrap_or_default(),
                workflow: c.app.map(|a| a.name).unwrap_or_default(),
                description: c.output.and_then(|o| o.title).unwrap_or_default(),
                name: c.name,
            })
            .collect();
        out.extend(combined.statuses.into_iter().map(|s| Check {
            bucket: status_bucket(&s.state).to_owned(),
            state: s.state.to_uppercase(),
            link: s.target_url.unwrap_or_default(),
            workflow: String::new(),
            description: s.description.unwrap_or_default(),
            name: s.context,
        }));
        Ok(out)
    }

    pub fn pr_diff(&self, dir: &Path, number: u64) -> Result<String> {
        self.api.get_text(
            &format!("{}/pulls/{number}", self.repo_path(dir)?),
            "application/vnd.github.diff",
        )
    }

    pub fn pr_action(&self, dir: &Path, number: u64, action: &PrAction) -> Result<String> {
        let base = self.repo_path(dir)?;
        match action {
            PrAction::Merge(method) => {
                let m = match method {
                    MergeMethod::Merge => "merge",
                    MergeMethod::Squash => "squash",
                    MergeMethod::Rebase => "rebase",
                };
                self.api.send("PUT", &format!("{base}/pulls/{number}/merge"), &json!({ "merge_method": m }))?;
                Ok(format!("Merged pull request #{number}."))
            }
            PrAction::Close => {
                self.api.send("PATCH", &format!("{base}/pulls/{number}"), &json!({ "state": "closed" }))?;
                Ok(format!("Closed pull request #{number}."))
            }
            PrAction::Ready => {
                let pr = self.pull(dir, number)?;
                let _: Value = self.api.graphql(
                    "mutation($id: ID!) { markPullRequestReadyForReview(input: { pullRequestId: $id }) { clientMutationId } }",
                    json!({ "id": pr.node_id }),
                )?;
                Ok(format!("Pull request #{number} is ready for review."))
            }
            PrAction::Checkout => {
                let pr = self.pull(dir, number)?;
                let (o, r) = self.slug(dir)?;
                let same_repo = pr
                    .head
                    .repo
                    .as_ref()
                    .is_some_and(|h| h.full_name.eq_ignore_ascii_case(&format!("{o}/{r}")));
                let remote = self.git.github_remote_name(dir, &o, &r).unwrap_or_else(|| "origin".into());
                let branch = if same_repo {
                    self.git.fetch_refspec(dir, &remote, &pr.head.name)?;
                    pr.head.name.clone()
                } else {
                    // A fork: its head lives at refs/pull/N/head.
                    let local = format!("pr-{}-{}", pr.number, pr.head.name);
                    self.git
                        .fetch_refspec(dir, &remote, &format!("+refs/pull/{number}/head:refs/heads/{local}"))?;
                    local
                };
                self.git.switch_branch(dir, &branch, same_repo.then_some(remote.as_str()))?;
                Ok(format!("Switched to {branch} (pull request #{number})."))
            }
        }
    }

    /// Open a pull request from the current branch; its URL.
    pub fn pr_create(&self, dir: &Path, new: &NewPr) -> Result<String> {
        let head = self
            .git
            .current_branch(dir)
            .ok_or_else(|| anyhow!("Not on a branch."))?;
        let pr: RestPull = self.api.send_json(
            "POST",
            &format!("{}/pulls", self.repo_path(dir)?),
            &json!({
                "title": new.title,
                "body": new.body,
                "head": head,
                "base": new.base,
                "draft": new.draft,
            }),
        )?;
        Ok(pr.html_url)
    }

    pub fn run_list(&self, dir: &Path, branch: &str) -> Result<Vec<Run>> {
        let base = self.repo_path(dir)?;
        let branch_q = branch.replace(' ', "%20");
        let runs: Vec<RestRun> = self.api.get_paged(
            &format!("{base}/actions/runs?branch={branch_q}&per_page=20"),
            Some("workflow_runs"),
            20,
        )?;
        Ok(runs.into_iter().map(run_from_rest).collect())
    }

    pub fn run_view(&self, dir: &Path, id: u64) -> Result<RunDetail> {
        let base = self.repo_path(dir)?;
        let run: RestRun = self.api.get(&format!("{base}/actions/runs/{id}"))?;
        let jobs: Vec<RestJob> = self.api.get_paged(
            &format!("{base}/actions/runs/{id}/jobs?per_page=100"),
            Some("jobs"),
            500,
        )?;
        let run = run_from_rest(run);
        Ok(RunDetail {
            database_id: run.database_id,
            display_title: run.display_title,
            status: run.status,
            conclusion: run.conclusion,
            workflow_name: run.workflow_name,
            head_branch: run.head_branch,
            url: run.url,
            jobs: jobs
                .into_iter()
                .map(|j| crate::gh::Job {
                    database_id: j.id,
                    name: j.name,
                    status: j.status.unwrap_or_default(),
                    conclusion: j.conclusion.unwrap_or_default(),
                    url: j.html_url.unwrap_or_default(),
                })
                .collect(),
        })
    }

    /// The logs of the run's failed jobs, one after the other.
    pub fn run_failed_log(&self, dir: &Path, id: u64) -> Result<String> {
        let base = self.repo_path(dir)?;
        let detail = self.run_view(dir, id)?;
        let mut out = String::new();
        for job in detail.jobs.iter().filter(|j| j.state() == crate::gh::RunState::Failure) {
            let log = self.api.get_text(
                &format!("{base}/actions/jobs/{}/logs", job.database_id),
                "application/vnd.github+json",
            )?;
            out.push_str(&format!("──── {} ────\n", job.name));
            out.push_str(&log);
            if !out.ends_with('\n') {
                out.push('\n');
            }
        }
        if out.is_empty() {
            out.push_str("No failed jobs in this run.\n");
        }
        Ok(out)
    }

    pub fn run_rerun_failed(&self, dir: &Path, id: u64) -> Result<()> {
        self.api.send(
            "POST",
            &format!("{}/actions/runs/{id}/rerun-failed-jobs", self.repo_path(dir)?),
            &json!({}),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Answers by URL substring, records what was sent.
    #[derive(Default)]
    pub struct FakeTransport {
        pub answers: Vec<(String, Response)>,
        pub sent: Mutex<Vec<Request>>,
    }

    impl FakeTransport {
        fn answer(mut self, url_part: &str, status: u16, body: &str) -> Self {
            self.answers.push((
                url_part.to_owned(),
                Response {
                    status,
                    headers: Vec::new(),
                    body: body.as_bytes().to_vec(),
                },
            ));
            self
        }

        fn answer_with(mut self, url_part: &str, r: Response) -> Self {
            self.answers.push((url_part.to_owned(), r));
            self
        }
    }

    impl Transport for FakeTransport {
        fn send(&self, request: &Request) -> Result<Response> {
            if let Ok(mut s) = self.sent.lock() {
                s.push(request.clone());
            }
            self.answers
                .iter()
                .find(|(part, _)| request.url.contains(part.as_str()))
                .map(|(_, r)| r.clone())
                .ok_or_else(|| anyhow!("no fake answer for {}", request.url))
        }
    }

    fn api(t: FakeTransport) -> (Api, Arc<FakeTransport>) {
        let t = Arc::new(t);
        (Api::new(t.clone(), Some(Secret::new("tok"))), t)
    }

    fn resp(status: u16, headers: &[(&str, &str)], body: &str) -> Response {
        Response {
            status,
            headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            body: body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn remote_urls() {
        let s = |o: &str, r: &str| Some((o.to_owned(), r.to_owned()));
        assert_eq!(parse_remote("https://github.com/o/r.git"), s("o", "r"));
        assert_eq!(parse_remote("https://github.com/o/r"), s("o", "r"));
        assert_eq!(parse_remote("https://github.com/o/r/"), s("o", "r"));
        assert_eq!(parse_remote("https://user:tok@github.com/o/r.git"), s("o", "r"));
        assert_eq!(parse_remote("git@github.com:o/my.repo.git"), s("o", "my.repo"));
        assert_eq!(parse_remote("ssh://git@github.com/o/r.git"), s("o", "r"));
        assert_eq!(parse_remote("ssh://git@github.com:22/o/r.git"), s("o", "r"));
        assert_eq!(parse_remote("git://github.com/o/r.git"), s("o", "r"));
        assert_eq!(parse_remote("https://gitlab.com/o/r.git"), None);
        assert_eq!(parse_remote("https://github.com/o"), None);
        assert_eq!(parse_remote("/local/path"), None);
        assert_eq!(parse_slug("o/r"), s("o", "r"));
        assert_eq!(parse_slug("github.com/o/r"), s("o", "r"));
        assert_eq!(parse_slug(" https://github.com/o/r.git "), s("o", "r"));
        assert_eq!(parse_slug("not a repo"), None);
    }

    #[test]
    fn link_header_pagination() -> Result<()> {
        let link = r#"<https://api.github.com/user/repos?page=2>; rel="next", <https://api.github.com/user/repos?page=3>; rel="last""#;
        assert_eq!(next_link(link).as_deref(), Some("https://api.github.com/user/repos?page=2"));
        assert_eq!(next_link(r#"<https://x/?page=1>; rel="prev""#), None);
        let page1 = resp(200, &[("link", r#"<https://api.github.com/items?page=2>; rel="next""#)], r#"[{"login":"a"},{"login":"b"}]"#);
        let page2 = resp(200, &[], r#"[{"login":"c"}]"#);
        let (api, t) = api(FakeTransport::default().answer_with("page=2", page2).answer_with("/items", page1));
        let all: Vec<Org> = api.get_paged("/items", None, 10)?;
        assert_eq!(all.iter().map(|o| o.login.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
        // Stops at `max` without fetching more.
        let two: Vec<Org> = api.get_paged("/items", None, 2)?;
        assert_eq!(two.len(), 2);
        let sent = t.sent.lock().map(|s| s.len()).unwrap_or(0);
        assert_eq!(sent, 3);
        Ok(())
    }

    #[test]
    fn requests_carry_the_headers() -> Result<()> {
        let (api, t) = api(FakeTransport::default().answer("/user", 200, r#"{"login":"me"}"#));
        let u: User = api.get("/user")?;
        assert_eq!(u.login, "me");
        let sent = t.sent.lock().map_err(|_| anyhow!("lock"))?;
        let h = &sent[0].headers;
        let has = |k: &str, v: &str| h.iter().any(|(a, b)| a == k && b == v);
        assert!(has("Authorization", "Bearer tok"));
        assert!(has("Accept", "application/vnd.github+json"));
        assert!(has("X-GitHub-Api-Version", API_VERSION));
        assert!(has("User-Agent", USER_AGENT));
        assert_eq!(sent[0].url, "https://api.github.com/user");
        // The token never shows in debug output.
        assert_eq!(format!("{:?}", Secret::new("tok")), "Secret(…)");
        Ok(())
    }

    #[test]
    fn error_mapping() {
        let e = check(resp(401, &[], r#"{"message":"Bad credentials"}"#)).err();
        assert!(e.as_ref().is_some_and(is_signed_out));
        let e = check(resp(403, &[("x-ratelimit-remaining", "0"), ("x-ratelimit-reset", "0")], r#"{"message":"API rate limit exceeded for user"}"#)).err();
        assert!(e.as_ref().is_some_and(|e| e.downcast_ref::<RateLimited>().is_some()));
        let e = check(resp(429, &[], "")).err();
        assert!(e.as_ref().is_some_and(|e| e.downcast_ref::<RateLimited>().is_some()));
        // A 403 that is not a rate limit (no permission) stays an API error.
        let e = check(resp(403, &[], r#"{"message":"Resource not accessible by integration"}"#)).err();
        assert_eq!(e.as_ref().and_then(api_status), Some(403));
        let e = check(resp(404, &[], r#"{"message":"Not Found"}"#)).err();
        assert_eq!(e.as_ref().and_then(api_status), Some(404));
        assert!(e.is_some_and(|e| e.to_string().starts_with("Not found on GitHub")));
        let e = check(resp(
            422,
            &[],
            r#"{"message":"Repository creation failed.","errors":[{"resource":"Repository","code":"custom","field":"name","message":"name already exists on this account"}]}"#,
        ))
        .err();
        assert_eq!(e.as_ref().and_then(api_status), Some(422));
        assert!(e.is_some_and(|e| e.to_string() == "Repository creation failed.: name already exists on this account"));
        assert!(check(resp(201, &[], "{}")).is_ok());
    }

    fn github(t: FakeTransport) -> GitHub {
        GitHub::new(Api::new(Arc::new(t), Some(Secret::new("tok"))), Git::default())
    }

    #[test]
    fn create_repo_explains_a_taken_name() {
        let g = github(FakeTransport::default().answer(
            "/user/repos",
            422,
            r#"{"message":"Repository creation failed.","errors":[{"field":"name","message":"name already exists on this account"}]}"#,
        ));
        let e = g
            .create_repo(&NewRepo {
                org: None,
                name: "proj".into(),
                description: String::new(),
                private: true,
            })
            .err();
        assert_eq!(
            e.map(|e| e.to_string()).as_deref(),
            Some("A repository named “proj” already exists in your account. Choose another name.")
        );
        let g = github(FakeTransport::default().answer(
            "/orgs/acme/repos",
            201,
            r#"{"full_name":"acme/proj","html_url":"https://github.com/acme/proj","clone_url":"https://github.com/acme/proj.git"}"#,
        ));
        let created = g.create_repo(&NewRepo {
            org: Some("acme".into()),
            name: "proj".into(),
            description: "d".into(),
            private: false,
        });
        assert_eq!(created.ok().map(|c| c.clone_url).as_deref(), Some("https://github.com/acme/proj.git"));
    }

    // JSON fixtures for every parsed type.

    #[test]
    fn fixtures_pr_list_and_detail() -> Result<()> {
        let list = r#"{"data":{"search":{"nodes":[
          {"number":12,"title":"Fix it","author":{"login":"a"},"headRefName":"fix","isDraft":true,
           "reviewDecision":"APPROVED","updatedAt":"2026-11-20T10:00:00Z",
           "commits":{"nodes":[{"commit":{"statusCheckRollup":{"contexts":{"nodes":[
             {"__typename":"CheckRun","name":"ci","status":"COMPLETED","conclusion":"SUCCESS"},
             {"__typename":"StatusContext","context":"x","state":"PENDING"}]}}}}]}},
          {"number":13,"title":"Other","commits":{"nodes":[{"commit":{"statusCheckRollup":null}}]}},
          {}]}}}"#;
        let g = github(FakeTransport::default().answer("/graphql", 200, list));
        let prs: Vec<PrSummary> = {
            let found: GqlSearch = g.api.graphql("q", json!({}))?;
            found.search.nodes.into_iter().filter(|p| p.number > 0).map(pr_summary).collect()
        };
        assert_eq!(prs.len(), 2);
        assert_eq!(prs[0].checks(), crate::gh::ChecksSummary::Pending);
        assert_eq!(prs[0].head_ref_name, "fix");
        assert_eq!(prs[1].checks(), crate::gh::ChecksSummary::None);

        let detail = r#"{"data":{"repository":{"pullRequest":{"number":5,"title":"T","body":"**hi**",
          "baseRefName":"main","headRefName":"f","url":"https://github.com/o/r/pull/5","state":"OPEN",
          "additions":3,"deletions":1,"changedFiles":2,
          "reviews":{"nodes":[{"author":{"login":"r"},"state":"APPROVED","body":""}]},
          "comments":{"nodes":[{"author":{"login":"c"},"body":"nice","createdAt":"2026-11-20T10:00:00Z"}]},
          "commits":{"nodes":[]}}}}}"#;
        let g = github(FakeTransport::default().answer("/graphql", 200, detail));
        let found: GqlRepoPr = g.api.graphql("q", json!({}))?;
        let d = found.repository.and_then(|r| r.pull_request).map(pr_detail);
        let d = d.ok_or_else(|| anyhow!("no detail"))?;
        assert_eq!((d.number, d.base_ref_name.as_str(), d.changed_files), (5, "main", 2));
        assert_eq!(d.reviews[0].state, "APPROVED");
        assert_eq!(d.comments[0].body, "nice");
        // GraphQL errors become an error.
        let g = github(FakeTransport::default().answer("/graphql", 200, r#"{"errors":[{"message":"Could not resolve to a Repository"}]}"#));
        let e: Result<Value> = g.api.graphql("q", json!({}));
        assert!(e.is_err_and(|e| e.to_string().contains("Could not resolve")));
        Ok(())
    }

    #[test]
    fn fixtures_checks_runs_jobs() -> Result<()> {
        let runs: Vec<RestCheckRun> = serde_json::from_value(serde_json::from_str::<Value>(
            r#"{"check_runs":[
              {"name":"build","status":"completed","conclusion":"failure","html_url":"https://github.com/o/r/actions/runs/123/job/456","app":{"name":"GitHub Actions"},"output":{"title":"2 errors"}},
              {"name":"lint","status":"in_progress","conclusion":null},
              {"name":"docs","status":"completed","conclusion":"skipped"}]}"#,
        )?["check_runs"].clone())?;
        let buckets: Vec<&str> = runs.iter().map(|c| check_run_bucket(&c.status, c.conclusion.as_deref())).collect();
        assert_eq!(buckets, ["fail", "pending", "skipping"]);
        let status: RestCombinedStatus = serde_json::from_str(r#"{"statuses":[{"context":"ci/x","state":"error","target_url":"https://x"}]}"#)?;
        assert_eq!(status_bucket(&status.statuses[0].state), "fail");
        let check = Check {
            link: runs[0].html_url.clone().unwrap_or_default(),
            ..Default::default()
        };
        assert_eq!(check.run_id(), Some(123));

        let run: RestRun = serde_json::from_str(
            r#"{"id":9,"display_title":"Fix","name":"CI","status":"completed","conclusion":"failure","event":"push","head_branch":"main","created_at":"2026-11-20T10:00:00Z","html_url":"https://github.com/o/r/actions/runs/9"}"#,
        )?;
        let run = run_from_rest(run);
        assert_eq!((run.database_id, run.workflow_name.as_str()), (9, "CI"));
        assert_eq!(run.state(), crate::gh::RunState::Failure);
        let queued: RestRun = serde_json::from_str(r#"{"id":10,"status":"queued","conclusion":null}"#)?;
        assert_eq!(run_from_rest(queued).state(), crate::gh::RunState::Pending);
        let job: RestJob = serde_json::from_str(r#"{"id":1,"name":"test","status":"completed","conclusion":"skipped","html_url":"https://x"}"#)?;
        assert_eq!(job.conclusion.as_deref(), Some("skipped"));

        let repo: RestRepo = serde_json::from_str(r#"{"full_name":"o/r","html_url":"https://github.com/o/r","default_branch":"main"}"#)?;
        assert_eq!(repo.default_branch.as_deref(), Some("main"));
        let pull: RestPull = serde_json::from_str(r#"{"number":5,"node_id":"PR_x","html_url":"https://github.com/o/r/pull/5","head":{"ref":"f","sha":"abc","repo":{"full_name":"fork/r"}}}"#)?;
        assert_eq!((pull.head.name.as_str(), pull.head.sha.as_str()), ("f", "abc"));
        let items: Vec<RepoItem> = serde_json::from_str(r#"[{"full_name":"o/r","name":"r","private":true,"clone_url":"https://github.com/o/r.git","html_url":"https://github.com/o/r","updated_at":"2026-11-20T10:00:00Z","description":null}]"#)?;
        assert!(items[0].private && items[0].description.is_none());
        Ok(())
    }

    #[test]
    fn run_list_uses_the_origin_remote() -> Result<()> {
        // A real repository whose origin points at GitHub.
        let tmp = tempfile::tempdir()?;
        let dir = tmp.path();
        crate::process::run(
            &crate::process::Cmd::new("sh")
                .args(["-c", "git init -q && git remote add origin git@github.com:acme/widgets.git"])
                .cwd(dir),
        )?
        .check()?;
        let g = github(FakeTransport::default().answer(
            "/repos/acme/widgets/actions/runs?branch=main",
            200,
            r#"{"total_count":1,"workflow_runs":[{"id":7,"display_title":"x","name":"CI","status":"completed","conclusion":"success","event":"push","head_branch":"main","html_url":"h"}]}"#,
        ));
        let runs = g.run_list(dir, "main")?;
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].state(), crate::gh::RunState::Success);
        Ok(())
    }
}
