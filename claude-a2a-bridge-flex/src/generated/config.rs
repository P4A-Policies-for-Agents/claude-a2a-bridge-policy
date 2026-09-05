use serde::Deserialize;
#[derive(Deserialize, Clone, Debug)]
pub struct CardDerivationConfig {
    #[serde(alias = "excludeMcpServers")]
    pub exclude_mcp_servers: Option<Vec<String>>,
}
#[derive(Deserialize, Clone, Debug)]
pub struct Rules0Config {
    #[serde(alias = "action")]
    pub action: String,
    #[serde(alias = "message")]
    pub message: Option<String>,
    #[serde(alias = "prompt")]
    pub prompt: Option<String>,
    #[serde(alias = "tool")]
    pub tool: String,
}
#[derive(Deserialize, Clone, Debug)]
pub struct ToolConfirmationConfig {
    #[serde(alias = "defaultAction")]
    pub default_action: Option<String>,
    #[serde(alias = "logDecisions")]
    pub log_decisions: Option<bool>,
    #[serde(alias = "rules")]
    pub rules: Option<Vec<Rules0Config>>,
}
#[derive(Deserialize, Clone, Debug)]
pub struct Config {
    #[serde(alias = "agentCard")]
    pub agent_card: Option<String>,
    #[serde(alias = "agentCardSource")]
    pub agent_card_source: Option<String>,
    #[serde(alias = "agentId")]
    pub agent_id: String,
    #[serde(alias = "anthropicVersion")]
    pub anthropic_version: Option<String>,
    #[serde(alias = "apiKey")]
    pub api_key: String,
    #[serde(alias = "betaHeader")]
    pub beta_header: Option<String>,
    #[serde(alias = "cardDerivation")]
    pub card_derivation: Option<CardDerivationConfig>,
    #[serde(alias = "environmentId")]
    pub environment_id: String,
    #[serde(alias = "maxPollAttempts")]
    pub max_poll_attempts: Option<i64>,
    #[serde(alias = "pollIntervalMs")]
    pub poll_interval_ms: Option<i64>,
    #[serde(alias = "timeout")]
    pub timeout: Option<i64>,
    #[serde(alias = "toolConfirmation")]
    pub tool_confirmation: Option<ToolConfirmationConfig>,
}
#[pdk::hl::entrypoint_flex]
fn init(abi: &dyn pdk::flex_abi::api::FlexAbi) -> Result<(), anyhow::Error> {
    abi.setup()?;
    Ok(())
}
