use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::{StreamExt, stream::BoxStream};
use reqwest::header::{
    ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderName, HeaderValue, WWW_AUTHENTICATE,
};
use rmcp::{
    RoleClient, ServiceExt,
    model::{ClientJsonRpcMessage, JsonRpcMessage, ServerJsonRpcMessage},
    service::RunningService,
    transport::{
        StreamableHttpClientTransport,
        auth::{
            AuthError, AuthorizationManager, AuthorizationSession, CredentialStore,
            StoredCredentials,
        },
        common::http_header::{
            EVENT_STREAM_MIME_TYPE, HEADER_LAST_EVENT_ID, HEADER_MCP_PROTOCOL_VERSION,
            HEADER_SESSION_ID, JSON_MIME_TYPE,
        },
        streamable_http_client::{
            StreamableHttpClient, StreamableHttpClientTransportConfig, StreamableHttpError,
            StreamableHttpPostResponse,
        },
    },
};
use sse_stream::{Sse, SseStream};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Mutex,
    time::timeout,
};

use crate::console::{message_error, print_app_event, print_app_warning};
use crate::fs_util::{acquire_sibling_lock, write_file_atomically};
use crate::paths::oauth_credentials_path;
use crate::types::{ConfiguredServer, ConfiguredTransport};

const CALLBACK_HOST: &str = "127.0.0.1";
const CALLBACK_PATH: &str = "/oauth/callback";
const AUTH_TIMEOUT: Duration = Duration::from_secs(300);

type RemoteTransport = StreamableHttpClientTransport<OAuthAwareHttpClient>;

pub async fn connect_remote_client(
    server_name: &str,
    server: &ConfiguredServer,
) -> Result<RunningService<RoleClient, ()>, Box<dyn Error>> {
    let remote = remote_target(server)?;
    let auth = RemoteAuth::new(server_name, &remote.url).await?;
    let client = OAuthAwareHttpClient::new(
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        auth,
    );
    let transport = RemoteTransport::with_client(
        client,
        StreamableHttpClientTransportConfig::with_uri(remote.url.clone())
            .custom_headers(remote.headers),
    );

    ().serve(transport).await.map_err(Into::into)
}

pub async fn login_remote_server(
    server_name: &str,
    server: &ConfiguredServer,
) -> Result<(), Box<dyn Error>> {
    let remote = remote_target(server)?;
    let auth = RemoteAuth::new(server_name, &remote.url).await?;
    auth.ensure_authorized(None).await.map_err(Into::into)
}

pub fn logout_remote_server(server_name: &str) -> Result<bool, Box<dyn Error>> {
    let path = oauth_credentials_path(server_name)?;
    let _guard = acquire_sibling_lock(&path).map_err(Box::<dyn Error>::from)?;
    if !path.exists() {
        return Ok(false);
    }

    fs::remove_file(path)?;
    Ok(true)
}

#[derive(Clone)]
struct RemoteTarget {
    url: String,
    headers: HashMap<HeaderName, HeaderValue>,
}

fn remote_target(server: &ConfiguredServer) -> Result<RemoteTarget, Box<dyn Error>> {
    let ConfiguredTransport::Remote { url, headers } = &server.transport else {
        return Err(message_error(
            "remote.resolve",
            "configured server is not a remote transport",
        ));
    };

    Ok(RemoteTarget {
        url: url.clone(),
        headers: resolve_remote_headers(headers, &server.resolved_env_map())?,
    })
}

fn resolve_remote_headers(
    headers: &BTreeMap<String, String>,
    env_values: &BTreeMap<String, std::ffi::OsString>,
) -> Result<HashMap<HeaderName, HeaderValue>, Box<dyn Error>> {
    let mut resolved = HashMap::new();

    for (name, value) in headers {
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|error| format!("invalid remote header name `{name}`: {error}"))?;
        if is_config_reserved_header(&header_name) {
            return Err(
                format!("remote header `{name}` is reserved and cannot be configured").into(),
            );
        }
        let header_value = resolve_remote_header_value(value, env_values)?;
        let header_value = HeaderValue::from_str(&header_value)
            .map_err(|error| format!("invalid remote header value for `{name}`: {error}"))?;
        resolved.insert(header_name, header_value);
    }

    Ok(resolved)
}

fn resolve_remote_header_value(
    value: &str,
    env_values: &BTreeMap<String, std::ffi::OsString>,
) -> Result<String, Box<dyn Error>> {
    let mut rendered = String::new();
    let bytes = value.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        if value[index..].starts_with("{env:") {
            let suffix = &value[index + 5..];
            let Some(end) = suffix.find('}') else {
                rendered.push_str(&value[index..]);
                break;
            };
            let name = &suffix[..end];
            let resolved = env_values.get(name).ok_or_else(|| {
                format!("missing environment variable `{name}` required by remote header")
            })?;
            rendered.push_str(&resolved.to_string_lossy());
            index += 6 + end;
            continue;
        }

        if value[index..].starts_with("${") {
            let suffix = &value[index + 2..];
            let Some(end) = suffix.find('}') else {
                rendered.push_str(&value[index..]);
                break;
            };
            let expression = &suffix[..end];
            let (name, fallback) = expression
                .split_once(":-")
                .map(|(name, fallback)| (name, Some(fallback)))
                .unwrap_or((expression, None));
            if let Some(resolved) = env_values.get(name) {
                rendered.push_str(&resolved.to_string_lossy());
            } else if let Some(fallback) = fallback {
                rendered.push_str(fallback);
            } else {
                return Err(format!(
                    "missing environment variable `{name}` required by remote header"
                )
                .into());
            }
            index += 3 + end;
            continue;
        }

        rendered.push(bytes[index] as char);
        index += 1;
    }

    Ok(rendered)
}

#[derive(Clone)]
struct OAuthAwareHttpClient {
    inner: reqwest::Client,
    auth: RemoteAuth,
}

impl OAuthAwareHttpClient {
    fn new(inner: reqwest::Client, auth: RemoteAuth) -> Self {
        Self { inner, auth }
    }

    async fn current_access_token(
        &self,
    ) -> Result<Option<String>, StreamableHttpError<reqwest::Error>> {
        let manager = self.auth.manager.lock().await;
        match manager.get_access_token().await {
            Ok(token) => Ok(Some(token)),
            Err(AuthError::AuthorizationRequired) => Ok(None),
            Err(error) => Err(StreamableHttpError::Auth(error)),
        }
    }

    async fn maybe_retry_authorization(
        &self,
        error: &StreamableHttpError<reqwest::Error>,
    ) -> Result<bool, StreamableHttpError<reqwest::Error>> {
        match error {
            StreamableHttpError::Auth(AuthError::AuthorizationRequired) => {
                self.auth.ensure_authorized(None).await?;
                Ok(true)
            }
            StreamableHttpError::Auth(AuthError::InsufficientScope { required_scope, .. }) => {
                let scope = (!required_scope.is_empty()).then_some(required_scope.as_str());
                self.auth.ensure_authorized(scope).await?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn attach_auth_header(
        mut request: reqwest::RequestBuilder,
        auth_token: Option<String>,
        custom_headers: &HashMap<HeaderName, HeaderValue>,
    ) -> reqwest::RequestBuilder {
        let has_authorization = custom_headers
            .keys()
            .any(|name| name.as_str().eq_ignore_ascii_case(AUTHORIZATION.as_str()));
        if !has_authorization && let Some(auth_header) = auth_token {
            request = request.bearer_auth(auth_header);
        }
        request
    }

    fn apply_custom_headers(
        mut request: reqwest::RequestBuilder,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<reqwest::RequestBuilder, StreamableHttpError<reqwest::Error>> {
        for (name, value) in custom_headers {
            if is_reserved_header(&name) {
                return Err(StreamableHttpError::ReservedHeaderConflict(
                    name.to_string(),
                ));
            }
            request = request.header(name, value);
        }
        Ok(request)
    }

    async fn get_stream_once(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        last_event_id: Option<String>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<
        BoxStream<'static, Result<Sse, sse_stream::Error>>,
        StreamableHttpError<reqwest::Error>,
    > {
        let mut request = self
            .inner
            .get(uri.as_ref())
            .header(ACCEPT, [EVENT_STREAM_MIME_TYPE, JSON_MIME_TYPE].join(", "))
            .header(HEADER_SESSION_ID, session_id.as_ref());
        if let Some(last_event_id) = last_event_id {
            request = request.header(HEADER_LAST_EVENT_ID, last_event_id);
        }
        request = Self::attach_auth_header(request, auth_token, &custom_headers);
        request = Self::apply_custom_headers(request, custom_headers)?;
        let response = request.send().await?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return unauthorized_response(response).await;
        }
        if response.status() == reqwest::StatusCode::FORBIDDEN {
            return forbidden_response(response).await;
        }
        if response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED {
            return Err(StreamableHttpError::ServerDoesNotSupportSse);
        }
        let response = response.error_for_status()?;
        match response.headers().get(CONTENT_TYPE) {
            Some(ct)
                if ct.as_bytes().starts_with(EVENT_STREAM_MIME_TYPE.as_bytes())
                    || ct.as_bytes().starts_with(JSON_MIME_TYPE.as_bytes()) => {}
            Some(ct) => {
                return Err(StreamableHttpError::UnexpectedContentType(Some(
                    String::from_utf8_lossy(ct.as_bytes()).to_string(),
                )));
            }
            None => return Err(StreamableHttpError::UnexpectedContentType(None)),
        }
        Ok(SseStream::from_byte_stream(response.bytes_stream()).boxed())
    }

    async fn delete_session_once(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<reqwest::Error>> {
        let mut request = self
            .inner
            .delete(uri.as_ref())
            .header(HEADER_SESSION_ID, session_id.as_ref());
        request = Self::attach_auth_header(request, auth_token, &custom_headers);
        request = Self::apply_custom_headers(request, custom_headers)?;
        let response = request.send().await?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return unauthorized_response(response).await;
        }
        if response.status() == reqwest::StatusCode::FORBIDDEN {
            return forbidden_response(response).await;
        }
        if response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED {
            return Ok(());
        }
        response.error_for_status()?;
        Ok(())
    }

    async fn post_message_once(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<reqwest::Error>> {
        let mut request = self
            .inner
            .post(uri.as_ref())
            .header(ACCEPT, [EVENT_STREAM_MIME_TYPE, JSON_MIME_TYPE].join(", "));
        request = Self::attach_auth_header(request, auth_token, &custom_headers);
        request = Self::apply_custom_headers(request, custom_headers)?;
        let session_was_attached = session_id.is_some();
        if let Some(session_id) = session_id {
            request = request.header(HEADER_SESSION_ID, session_id.as_ref());
        }
        let response = request.json(&message).send().await?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return unauthorized_response(response).await;
        }
        if response.status() == reqwest::StatusCode::FORBIDDEN {
            return forbidden_response(response).await;
        }
        let status = response.status();
        if matches!(
            status,
            reqwest::StatusCode::ACCEPTED | reqwest::StatusCode::NO_CONTENT
        ) {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        if status == reqwest::StatusCode::NOT_FOUND && session_was_attached {
            return Err(StreamableHttpError::SessionExpired);
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .map(|ct| String::from_utf8_lossy(ct.as_bytes()).to_string());
        let session_id = response
            .headers()
            .get(HEADER_SESSION_ID)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_string());
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<failed to read response body>".to_string());
            if content_type
                .as_deref()
                .is_some_and(|ct| ct.as_bytes().starts_with(JSON_MIME_TYPE.as_bytes()))
                && let Some(message) = parse_json_rpc_error(&body)
            {
                return Ok(StreamableHttpPostResponse::Json(message, session_id));
            }
            return Err(StreamableHttpError::UnexpectedServerResponse(Cow::Owned(
                format!("HTTP {status}: {body}"),
            )));
        }
        match content_type.as_deref() {
            Some(ct) if ct.as_bytes().starts_with(EVENT_STREAM_MIME_TYPE.as_bytes()) => {
                Ok(StreamableHttpPostResponse::Sse(
                    SseStream::from_byte_stream(response.bytes_stream()).boxed(),
                    session_id,
                ))
            }
            Some(ct) if ct.as_bytes().starts_with(JSON_MIME_TYPE.as_bytes()) => {
                match response.json::<ServerJsonRpcMessage>().await {
                    Ok(message) => Ok(StreamableHttpPostResponse::Json(message, session_id)),
                    Err(_) => Ok(StreamableHttpPostResponse::Accepted),
                }
            }
            _ => Err(StreamableHttpError::UnexpectedContentType(content_type)),
        }
    }
}

impl StreamableHttpClient for OAuthAwareHttpClient {
    type Error = reqwest::Error;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        _auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        let auth_token = self.current_access_token().await?;
        let retry_headers = custom_headers.clone();
        let retry_message = message.clone();
        let retry_session = session_id.clone();

        match self
            .post_message_once(uri.clone(), message, session_id, auth_token, custom_headers)
            .await
        {
            Ok(response) => Ok(response),
            Err(error) if self.maybe_retry_authorization(&error).await? => {
                let auth_token = self.current_access_token().await?;
                self.post_message_once(uri, retry_message, retry_session, auth_token, retry_headers)
                    .await
            }
            Err(error) => Err(error),
        }
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        _auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        let auth_token = self.current_access_token().await?;
        let retry_headers = custom_headers.clone();
        let retry_session = session_id.clone();

        match self
            .delete_session_once(uri.clone(), session_id, auth_token, custom_headers)
            .await
        {
            Ok(()) => Ok(()),
            Err(error) if self.maybe_retry_authorization(&error).await? => {
                let auth_token = self.current_access_token().await?;
                self.delete_session_once(uri, retry_session, auth_token, retry_headers)
                    .await
            }
            Err(error) => Err(error),
        }
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        last_event_id: Option<String>,
        _auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxStream<'static, Result<Sse, sse_stream::Error>>, StreamableHttpError<Self::Error>>
    {
        let auth_token = self.current_access_token().await?;
        let retry_headers = custom_headers.clone();
        let retry_session = session_id.clone();
        let retry_last_event_id = last_event_id.clone();

        match self
            .get_stream_once(
                uri.clone(),
                session_id,
                last_event_id,
                auth_token,
                custom_headers,
            )
            .await
        {
            Ok(stream) => Ok(stream),
            Err(error) if self.maybe_retry_authorization(&error).await? => {
                let auth_token = self.current_access_token().await?;
                self.get_stream_once(
                    uri,
                    retry_session,
                    retry_last_event_id,
                    auth_token,
                    retry_headers,
                )
                .await
            }
            Err(error) => Err(error),
        }
    }
}

#[derive(Clone)]
struct RemoteAuth {
    server_name: String,
    base_url: String,
    manager: Arc<Mutex<AuthorizationManager>>,
    flow_lock: Arc<Mutex<()>>,
}

impl RemoteAuth {
    async fn new(server_name: &str, base_url: &str) -> Result<Self, Box<dyn Error>> {
        let credentials_path = oauth_credentials_path(server_name)?;
        let mut manager = AuthorizationManager::new(base_url).await?;
        manager.set_credential_store(FileCredentialStore::new(credentials_path));
        manager.initialize_from_store().await?;

        Ok(Self {
            server_name: server_name.to_string(),
            base_url: base_url.to_string(),
            manager: Arc::new(Mutex::new(manager)),
            flow_lock: Arc::new(Mutex::new(())),
        })
    }

    async fn ensure_authorized(
        &self,
        required_scope: Option<&str>,
    ) -> Result<(), StreamableHttpError<reqwest::Error>> {
        let _flow_guard = self.flow_lock.lock().await;
        if required_scope.is_none() {
            let manager = self.manager.lock().await;
            match manager.get_access_token().await {
                Ok(_) => return Ok(()),
                Err(AuthError::AuthorizationRequired) => {}
                Err(error) => return Err(StreamableHttpError::Auth(error)),
            }
        }

        let callback = CallbackServer::bind().await?;
        let redirect_uri = callback.redirect_uri();

        let mut manager_guard = self.manager.lock().await;
        let placeholder = AuthorizationManager::new(self.base_url.as_str())
            .await
            .map_err(StreamableHttpError::Auth)?;
        let manager = std::mem::replace(&mut *manager_guard, placeholder);
        drop(manager_guard);

        let session = build_authorization_session(manager, &redirect_uri, required_scope).await?;
        let auth_url = session.get_authorization_url().to_string();
        print_app_event(
            "remote.oauth",
            format!(
                "Opening browser for OAuth login for remote MCP server `{}`",
                self.server_name
            ),
        );
        print_app_event("remote.oauth", format!("OAuth URL: {auth_url}"));
        if let Err(error) = webbrowser::open(&auth_url) {
            print_app_warning(
                "remote.oauth",
                format!("failed to open a browser automatically: {error}"),
            );
        }

        let callback_result = callback.wait_for_callback().await?;
        session
            .handle_callback(&callback_result.code, &callback_result.state)
            .await?;
        let mut manager_guard = self.manager.lock().await;
        *manager_guard = session.auth_manager;
        drop(manager_guard);

        print_app_event(
            "remote.oauth",
            format!(
                "Completed OAuth login for remote MCP server `{}`",
                self.server_name
            ),
        );
        Ok(())
    }
}

async fn build_authorization_session(
    mut manager: AuthorizationManager,
    redirect_uri: &str,
    required_scope: Option<&str>,
) -> Result<AuthorizationSession, StreamableHttpError<reqwest::Error>> {
    let metadata = manager
        .discover_metadata()
        .await
        .map_err(StreamableHttpError::Auth)?;
    manager.set_metadata(metadata);

    if let Some(required_scope) = required_scope {
        match manager.request_scope_upgrade(required_scope).await {
            Ok(auth_url) => {
                return Ok(AuthorizationSession::for_scope_upgrade(
                    manager,
                    auth_url,
                    redirect_uri,
                ));
            }
            Err(AuthError::AuthorizationRequired) | Err(AuthError::InternalError(_)) => {}
            Err(error) => return Err(StreamableHttpError::Auth(error)),
        }
    }

    let scopes = if let Some(required_scope) = required_scope {
        vec![required_scope.to_string()]
    } else {
        manager.select_scopes(None, &[])
    };
    let scope_refs = scopes.iter().map(String::as_str).collect::<Vec<_>>();
    AuthorizationSession::new(manager, &scope_refs, redirect_uri, Some("msp"), None)
        .await
        .map_err(StreamableHttpError::Auth)
}

struct CallbackServer {
    listener: TcpListener,
    redirect_uri: String,
}

impl CallbackServer {
    async fn bind() -> Result<Self, StreamableHttpError<reqwest::Error>> {
        let listener = TcpListener::bind((CALLBACK_HOST, 0))
            .await
            .map_err(StreamableHttpError::Io)?;
        let port = listener
            .local_addr()
            .map_err(StreamableHttpError::Io)?
            .port();
        Ok(Self {
            listener,
            redirect_uri: format!("http://{CALLBACK_HOST}:{port}{CALLBACK_PATH}"),
        })
    }

    fn redirect_uri(&self) -> String {
        self.redirect_uri.clone()
    }

    async fn wait_for_callback(
        self,
    ) -> Result<CallbackResult, StreamableHttpError<reqwest::Error>> {
        let accepted = timeout(AUTH_TIMEOUT, self.listener.accept())
            .await
            .map_err(|_| {
                StreamableHttpError::UnexpectedServerResponse(Cow::from(
                    "timed out waiting for OAuth callback",
                ))
            })?
            .map_err(StreamableHttpError::Io)?;
        let (mut stream, _) = accepted;
        let mut buffer = vec![0_u8; 8192];
        let read = stream
            .read(&mut buffer)
            .await
            .map_err(StreamableHttpError::Io)?;
        let request = String::from_utf8_lossy(&buffer[..read]).to_string();
        let first_line = request.lines().next().unwrap_or_default();
        let path = first_line
            .split_whitespace()
            .nth(1)
            .ok_or_else(|| {
                StreamableHttpError::UnexpectedServerResponse(Cow::from(
                    "received an invalid OAuth callback request",
                ))
            })?
            .to_string();
        let callback = parse_callback_request(&path)?;
        stream
            .write_all(success_http_response().as_bytes())
            .await
            .map_err(StreamableHttpError::Io)?;
        Ok(callback)
    }
}

struct CallbackResult {
    code: String,
    state: String,
}

fn parse_callback_request(
    path: &str,
) -> Result<CallbackResult, StreamableHttpError<reqwest::Error>> {
    let parsed = reqwest::Url::parse(&format!("http://localhost{path}")).map_err(|error| {
        StreamableHttpError::UnexpectedServerResponse(Cow::Owned(error.to_string()))
    })?;
    if parsed.path() != CALLBACK_PATH {
        return Err(StreamableHttpError::UnexpectedServerResponse(Cow::Owned(
            format!("unexpected OAuth callback path `{}`", parsed.path()),
        )));
    }

    let mut code = None;
    let mut state = None;
    for (key, value) in parsed.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.to_string()),
            "state" => state = Some(value.to_string()),
            _ => {}
        }
    }

    Ok(CallbackResult {
        code: code.ok_or_else(|| {
            StreamableHttpError::UnexpectedServerResponse(Cow::from(
                "OAuth callback did not include `code`",
            ))
        })?,
        state: state.ok_or_else(|| {
            StreamableHttpError::UnexpectedServerResponse(Cow::from(
                "OAuth callback did not include `state`",
            ))
        })?,
    })
}

fn success_http_response() -> &'static str {
    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n<html><body><h1>OAuth login complete</h1><p>You can close this window.</p></body></html>"
}

fn parse_json_rpc_error(body: &str) -> Option<ServerJsonRpcMessage> {
    match serde_json::from_str::<ServerJsonRpcMessage>(body) {
        Ok(message @ JsonRpcMessage::Error(_)) => Some(message),
        _ => None,
    }
}

async fn unauthorized_response<T>(
    response: reqwest::Response,
) -> Result<T, StreamableHttpError<reqwest::Error>> {
    if let Some(header) = response.headers().get(WWW_AUTHENTICATE) {
        header.to_str().map_err(|_| {
            StreamableHttpError::UnexpectedServerResponse(Cow::from(
                "invalid www-authenticate header value",
            ))
        })?;
        return Err(StreamableHttpError::Auth(AuthError::AuthorizationRequired));
    }
    Err(StreamableHttpError::UnexpectedServerResponse(Cow::from(
        "remote server returned 401 without a www-authenticate header",
    )))
}

async fn forbidden_response<T>(
    response: reqwest::Response,
) -> Result<T, StreamableHttpError<reqwest::Error>> {
    if let Some(header) = response.headers().get(WWW_AUTHENTICATE) {
        let header = header
            .to_str()
            .map_err(|_| {
                StreamableHttpError::UnexpectedServerResponse(Cow::from(
                    "invalid www-authenticate header value",
                ))
            })?
            .to_string();
        return Err(StreamableHttpError::Auth(AuthError::InsufficientScope {
            required_scope: extract_scope_from_header(&header).unwrap_or_default(),
            upgrade_url: None,
        }));
    }
    Err(StreamableHttpError::UnexpectedServerResponse(Cow::from(
        "remote server returned 403 without a www-authenticate header",
    )))
}

fn extract_scope_from_header(header: &str) -> Option<String> {
    let header_lowercase = header.to_ascii_lowercase();
    let scope_key = "scope=";
    let position = header_lowercase.find(scope_key)?;
    let start = position + scope_key.len();
    let value = &header[start..];
    if let Some(quoted) = value.strip_prefix('"') {
        let end = quoted.find('"')?;
        return Some(quoted[..end].to_string());
    }
    let end = value
        .find(|character: char| character == ',' || character == ';' || character.is_whitespace())
        .unwrap_or(value.len());
    (end > 0).then(|| value[..end].to_string())
}

fn is_reserved_header(name: &HeaderName) -> bool {
    let value = name.as_str();
    value.eq_ignore_ascii_case(ACCEPT.as_str())
        || value.eq_ignore_ascii_case(HEADER_SESSION_ID)
        || value.eq_ignore_ascii_case(HEADER_LAST_EVENT_ID)
}

fn is_config_reserved_header(name: &HeaderName) -> bool {
    is_reserved_header(name)
        || name
            .as_str()
            .eq_ignore_ascii_case(HEADER_MCP_PROTOCOL_VERSION)
}

#[derive(Clone)]
struct FileCredentialStore {
    path: PathBuf,
}

impl FileCredentialStore {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

#[async_trait]
impl CredentialStore for FileCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let _guard = acquire_sibling_lock(&self.path)
            .map_err(|error| AuthError::InternalError(error.to_string()))?;
        if !self.path.exists() {
            return Ok(None);
        }
        let contents = fs::read_to_string(&self.path)
            .map_err(|error| AuthError::InternalError(error.to_string()))?;
        serde_json::from_str(&contents)
            .map(Some)
            .map_err(|error| AuthError::InternalError(error.to_string()))
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        let _guard = acquire_sibling_lock(&self.path)
            .map_err(|error| AuthError::InternalError(error.to_string()))?;
        let contents = serde_json::to_string_pretty(&credentials)
            .map_err(|error| AuthError::InternalError(error.to_string()))?;
        write_file_atomically(&self.path, contents.as_bytes())
            .map_err(|error| AuthError::InternalError(error.to_string()))
    }

    async fn clear(&self) -> Result<(), AuthError> {
        let _guard = acquire_sibling_lock(&self.path)
            .map_err(|error| AuthError::InternalError(error.to_string()))?;
        if self.path.exists() {
            fs::remove_file(&self.path)
                .map_err(|error| AuthError::InternalError(error.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_headers_allow_negotiated_protocol_version() {
        let client = reqwest::Client::new();
        let request = client.get("http://example.com");
        let headers = HashMap::from([(
            HeaderName::from_static("mcp-protocol-version"),
            HeaderValue::from_static("2025-06-18"),
        )]);

        let request = OAuthAwareHttpClient::apply_custom_headers(request, headers)
            .expect("negotiated protocol header should be allowed")
            .build()
            .expect("request should build");

        assert_eq!(
            request.headers().get(HEADER_MCP_PROTOCOL_VERSION),
            Some(&HeaderValue::from_static("2025-06-18"))
        );
    }

    #[test]
    fn configured_headers_reject_protocol_version() {
        let headers =
            BTreeMap::from([("mcp-protocol-version".to_string(), "2025-06-18".to_string())]);

        let error = resolve_remote_headers(&headers, &BTreeMap::new())
            .expect_err("configured protocol header should be rejected");

        assert_eq!(
            error.to_string(),
            "remote header `mcp-protocol-version` is reserved and cannot be configured"
        );
    }
}
