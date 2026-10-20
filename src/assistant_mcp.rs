//! Assistant MCP client configuration and connection checks.
//! Servers are explicit user choices; Echo never shells through command text.
use rmcp::{
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, StreamableHttpClientTransport,
        TokioChildProcess,
    },
    ServiceExt,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServer {
    pub id: String,
    pub name: String,
    pub transport: McpTransport,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub discovered_tools: Vec<rmcp::model::Tool>,
    #[serde(default)]
    pub environment: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpTransport {
    Stdio { command: String, args: Vec<String> },
    Http { url: String },
}

impl McpServer {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Name the MCP server.".into());
        }
        match &self.transport {
            McpTransport::Stdio { command, .. } if command.trim().is_empty() => {
                Err("Enter an MCP server executable.".into())
            }
            McpTransport::Http { url } => {
                let parsed = url::Url::parse(url).map_err(|_| "Enter a valid MCP URL.")?;
                if !parsed.username().is_empty() || parsed.password().is_some() {
                    return Err("Do not put credentials in an MCP URL.".into());
                }
                let local = matches!(
                    parsed.host_str(),
                    Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
                );
                if parsed.scheme() != "https" && !(parsed.scheme() == "http" && local) {
                    return Err("Remote MCP servers must use HTTPS.".into());
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

pub struct Client {
    service: rmcp::service::RunningService<rmcp::service::RoleClient, ()>,
    #[cfg(windows)]
    _tree: Option<crate::agent_tools::ProcessTree>,
}
impl Client {
    pub fn peer(&self) -> &rmcp::service::Peer<rmcp::service::RoleClient> {
        self.service.peer()
    }
    pub async fn cancel(self) -> Result<rmcp::service::QuitReason, tokio::task::JoinError> {
        self.service.cancel().await
    }
}
pub async fn connect(server: &McpServer) -> Result<Client, String> {
    server.validate()?;
    let operation = async {
        match &server.transport {
            McpTransport::Stdio { command, args } => {
                let mut process = tokio::process::Command::new(command);
                process
                    .args(args)
                    .envs(&server.environment)
                    .kill_on_drop(true);
                if crate::components::server("browser").is_ok_and(|managed| {
                    managed.transport == server.transport
                        && managed.environment == server.environment
                        && server.id == managed.id
                }) {
                    let dir = crate::artifacts::root();
                    std::fs::create_dir_all(&dir)
                        .map_err(|_| "Could not create browser output directory.")?;
                    process.current_dir(dir);
                }
                #[cfg(windows)]
                process.creation_flags(0x08000000);
                let (transport, _) = TokioChildProcess::builder(process)
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .map_err(|_| "Could not launch MCP server.")?;
                #[cfg(windows)]
                let tree = crate::agent_tools::ProcessTree::attach_pid(
                    transport
                        .id()
                        .ok_or("MCP process ended before initialization.")?,
                )?;
                let service =
                    ().serve(transport)
                        .await
                        .map_err(|_| "Could not initialize MCP server.")?;
                Ok(Client {
                    service,
                    #[cfg(windows)]
                    _tree: Some(tree),
                })
            }
            McpTransport::Http { url } => {
                let mut config = StreamableHttpClientTransportConfig::with_uri(url.clone());
                for (name, value) in &server.headers {
                    config.custom_headers.insert(
                        name.parse().map_err(|_| "Invalid MCP header name.")?,
                        value.parse().map_err(|_| "Invalid MCP header value.")?,
                    );
                }
                let client = reqwest_rig::Client::builder()
                    .redirect(reqwest_rig::redirect::Policy::none())
                    .build()
                    .map_err(|_| "Could not initialize MCP HTTP client.")?;
                let transport = StreamableHttpClientTransport::with_client(client, config);
                let service =
                    ().serve(transport)
                        .await
                        .map_err(|_| "Could not initialize MCP endpoint.")?;
                Ok(Client {
                    service,
                    #[cfg(windows)]
                    _tree: None,
                })
            }
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(15), operation)
        .await
        .map_err(|_| "MCP connection timed out.")?
}

impl std::fmt::Debug for McpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServer")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("enabled", &self.enabled)
            .field("credentials", &"[REDACTED]")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_http_requires_https() {
        let server = McpServer {
            id: "x".into(),
            name: "server".into(),
            enabled: true,
            discovered_tools: vec![],
            environment: Default::default(),
            headers: Default::default(),
            transport: McpTransport::Http {
                url: "http://example.com/mcp".into(),
            },
        };
        assert!(server.validate().is_err());
        let local = McpServer {
            transport: McpTransport::Http {
                url: "http://localhost:8080/mcp".into(),
            },
            ..server
        };
        assert!(local.validate().is_ok());
    }
}
