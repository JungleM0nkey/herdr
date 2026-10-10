use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConditionalPluginTarget {
    pub server_incarnation: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub anchor_pane_id: String,
    pub anchor_terminal_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConditionalPluginStateTarget {
    pub server_incarnation: Option<String>,
    pub workspace_id: String,
    pub tab_id: String,
    pub anchor_pane_id: String,
    pub anchor_terminal_id: String,
}

impl ConditionalPluginStateTarget {
    pub(crate) fn bind(&self, incarnation: &str) -> ConditionalPluginTarget {
        ConditionalPluginTarget {
            server_incarnation: self
                .server_incarnation
                .clone()
                .unwrap_or_else(|| incarnation.to_owned()),
            workspace_id: self.workspace_id.clone(),
            tab_id: self.tab_id.clone(),
            anchor_pane_id: self.anchor_pane_id.clone(),
            anchor_terminal_id: self.anchor_terminal_id.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConditionalPluginStateParams {
    pub target: ConditionalPluginStateTarget,
    pub plugin_id: String,
    pub entrypoint: String,
    pub ownership_nonce: String,
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConditionalOwnedPane {
    pub pane_id: String,
    pub terminal_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConditionalPluginSetStateParams {
    pub operation_id: String,
    pub target: ConditionalPluginTarget,
    pub plugin_id: String,
    pub entrypoint: String,
    pub installation_fingerprint: String,
    pub ownership_nonce: String,
    pub open: bool,
    pub expected_owned_pane: Option<ConditionalOwnedPane>,
    pub not_after_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConditionalPluginOutcome {
    Opened,
    Closed,
    Unchanged,
    Uncertain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConditionalPluginReceipt {
    pub operation_id: String,
    pub request_digest: String,
    pub outcome: ConditionalPluginOutcome,
    pub owned_pane: Option<ConditionalOwnedPane>,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConditionalPluginState {
    pub api_version: u32,
    pub server_now_unix_ms: u64,
    pub target: ConditionalPluginTarget,
    pub installation_fingerprint: String,
    pub owned_pane: Option<ConditionalOwnedPane>,
    pub conflict: Option<String>,
    pub saved_operation: Option<ConditionalPluginReceipt>,
    pub live_state_unavailable: bool,
}
