use std::collections::BTreeMap;
use std::ffi::OsString;

const FIGMA_MCP_HOST: &str = "mcp.figma.com";
const FIGMA_OAUTH_CLIENT_ID_ENV: &str = "MSP_FIGMA_OAUTH_CLIENT_ID";
const FIGMA_OAUTH_CLIENT_SECRET_ENV: &str = "MSP_FIGMA_OAUTH_CLIENT_SECRET";
const FIGMA_OAUTH_REDIRECT_URI_ENV: &str = "MSP_FIGMA_OAUTH_REDIRECT_URI";
const DEFAULT_FIGMA_OAUTH_REDIRECT_URI: &str = "http://127.0.0.1:33333/oauth/callback";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StaticOAuthClientConfig {
    pub(crate) client_id: String,
    pub(crate) client_secret: String,
    pub(crate) redirect_uri: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FigmaOAuthMode {
    NotFigma,
    StaticClient(StaticOAuthClientConfig),
    MissingClientConfig,
}

pub(crate) fn detect_figma_oauth_mode(
    base_url: &str,
    env_values: &BTreeMap<String, OsString>,
) -> FigmaOAuthMode {
    if !is_figma_remote(base_url) {
        return FigmaOAuthMode::NotFigma;
    }

    let client_id = resolved_env_string(env_values, FIGMA_OAUTH_CLIENT_ID_ENV);
    let client_secret = resolved_env_string(env_values, FIGMA_OAUTH_CLIENT_SECRET_ENV);

    match (client_id, client_secret) {
        (Some(client_id), Some(client_secret)) => {
            let redirect_uri = resolved_env_string(env_values, FIGMA_OAUTH_REDIRECT_URI_ENV)
                .unwrap_or_else(|| DEFAULT_FIGMA_OAUTH_REDIRECT_URI.to_string());
            FigmaOAuthMode::StaticClient(StaticOAuthClientConfig {
                client_id,
                client_secret,
                redirect_uri,
            })
        }
        _ => FigmaOAuthMode::MissingClientConfig,
    }
}

pub(crate) fn figma_missing_oauth_config_message(server_name: &str) -> String {
    format!(
        "remote MCP server `{server_name}` uses Figma's hosted endpoint, but Figma rejected generic dynamic client registration. Configure a Figma OAuth app and expose `{FIGMA_OAUTH_CLIENT_ID_ENV}` plus `{FIGMA_OAUTH_CLIENT_SECRET_ENV}` through this server's `env` or `env_vars`. Register redirect URI `{DEFAULT_FIGMA_OAUTH_REDIRECT_URI}`, or override it with `{FIGMA_OAUTH_REDIRECT_URI_ENV}`."
    )
}

fn is_figma_remote(base_url: &str) -> bool {
    reqwest::Url::parse(base_url)
        .ok()
        .and_then(|url| {
            url.host_str()
                .map(|host| host.eq_ignore_ascii_case(FIGMA_MCP_HOST))
        })
        .unwrap_or(false)
}

fn resolved_env_string(env_values: &BTreeMap<String, OsString>, name: &str) -> Option<String> {
    env_values
        .get(name)
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_non_figma_hosts() {
        let mode = detect_figma_oauth_mode("https://example.com/mcp", &BTreeMap::new());

        assert_eq!(mode, FigmaOAuthMode::NotFigma);
    }

    #[test]
    fn uses_default_redirect_uri_for_figma() {
        let mode = detect_figma_oauth_mode(
            "https://mcp.figma.com/mcp",
            &BTreeMap::from([
                (
                    FIGMA_OAUTH_CLIENT_ID_ENV.to_string(),
                    OsString::from("client-id"),
                ),
                (
                    FIGMA_OAUTH_CLIENT_SECRET_ENV.to_string(),
                    OsString::from("client-secret"),
                ),
            ]),
        );

        assert_eq!(
            mode,
            FigmaOAuthMode::StaticClient(StaticOAuthClientConfig {
                client_id: "client-id".to_string(),
                client_secret: "client-secret".to_string(),
                redirect_uri: DEFAULT_FIGMA_OAUTH_REDIRECT_URI.to_string(),
            })
        );
    }

    #[test]
    fn respects_custom_redirect_uri_for_figma() {
        let mode = detect_figma_oauth_mode(
            "https://mcp.figma.com/mcp",
            &BTreeMap::from([
                (
                    FIGMA_OAUTH_CLIENT_ID_ENV.to_string(),
                    OsString::from("client-id"),
                ),
                (
                    FIGMA_OAUTH_CLIENT_SECRET_ENV.to_string(),
                    OsString::from("client-secret"),
                ),
                (
                    FIGMA_OAUTH_REDIRECT_URI_ENV.to_string(),
                    OsString::from("http://127.0.0.1:45454/oauth/callback"),
                ),
            ]),
        );

        assert_eq!(
            mode,
            FigmaOAuthMode::StaticClient(StaticOAuthClientConfig {
                client_id: "client-id".to_string(),
                client_secret: "client-secret".to_string(),
                redirect_uri: "http://127.0.0.1:45454/oauth/callback".to_string(),
            })
        );
    }

    #[test]
    fn requires_client_id_and_secret_for_figma() {
        let mode = detect_figma_oauth_mode(
            "https://mcp.figma.com/mcp",
            &BTreeMap::from([(
                FIGMA_OAUTH_CLIENT_ID_ENV.to_string(),
                OsString::from("client-id"),
            )]),
        );

        assert_eq!(mode, FigmaOAuthMode::MissingClientConfig);
    }
}
