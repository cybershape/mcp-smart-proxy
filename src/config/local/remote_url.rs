use std::error::Error;

const FIGMA_MCP_HOST: &str = "mcp.figma.com";

pub(crate) fn is_unsupported_remote_server_url(url: &str) -> bool {
    is_unsupported_figma_remote(url)
}

pub(crate) fn validate_supported_remote_server_url(
    url: &str,
    server_name: &str,
) -> Result<(), Box<dyn Error>> {
    if is_unsupported_remote_server_url(url) {
        return Err(format!(
            "server `{server_name}` uses unsupported remote MCP URL `{url}`; msp does not support Figma's hosted MCP endpoint"
        )
        .into());
    }

    Ok(())
}

fn is_unsupported_figma_remote(url: &str) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_owned))
        .is_some_and(|host| host.eq_ignore_ascii_case(FIGMA_MCP_HOST))
}

#[cfg(test)]
mod tests {
    use super::validate_supported_remote_server_url;

    #[test]
    fn rejects_figma_remote_url() {
        let error =
            validate_supported_remote_server_url("https://mcp.figma.com/mcp", "figma").unwrap_err();

        assert_eq!(
            error.to_string(),
            "server `figma` uses unsupported remote MCP URL `https://mcp.figma.com/mcp`; msp does not support Figma's hosted MCP endpoint"
        );
    }

    #[test]
    fn accepts_non_figma_remote_url() {
        validate_supported_remote_server_url("https://example.com/mcp", "demo").unwrap();
    }
}
