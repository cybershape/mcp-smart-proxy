use std::error::Error;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use reqwest::Error as ReqwestError;
use rmcp::transport::{
    auth::{AuthError, AuthorizationManager, AuthorizationSession, OAuthClientConfig},
    streamable_http_client::StreamableHttpError,
};
use tokio::sync::Mutex;

use crate::console::{print_app_event, print_app_warning};
use crate::paths::oauth_credentials_path;

use super::callback::CallbackServer;
use super::figma::{
    FigmaOAuthMode, StaticOAuthClientConfig, detect_figma_oauth_mode,
    figma_missing_oauth_config_message,
};
use super::store::FileCredentialStore;

#[derive(Clone)]
pub(crate) struct RemoteAuth {
    server_name: String,
    base_url: String,
    credentials_path: PathBuf,
    bootstrap: OAuthBootstrap,
    pub(crate) manager: Arc<Mutex<AuthorizationManager>>,
    flow_lock: Arc<Mutex<()>>,
}

#[derive(Clone)]
enum OAuthBootstrap {
    DynamicRegistration,
    StaticClient(StaticOAuthClientConfig),
    FigmaManualClientRequired,
}

enum PendingAuthorizationSession {
    Rmcp(AuthorizationSession),
    StaticClient {
        auth_manager: AuthorizationManager,
        auth_url: String,
    },
}

impl PendingAuthorizationSession {
    fn authorization_url(&self) -> &str {
        match self {
            Self::Rmcp(session) => session.get_authorization_url(),
            Self::StaticClient { auth_url, .. } => auth_url,
        }
    }

    async fn handle_callback(
        &self,
        code: &str,
        state: &str,
    ) -> Result<(), StreamableHttpError<ReqwestError>> {
        match self {
            Self::Rmcp(session) => session
                .handle_callback(code, state)
                .await
                .map(|_| ())
                .map_err(StreamableHttpError::Auth),
            Self::StaticClient { auth_manager, .. } => auth_manager
                .exchange_code_for_token(code, state)
                .await
                .map(|_| ())
                .map_err(StreamableHttpError::Auth),
        }
    }

    fn into_manager(self) -> AuthorizationManager {
        match self {
            Self::Rmcp(session) => session.auth_manager,
            Self::StaticClient { auth_manager, .. } => auth_manager,
        }
    }
}

impl RemoteAuth {
    pub(crate) async fn new(
        server_name: &str,
        base_url: &str,
        env_values: &std::collections::BTreeMap<String, OsString>,
    ) -> Result<Self, Box<dyn Error>> {
        let credentials_path = oauth_credentials_path(server_name)?;
        let mut manager = AuthorizationManager::new(base_url).await?;
        manager.set_credential_store(FileCredentialStore::new(credentials_path.clone()));
        manager.initialize_from_store().await?;
        let bootstrap = match detect_figma_oauth_mode(base_url, env_values) {
            FigmaOAuthMode::NotFigma => OAuthBootstrap::DynamicRegistration,
            FigmaOAuthMode::StaticClient(config) => OAuthBootstrap::StaticClient(config),
            FigmaOAuthMode::MissingClientConfig => OAuthBootstrap::FigmaManualClientRequired,
        };

        Ok(Self {
            server_name: server_name.to_string(),
            base_url: base_url.to_string(),
            credentials_path,
            bootstrap,
            manager: Arc::new(Mutex::new(manager)),
            flow_lock: Arc::new(Mutex::new(())),
        })
    }

    pub(crate) async fn ensure_authorized(
        &self,
        required_scope: Option<&str>,
    ) -> Result<(), StreamableHttpError<ReqwestError>> {
        let _flow_guard = self.flow_lock.lock().await;
        if required_scope.is_none() {
            let manager = self.manager.lock().await;
            match manager.get_access_token().await {
                Ok(_) => return Ok(()),
                Err(AuthError::AuthorizationRequired) => {}
                Err(error) => return Err(StreamableHttpError::Auth(error)),
            }
        }

        let callback = match &self.bootstrap {
            OAuthBootstrap::StaticClient(config) => {
                CallbackServer::bind_to_redirect_uri(&config.redirect_uri).await?
            }
            _ => CallbackServer::bind().await?,
        };
        let redirect_uri = callback.redirect_uri();

        let placeholder = self.initialized_manager().await?;
        let mut manager_guard = self.manager.lock().await;
        let manager = std::mem::replace(&mut *manager_guard, placeholder);
        drop(manager_guard);

        let session = match build_authorization_session(
            &self.server_name,
            manager,
            &redirect_uri,
            required_scope,
            &self.bootstrap,
        )
        .await
        {
            Ok(session) => session,
            Err(error) => return Err(error),
        };
        let auth_url = session.authorization_url().to_string();
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

        let callback_result = match callback.wait_for_callback().await {
            Ok(callback_result) => callback_result,
            Err(error) => {
                self.replace_manager(session.into_manager()).await;
                return Err(error);
            }
        };
        if let Err(error) = session
            .handle_callback(&callback_result.code, &callback_result.state)
            .await
        {
            self.replace_manager(session.into_manager()).await;
            return Err(error);
        }
        self.replace_manager(session.into_manager()).await;

        print_app_event(
            "remote.oauth",
            format!(
                "Completed OAuth login for remote MCP server `{}`",
                self.server_name
            ),
        );
        Ok(())
    }

    async fn initialized_manager(
        &self,
    ) -> Result<AuthorizationManager, StreamableHttpError<ReqwestError>> {
        let mut manager = AuthorizationManager::new(self.base_url.as_str())
            .await
            .map_err(StreamableHttpError::Auth)?;
        manager.set_credential_store(FileCredentialStore::new(self.credentials_path.clone()));
        manager
            .initialize_from_store()
            .await
            .map_err(StreamableHttpError::Auth)?;
        Ok(manager)
    }

    async fn replace_manager(&self, manager: AuthorizationManager) {
        let mut manager_guard = self.manager.lock().await;
        *manager_guard = manager;
    }
}

async fn build_authorization_session(
    server_name: &str,
    mut manager: AuthorizationManager,
    redirect_uri: &str,
    required_scope: Option<&str>,
    bootstrap: &OAuthBootstrap,
) -> Result<PendingAuthorizationSession, StreamableHttpError<ReqwestError>> {
    let metadata = manager
        .discover_metadata()
        .await
        .map_err(StreamableHttpError::Auth)?;
    manager.set_metadata(metadata);

    if let Some(required_scope) = required_scope {
        match manager.request_scope_upgrade(required_scope).await {
            Ok(auth_url) => {
                return Ok(PendingAuthorizationSession::Rmcp(
                    AuthorizationSession::for_scope_upgrade(manager, auth_url, redirect_uri),
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

    match bootstrap {
        OAuthBootstrap::DynamicRegistration => {
            AuthorizationSession::new(manager, &scope_refs, redirect_uri, Some("msp"), None)
                .await
                .map(PendingAuthorizationSession::Rmcp)
                .map_err(StreamableHttpError::Auth)
        }
        OAuthBootstrap::StaticClient(config) => {
            let client_config = OAuthClientConfig::new(&config.client_id, &config.redirect_uri)
                .with_client_secret(&config.client_secret)
                .with_scopes(scopes.clone());
            manager
                .configure_client(client_config)
                .map_err(StreamableHttpError::Auth)?;
            let auth_url = manager
                .get_authorization_url(&scope_refs)
                .await
                .map_err(StreamableHttpError::Auth)?;
            Ok(PendingAuthorizationSession::StaticClient {
                auth_manager: manager,
                auth_url,
            })
        }
        OAuthBootstrap::FigmaManualClientRequired => {
            Err(StreamableHttpError::UnexpectedServerResponse(
                figma_missing_oauth_config_message(server_name).into(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn figma_missing_config_error_is_actionable() {
        let error = figma_missing_oauth_config_message("figma");

        assert!(error.contains("MSP_FIGMA_OAUTH_CLIENT_ID"));
        assert!(error.contains("MSP_FIGMA_OAUTH_CLIENT_SECRET"));
        assert!(error.contains("http://127.0.0.1:33333/oauth/callback"));
    }
}
