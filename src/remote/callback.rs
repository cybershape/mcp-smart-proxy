use std::borrow::Cow;
use std::time::Duration;

use reqwest::Error as ReqwestError;
use rmcp::transport::streamable_http_client::StreamableHttpError;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::timeout,
};

const CALLBACK_HOST: &str = "127.0.0.1";
const CALLBACK_PATH: &str = "/oauth/callback";
const AUTH_TIMEOUT: Duration = Duration::from_secs(300);

pub(crate) struct CallbackServer {
    listener: TcpListener,
    redirect_uri: String,
    callback_path: String,
}

impl CallbackServer {
    pub(crate) async fn bind() -> Result<Self, StreamableHttpError<ReqwestError>> {
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
            callback_path: CALLBACK_PATH.to_string(),
        })
    }

    pub(crate) async fn bind_to_redirect_uri(
        redirect_uri: &str,
    ) -> Result<Self, StreamableHttpError<ReqwestError>> {
        let binding = parse_loopback_redirect_uri(redirect_uri)?;
        let listener = TcpListener::bind((binding.host.as_str(), binding.port))
            .await
            .map_err(StreamableHttpError::Io)?;

        Ok(Self {
            listener,
            redirect_uri: redirect_uri.to_string(),
            callback_path: binding.path,
        })
    }

    pub(crate) fn redirect_uri(&self) -> String {
        self.redirect_uri.clone()
    }

    pub(crate) async fn wait_for_callback(
        self,
    ) -> Result<CallbackResult, StreamableHttpError<ReqwestError>> {
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
        let callback = parse_callback_request(&path, &self.callback_path)?;
        stream
            .write_all(success_http_response().as_bytes())
            .await
            .map_err(StreamableHttpError::Io)?;
        Ok(callback)
    }
}

pub(crate) struct CallbackResult {
    pub(crate) code: String,
    pub(crate) state: String,
}

fn parse_callback_request(
    path: &str,
    expected_path: &str,
) -> Result<CallbackResult, StreamableHttpError<ReqwestError>> {
    let parsed = reqwest::Url::parse(&format!("http://localhost{path}")).map_err(|error| {
        StreamableHttpError::UnexpectedServerResponse(Cow::Owned(error.to_string()))
    })?;
    if parsed.path() != expected_path {
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

#[derive(Debug)]
struct LoopbackRedirectBinding {
    host: String,
    port: u16,
    path: String,
}

fn parse_loopback_redirect_uri(
    redirect_uri: &str,
) -> Result<LoopbackRedirectBinding, StreamableHttpError<ReqwestError>> {
    let parsed = reqwest::Url::parse(redirect_uri).map_err(|error| {
        StreamableHttpError::UnexpectedServerResponse(Cow::Owned(format!(
            "invalid OAuth redirect URI `{redirect_uri}`: {error}"
        )))
    })?;

    if parsed.scheme() != "http" {
        return Err(StreamableHttpError::UnexpectedServerResponse(Cow::Owned(
            format!(
                "OAuth redirect URI must use http on the loopback interface, got `{redirect_uri}`"
            ),
        )));
    }

    let host = parsed.host_str().ok_or_else(|| {
        StreamableHttpError::UnexpectedServerResponse(Cow::Owned(format!(
            "OAuth redirect URI `{redirect_uri}` is missing a host"
        )))
    })?;
    if !matches!(host, "127.0.0.1" | "localhost") {
        return Err(StreamableHttpError::UnexpectedServerResponse(Cow::Owned(
            format!("OAuth redirect URI must use `127.0.0.1` or `localhost`, got `{redirect_uri}`"),
        )));
    }

    let port = parsed.port().ok_or_else(|| {
        StreamableHttpError::UnexpectedServerResponse(Cow::Owned(format!(
            "OAuth redirect URI `{redirect_uri}` must include an explicit port"
        )))
    })?;
    let path = parsed.path().to_string();
    if path.is_empty() || path == "/" {
        return Err(StreamableHttpError::UnexpectedServerResponse(Cow::Owned(
            format!("OAuth redirect URI `{redirect_uri}` must include a non-root callback path"),
        )));
    }

    Ok(LoopbackRedirectBinding {
        host: host.to_string(),
        port,
        path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_loopback_redirect_uri() {
        let binding = parse_loopback_redirect_uri("http://127.0.0.1:33333/oauth/callback").unwrap();

        assert_eq!(binding.host, "127.0.0.1");
        assert_eq!(binding.port, 33333);
        assert_eq!(binding.path, "/oauth/callback");
    }

    #[test]
    fn rejects_non_loopback_redirect_uri() {
        let error =
            parse_loopback_redirect_uri("http://example.com:33333/oauth/callback").unwrap_err();

        assert!(
            error
                .to_string()
                .contains("must use `127.0.0.1` or `localhost`")
        );
    }

    #[test]
    fn rejects_redirect_uri_without_port() {
        let error = parse_loopback_redirect_uri("http://127.0.0.1/oauth/callback").unwrap_err();

        assert!(error.to_string().contains("must include an explicit port"));
    }

    #[test]
    fn parses_callback_request_with_custom_path() {
        let callback =
            parse_callback_request("/custom/callback?code=a&state=b", "/custom/callback").unwrap();

        assert_eq!(callback.code, "a");
        assert_eq!(callback.state, "b");
    }
}
