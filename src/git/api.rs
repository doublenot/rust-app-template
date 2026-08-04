//! The `/api/git` sub-router: DTOs in, `GitService` calls out, one error envelope back.
//!
//! No `git2` type crosses this file. Every route — GETs included — is authenticated by
//! the single `from_fn_with_state` layer at the bottom of `router`, so no handler can
//! forget the token.
use crate::git::error::{GitError, GitErrorCode};
use crate::git::jobs::{JobFilter, JobOp, JobRef, JobSlot, JobState};
use crate::git::ops::{BranchRequest, OpRequest, ResetRequest};
use crate::git::registry::{AuthorSpec, CredentialSpec, RepoDef, RepoView, Warning};
use crate::git::{DeleteOutcome, GitService, RepoWithStatus, StartOutcome};
use crate::internal_server::HostState;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::Json;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};
use std::sync::Arc;

/// Long enough for a UUID plus a prefix, short enough that a hostile child cannot make
/// the `(repo_id, request_id)` index grow without bound.
pub const MAX_REQUEST_ID_LEN: usize = 128;
pub const DEFAULT_JOB_LIST_LIMIT: usize = 50;
pub const MAX_JOB_LIST_LIMIT: usize = 200;

/// The §5.4 error envelope: a `GitError` plus the two things only the service knows.
///
/// `GitError` has its own `IntoResponse` for the answers produced where no service
/// exists — the `git_disabled` fallback. Everything a live service answers goes through
/// this type instead, so `host_instance` is present on every one of them.
pub struct ApiError {
    pub err: GitError,
    pub host_instance: String,
    /// Populated for `repo_busy` and nothing else: a client keys off its presence to
    /// decide whether it can poll instead of retry.
    pub job: Option<JobRef>,
}

impl ApiError {
    pub fn new(err: GitError, host_instance: &str) -> ApiError {
        ApiError {
            err,
            host_instance: host_instance.to_string(),
            job: None,
        }
    }

    pub fn busy(slot: &Arc<JobSlot>, host_instance: &str) -> ApiError {
        ApiError {
            err: GitError::repo_busy(&slot.repo_id, slot.op.as_str()).with_repo(&slot.repo_id),
            host_instance: host_instance.to_string(),
            job: Some(slot.as_ref_view()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // Hand-rolled so the field order is the envelope's order and the optional
        // fields vanish rather than serializing as null.
        struct Body<'a>(&'a ApiError);
        impl Serialize for Body<'_> {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                let e = &self.0.err;
                let len = 3
                    + usize::from(e.repo_id.is_some())
                    + usize::from(e.path.is_some())
                    + usize::from(e.field.is_some())
                    + usize::from(self.0.job.is_some());
                let mut st = s.serialize_struct("error", len)?;
                st.serialize_field("code", e.code.as_str())?;
                st.serialize_field("message", &e.message)?;
                if let Some(v) = &e.repo_id {
                    st.serialize_field("repo_id", v)?;
                }
                if let Some(v) = &e.path {
                    st.serialize_field("path", v)?;
                }
                if let Some(v) = &e.field {
                    st.serialize_field("field", v)?;
                }
                st.serialize_field("host_instance", &self.0.host_instance)?;
                if let Some(job) = &self.0.job {
                    st.serialize_field("job", job)?;
                }
                st.end()
            }
        }
        #[derive(Serialize)]
        struct Envelope<'a> {
            error: Body<'a>,
        }

        let status = self.err.http_status();
        let mut response = (status, Json(Envelope { error: Body(&self) })).into_response();
        if status == StatusCode::CONFLICT {
            // Every 409 is a "someone else holds this, come back" answer, and a client
            // without a backoff should still not hot-loop.
            response.headers_mut().insert(
                axum::http::header::RETRY_AFTER,
                HeaderValue::from_static("1"),
            );
        }
        response
    }
}

fn default_remote_name() -> String {
    "origin".to_string()
}

fn default_settings_path() -> String {
    "settings.json".to_string()
}

fn yes() -> bool {
    true
}

/// `Option<Option<T>>` where the outer layer is "was the key present at all".
///
/// `#[serde(default)]` gives `None` for an absent key; this function is reached only
/// when the key *is* present, so an explicit `null` becomes `Some(None)`.
fn de_double_option<'de, D>(d: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::<String>::deserialize(d)?))
}

/// Parse a request body into its DTO.
///
/// The handlers take the body as a `String` rather than as `Json<T>` so that a stray
/// key, malformed JSON, a missing content-type and a missing body all come back through
/// the same envelope. `Json<T>`'s own rejection is a bare 422 with a plain-text body,
/// and §5.4 promises one envelope on every non-2xx from `/api/git/*`.
fn body_to<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, GitError> {
    let trimmed = raw.trim();
    let text = if trimmed.is_empty() { "{}" } else { trimmed };
    serde_json::from_str(text).map_err(|e| GitError::invalid_request("body", e.to_string()))
}

fn check_request_id(request_id: Option<&str>) -> Result<(), GitError> {
    let Some(value) = request_id else {
        return Ok(());
    };
    // Printable ASCII only: the id is echoed into `git.log`, which is one record per
    // line, and into a `BTreeMap` key.
    let printable = value.bytes().all(|b| (0x20..0x7f).contains(&b));
    if value.is_empty() || value.len() > MAX_REQUEST_ID_LEN || !printable {
        return Err(GitError::invalid_request(
            "request_id",
            format!("request_id must be 1 to {MAX_REQUEST_ID_LEN} printable ASCII characters"),
        ));
    }
    Ok(())
}

fn job_filter(query: JobsQuery) -> Result<JobFilter, GitError> {
    if let Some(id) = &query.repo_id {
        crate::git::registry::validate_id(id)?;
    }
    let state = match query.state.as_deref() {
        None => None,
        Some(raw) => Some(JobState::parse(raw).ok_or_else(|| {
            GitError::invalid_request(
                "state",
                format!("state must be running, succeeded or failed (got {raw:?})"),
            )
        })?),
    };
    Ok(JobFilter {
        repo_id: query.repo_id,
        state,
        // `JobFilter::limit == 0` means unlimited in the store; the HTTP layer always
        // passes a real cap so no caller can ask the host to serialize every record.
        limit: query
            .limit
            .unwrap_or(DEFAULT_JOB_LIST_LIMIT)
            .clamp(1, MAX_JOB_LIST_LIMIT),
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoDefBody {
    #[serde(default)]
    pub remote: Option<String>,
    #[serde(default = "default_remote_name")]
    pub remote_name: String,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub credential: Option<CredentialSpec>,
    #[serde(default)]
    pub author: Option<AuthorSpec>,
    #[serde(default)]
    pub sync_settings: bool,
    #[serde(default = "default_settings_path")]
    pub settings_path: String,
    #[serde(default)]
    pub auto_sync_secs: Option<u64>,
    #[serde(default)]
    pub sync_on_start: bool,
    #[serde(default)]
    pub sync_on_quit: bool,
    #[serde(default)]
    pub restart_children_on_pull: bool,
}

impl RepoDefBody {
    pub fn into_def(self, id: String) -> RepoDef {
        RepoDef {
            id,
            remote: self.remote,
            remote_name: self.remote_name,
            // "" is the registry's sentinel for "use [git].default_branch", and
            // `Registry::put` is the single place that resolves it.
            branch: self.branch.unwrap_or_default(),
            credential: self.credential,
            author: self.author,
            sync_settings: self.sync_settings,
            settings_path: self.settings_path,
            auto_sync_secs: self.auto_sync_secs,
            sync_on_start: self.sync_on_start,
            sync_on_quit: self.sync_on_quit,
            restart_children_on_pull: self.restart_children_on_pull,
            // 0 means "stamp me": the API never invents timestamps, and a replace keeps
            // the original `created_at_ms` because `Registry::put` owns that decision.
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitBody {
    #[serde(default)]
    pub request_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloneBody {
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub credential: Option<CredentialSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PullBody {
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub credential: Option<CredentialSpec>,
    #[serde(default)]
    pub commit_local: bool,
    #[serde(default)]
    pub restart_children: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushBody {
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub credential: Option<CredentialSpec>,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncBody {
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub credential: Option<CredentialSpec>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub author: Option<AuthorSpec>,
    #[serde(default)]
    pub allow_empty: bool,
    #[serde(default = "yes")]
    pub push: bool,
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub restart_children: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitBody {
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub author: Option<AuthorSpec>,
    #[serde(default)]
    pub paths: Option<Vec<String>>,
    #[serde(default)]
    pub allow_empty: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BranchBody {
    #[serde(default)]
    pub request_id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub create: bool,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default = "yes")]
    pub checkout: bool,
    #[serde(default, deserialize_with = "de_double_option")]
    pub upstream: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetBody {
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub clean_untracked: bool,
    #[serde(default)]
    pub confirm: bool,
}

/// `JobsQuery` and `DeleteQuery` deliberately do **not** carry `deny_unknown_fields`: a
/// query string routinely picks up cache-busting parameters and rejecting them would
/// break callers for no safety gain. Every JSON body does carry it — a silently-ignored
/// `"messsage"` key is a data-loss bug.
#[derive(Debug, Deserialize)]
pub struct JobsQuery {
    #[serde(default)]
    pub repo_id: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct DeleteQuery {
    #[serde(default)]
    pub purge: bool,
}

#[derive(Serialize)]
pub struct ReposResponse {
    pub repos: Vec<RepoView>,
}

#[derive(Serialize)]
pub struct RepoResponse {
    pub repo: RepoView,
    pub warnings: Vec<Warning>,
}

#[derive(Serialize)]
pub struct JobResponse {
    pub job: crate::git::jobs::JobView,
}

#[derive(Serialize)]
pub struct JobsResponse {
    pub jobs: Vec<crate::git::jobs::JobView>,
}

/// `HostState.git` is `Some` for every request that reaches this router — the disabled
/// variant is nested instead. Answering `git_disabled` rather than panicking keeps that
/// guarantee honest if the wiring is ever changed.
fn service(state: &HostState) -> Result<Arc<GitService>, Response> {
    match &state.git {
        Some(git) => Ok(git.clone()),
        None => Err(git_disabled_response()),
    }
}

fn git_disabled_response() -> Response {
    // Deliberately a bare `GitError`: there is no service, so there is no
    // `host_instance` to name and inventing one would be a lie.
    GitError::new(
        GitErrorCode::GitDisabled,
        "the host was built without a [git] section in app.toml",
    )
    .into_response()
}

/// Every handler funnels its errors through here, so `host_instance` is on all of them.
fn fail(git: &GitService, err: GitError) -> Response {
    ApiError::new(err, git.host_instance()).into_response()
}

/// The router nested at `/api/git` when `[git]` is absent.
///
/// A fallback rather than a route table, so it answers for `/api/git` itself, for every
/// sub-path, and for every method — there is no shape of request that can get a bare 404
/// out of a host that simply has no git.
pub fn disabled_router() -> axum::Router<HostState> {
    axum::Router::new().fallback(git_disabled)
}

async fn git_disabled() -> Response {
    git_disabled_response()
}

pub fn router(state: HostState) -> axum::Router<HostState> {
    // Every `.route(...)` goes BEFORE `.route_layer(...)`. A route chained after the
    // layer is not covered by it, which is the exact failure mode the single-layer
    // design exists to prevent.
    axum::Router::new()
        .route("/", get(service_info))
        .route("/repos", get(list_repos))
        .route(
            "/repos/{id}",
            put(put_repo).get(get_repo).delete(delete_repo),
        )
        .route("/repos/{id}/status", get(get_repo))
        .route("/repos/{id}/branches", get(get_branches))
        .route("/repos/{id}/init", post(post_init))
        .route("/repos/{id}/clone", post(post_clone))
        .route("/repos/{id}/pull", post(post_pull))
        .route("/repos/{id}/push", post(post_push))
        .route("/repos/{id}/sync", post(post_sync))
        .route("/repos/{id}/commit", post(post_commit))
        .route("/repos/{id}/branch", post(post_branch))
        .route("/repos/{id}/reset", post(post_reset))
        .route("/jobs", get(list_jobs))
        .route("/jobs/{job_id}", get(get_job))
        .route_layer(axum::middleware::from_fn_with_state(state, require_token))
}

/// The single gate. Every route under `/api/git` passes through it — GETs included,
/// which is a deliberate divergence from the unauthenticated `GET /api/status`: a repo
/// listing names remotes and paths, and `GET /jobs` names branches and commit ids.
///
/// `route_layer` and not `layer`, so a request for a path that does not exist is a plain
/// 404 rather than a 401 that implies the path is real.
async fn require_token(
    State(state): State<HostState>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    if !crate::internal_server::authorized(&headers, &state) {
        let instance = state
            .git
            .as_ref()
            .map(|git| git.host_instance().to_string())
            .unwrap_or_default();
        return ApiError::new(
            GitError::new(
                GitErrorCode::Unauthorized,
                "missing or invalid x-host-token",
            ),
            &instance,
        )
        .into_response();
    }
    next.run(request).await
}

async fn service_info(State(state): State<HostState>) -> Response {
    match service(&state) {
        Ok(git) => Json(git.service_info()).into_response(),
        Err(response) => response,
    }
}

async fn list_repos(State(state): State<HostState>) -> Response {
    match service(&state) {
        Ok(git) => Json(ReposResponse {
            repos: git.list_views(),
        })
        .into_response(),
        Err(response) => response,
    }
}

async fn put_repo(State(state): State<HostState>, Path(id): Path<String>, raw: String) -> Response {
    let git = match service(&state) {
        Ok(git) => git,
        Err(response) => return response,
    };
    let body: RepoDefBody = match body_to(&raw) {
        Ok(body) => body,
        Err(e) => return fail(&git, e.with_repo(&id)),
    };
    let outcome = match git.put_repo(&id, body.into_def(id.clone())) {
        Ok(outcome) => outcome,
        Err(e) => return fail(&git, e),
    };
    // Re-read rather than converting the returned def: the response must show what the
    // registry actually stored, including its normalised branch and timestamps.
    let repo = match git.repo_view(&id) {
        Ok(repo) => repo,
        Err(e) => return fail(&git, e),
    };
    let status = if outcome.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    (
        status,
        Json(RepoResponse {
            repo,
            warnings: outcome.warnings,
        }),
    )
        .into_response()
}

/// Serves both `GET /repos/{id}` and `GET /repos/{id}/status`: §5.5's worked example
/// gives them the same `{repo, status}` body, and one handler cannot drift from itself.
async fn get_repo(State(state): State<HostState>, Path(id): Path<String>) -> Response {
    let git = match service(&state) {
        Ok(git) => git,
        Err(response) => return response,
    };
    match git.read_status(&id).await {
        Ok(both) => Json::<RepoWithStatus>(both).into_response(),
        Err(e) => fail(&git, e),
    }
}

async fn get_branches(State(state): State<HostState>, Path(id): Path<String>) -> Response {
    let git = match service(&state) {
        Ok(git) => git,
        Err(response) => return response,
    };
    match git.read_branches(&id).await {
        Ok(branches) => Json(branches).into_response(),
        Err(e) => fail(&git, e),
    }
}

async fn delete_repo(
    State(state): State<HostState>,
    Path(id): Path<String>,
    Query(query): Query<DeleteQuery>,
) -> Response {
    let git = match service(&state) {
        Ok(git) => git,
        Err(response) => return response,
    };
    match git.delete_repo(&id, query.purge) {
        Ok(outcome) => Json::<DeleteOutcome>(outcome).into_response(),
        Err(e) => fail(&git, e),
    }
}

/// Pull the service and the parsed body out in one place, so eight handlers do not
/// repeat the same two `match`es.
fn prepare<T: serde::de::DeserializeOwned>(
    state: &HostState,
    id: &str,
    raw: &str,
) -> Result<(Arc<GitService>, T), Response> {
    let git = service(state)?;
    match body_to::<T>(raw) {
        Ok(body) => Ok((git, body)),
        Err(e) => Err(fail(&git, e.with_repo(id))),
    }
}

fn start(git: &Arc<GitService>, id: &str, op: JobOp, req: OpRequest) -> Response {
    if let Err(e) = check_request_id(req.request_id.as_deref()) {
        return fail(git, e.with_repo(id));
    }
    match git.start_job(id, op, req) {
        Ok(outcome) => job_response(git, outcome),
        Err(e) => fail(git, e),
    }
}

fn job_response(git: &GitService, outcome: StartOutcome) -> Response {
    let (status, slot) = match &outcome {
        StartOutcome::Started(slot) => (StatusCode::ACCEPTED, slot),
        // Not 202: nothing new was started, and a caller that keys off the status code
        // must be able to tell "I created this" from "you already had it".
        StartOutcome::Replay(slot) => (StatusCode::OK, slot),
        StartOutcome::Busy(slot) => {
            return ApiError::busy(slot, git.host_instance()).into_response()
        }
    };
    let job = slot.view(git.host_instance());
    let mut response = (status, Json(JobResponse { job })).into_response();
    if status == StatusCode::ACCEPTED {
        // Built here rather than in the client, so the job id stays opaque.
        if let Ok(value) = HeaderValue::from_str(&format!("/api/git/jobs/{}", slot.id)) {
            response
                .headers_mut()
                .insert(axum::http::header::LOCATION, value);
        }
    }
    response
}

async fn post_init(
    State(state): State<HostState>,
    Path(id): Path<String>,
    raw: String,
) -> Response {
    let (git, body): (Arc<GitService>, InitBody) = match prepare(&state, &id, &raw) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let req = OpRequest {
        request_id: body.request_id,
        ..OpRequest::default()
    };
    start(&git, &id, JobOp::Init, req)
}

async fn post_clone(
    State(state): State<HostState>,
    Path(id): Path<String>,
    raw: String,
) -> Response {
    let (git, body): (Arc<GitService>, CloneBody) = match prepare(&state, &id, &raw) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let req = OpRequest {
        request_id: body.request_id,
        credential: body.credential,
        ..OpRequest::default()
    };
    start(&git, &id, JobOp::Clone, req)
}

async fn post_pull(
    State(state): State<HostState>,
    Path(id): Path<String>,
    raw: String,
) -> Response {
    let (git, body): (Arc<GitService>, PullBody) = match prepare(&state, &id, &raw) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let req = OpRequest {
        request_id: body.request_id,
        credential: body.credential,
        commit_local: body.commit_local,
        restart_children: body.restart_children,
        ..OpRequest::default()
    };
    start(&git, &id, JobOp::Pull, req)
}

async fn post_push(
    State(state): State<HostState>,
    Path(id): Path<String>,
    raw: String,
) -> Response {
    let (git, body): (Arc<GitService>, PushBody) = match prepare(&state, &id, &raw) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let req = OpRequest {
        request_id: body.request_id,
        credential: body.credential,
        force: body.force,
        ..OpRequest::default()
    };
    start(&git, &id, JobOp::Push, req)
}

async fn post_sync(
    State(state): State<HostState>,
    Path(id): Path<String>,
    raw: String,
) -> Response {
    let (git, body): (Arc<GitService>, SyncBody) = match prepare(&state, &id, &raw) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let req = OpRequest {
        request_id: body.request_id,
        credential: body.credential,
        message: body.message,
        author: body.author,
        allow_empty: body.allow_empty,
        push: body.push,
        force: body.force,
        restart_children: body.restart_children,
        ..OpRequest::default()
    };
    start(&git, &id, JobOp::Sync, req)
}

async fn post_commit(
    State(state): State<HostState>,
    Path(id): Path<String>,
    raw: String,
) -> Response {
    let (git, body): (Arc<GitService>, CommitBody) = match prepare(&state, &id, &raw) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let req = OpRequest {
        request_id: body.request_id,
        message: body.message,
        author: body.author,
        paths: body.paths,
        allow_empty: body.allow_empty,
        ..OpRequest::default()
    };
    start(&git, &id, JobOp::Commit, req)
}

async fn post_branch(
    State(state): State<HostState>,
    Path(id): Path<String>,
    raw: String,
) -> Response {
    let (git, body): (Arc<GitService>, BranchBody) = match prepare(&state, &id, &raw) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    if !crate::config::validate_branch_name(&body.name) {
        return fail(
            &git,
            GitError::invalid_request(
                "name",
                format!("{:?} is not a valid branch name", body.name),
            )
            .with_repo(&id),
        );
    }
    if !body.create && !body.checkout && body.upstream.is_none() {
        // Answering 202 here would hand back a job that provably does nothing, and the
        // caller would poll it to find out.
        return fail(
            &git,
            GitError::invalid_request("create", "nothing to do: set create, checkout or upstream")
                .with_repo(&id),
        );
    }
    let req = OpRequest {
        request_id: body.request_id,
        branch: Some(BranchRequest {
            name: body.name,
            create: body.create,
            from: body.from,
            checkout: body.checkout,
            upstream: body.upstream,
        }),
        ..OpRequest::default()
    };
    start(&git, &id, JobOp::Branch, req)
}

async fn post_reset(
    State(state): State<HostState>,
    Path(id): Path<String>,
    raw: String,
) -> Response {
    let (git, body): (Arc<GitService>, ResetBody) = match prepare(&state, &id, &raw) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let req = OpRequest {
        request_id: body.request_id,
        reset: Some(ResetRequest {
            to: body.to.unwrap_or_else(|| "head".to_string()),
            clean_untracked: body.clean_untracked,
            confirm: body.confirm,
        }),
        ..OpRequest::default()
    };
    start(&git, &id, JobOp::Reset, req)
}

async fn list_jobs(State(state): State<HostState>, Query(query): Query<JobsQuery>) -> Response {
    let git = match service(&state) {
        Ok(git) => git,
        Err(response) => return response,
    };
    let filter = match job_filter(query) {
        Ok(filter) => filter,
        Err(e) => return fail(&git, e),
    };
    let jobs = git
        .jobs()
        .list(&filter)
        .iter()
        .map(|slot| slot.view(git.host_instance()))
        .collect();
    Json(JobsResponse { jobs }).into_response()
}

async fn get_job(State(state): State<HostState>, Path(job_id): Path<String>) -> Response {
    let git = match service(&state) {
        Ok(git) => git,
        Err(response) => return response,
    };
    match git.jobs().lookup(&job_id) {
        // A failed job is still a 200: the failure lives in `job.error`, so a client
        // writes one `match` over `code` for both surfaces rather than two.
        Ok(slot) => Json(JobResponse {
            job: slot.view(git.host_instance()),
        })
        .into_response(),
        // `lookup` rejects a foreign instance prefix outright, so a surviving child can
        // never be answered about a different job that shares a sequence number.
        Err(e) => fail(&git, e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::jobs::{Admission, JobStore};

    fn parse<T: serde::de::DeserializeOwned>(json: &str) -> Result<T, GitError> {
        body_to::<T>(json)
    }

    async fn body_of(r: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(r.into_body(), 64 * 1024)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json body")
    }

    #[tokio::test]
    async fn the_envelope_always_names_the_host_instance() {
        let r = ApiError::new(
            GitError::invalid_request("auto_sync_secs", "must be a positive integer")
                .with_repo("notes"),
            "7c1e93aa",
        )
        .into_response();
        assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(r.headers().get("retry-after").is_none());
        let body = body_of(r).await;
        assert_eq!(body["error"]["code"], "invalid_request");
        assert_eq!(body["error"]["repo_id"], "notes");
        assert_eq!(body["error"]["field"], "auto_sync_secs");
        assert_eq!(body["error"]["host_instance"], "7c1e93aa");
        // A client keys off the presence of `job`, so it must not be there when there
        // is no running job to point at.
        assert!(body["error"].get("job").is_none());
    }

    #[tokio::test]
    async fn only_repo_busy_embeds_the_running_job_and_sets_retry_after() {
        let store = JobStore::new("7c1e93aa".to_string());
        let Admission::Started(slot, _lease) =
            store.admit("notes", JobOp::Sync, None).expect("admitted")
        else {
            panic!("a fresh store must admit");
        };
        let r = ApiError::busy(&slot, "7c1e93aa").into_response();
        assert_eq!(r.status(), StatusCode::CONFLICT);
        assert_eq!(
            r.headers().get("retry-after").and_then(|v| v.to_str().ok()),
            Some("1")
        );
        let body = body_of(r).await;
        assert_eq!(body["error"]["code"], "repo_busy");
        assert_eq!(body["error"]["job"]["id"], slot.id.to_string());
        assert_eq!(body["error"]["job"]["op"], "sync");
        assert_eq!(body["error"]["job"]["state"], "running");
    }

    #[test]
    fn a_repo_body_fills_in_the_registry_defaults_and_rejects_strays() {
        let body: RepoDefBody = parse("{}").expect("an empty body is a valid definition");
        assert_eq!(body.remote_name, "origin");
        assert_eq!(body.settings_path, "settings.json");
        assert!(body.branch.is_none());
        let def = body.into_def("notes".to_string());
        assert_eq!(def.id, "notes");
        // "" is the registry's sentinel for "use [git].default_branch", so the API
        // never has to know what the default is.
        assert!(def.branch.is_empty());
        assert_eq!(def.created_at_ms, 0);

        let err = parse::<RepoDefBody>(r#"{"bogus": 1}"#).expect_err("stray key");
        assert_eq!(err.code(), GitErrorCode::InvalidRequest);
        assert_eq!(err.field.as_deref(), Some("body"));
        assert!(err.message.contains("bogus"), "{}", err.message);
    }

    #[test]
    fn a_missing_body_is_an_empty_object_and_bad_json_is_invalid_request() {
        // `POST /init` from a shell has no body at all, and `{}` must mean the same.
        let init: InitBody = parse("").expect("no body");
        assert!(init.request_id.is_none());
        let err = parse::<InitBody>("{").expect_err("malformed");
        assert_eq!(err.code(), GitErrorCode::InvalidRequest);
    }

    #[test]
    fn sync_pushes_by_default_and_the_caller_can_turn_it_off() {
        let body: SyncBody = parse("{}").unwrap();
        assert!(body.push, "sync means sync, so push defaults to true");
        let body: SyncBody = parse(r#"{"push": false}"#).unwrap();
        assert!(!body.push);
        assert!(!body.force && !body.allow_empty);
        assert!(body.restart_children.is_none());
    }

    #[test]
    fn branch_upstream_distinguishes_absent_from_null_from_a_value() {
        // Three different intentions that JSON alone cannot tell apart without this:
        // leave it alone, unset it, set it.
        let body: BranchBody = parse(r#"{"name": "dev"}"#).unwrap();
        assert_eq!(body.upstream, None);
        assert!(body.checkout, "checkout defaults to true");
        let body: BranchBody = parse(r#"{"name": "dev", "upstream": null}"#).unwrap();
        assert_eq!(body.upstream, Some(None));
        let body: BranchBody = parse(r#"{"name": "dev", "upstream": "origin/dev"}"#).unwrap();
        assert_eq!(body.upstream, Some(Some("origin/dev".to_string())));
    }

    #[test]
    fn a_request_id_is_bounded_and_printable() {
        assert!(check_request_id(None).is_ok());
        assert!(check_request_id(Some("boot-1")).is_ok());
        let long = "x".repeat(MAX_REQUEST_ID_LEN + 1);
        let err = check_request_id(Some(&long)).expect_err("too long");
        assert_eq!(err.code(), GitErrorCode::InvalidRequest);
        assert_eq!(err.field.as_deref(), Some("request_id"));
        // A newline would break `git.log`'s one-record-per-line format.
        assert!(check_request_id(Some("boot\n1")).is_err());
        assert!(check_request_id(Some("")).is_err());
    }

    #[test]
    fn the_job_filter_is_always_capped() {
        let filter = job_filter(JobsQuery {
            repo_id: None,
            state: None,
            limit: None,
        })
        .unwrap();
        assert_eq!(filter.limit, DEFAULT_JOB_LIST_LIMIT);
        // `JobFilter::limit == 0` means unlimited in the store, so the HTTP layer must
        // never pass it through.
        let filter = job_filter(JobsQuery {
            repo_id: None,
            state: None,
            limit: Some(0),
        })
        .unwrap();
        assert_eq!(filter.limit, 1);
        let filter = job_filter(JobsQuery {
            repo_id: None,
            state: None,
            limit: Some(100_000),
        })
        .unwrap();
        assert_eq!(filter.limit, MAX_JOB_LIST_LIMIT);
        let err = job_filter(JobsQuery {
            repo_id: None,
            state: Some("sleeping".to_string()),
            limit: None,
        })
        .expect_err("unknown state");
        assert_eq!(err.field.as_deref(), Some("state"));
    }
}
