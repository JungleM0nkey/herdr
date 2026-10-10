use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use super::super::responses::{encode_error, encode_success};
use super::manifest::{effective_platforms, ensure_platform_supported, normalize_action_id};
use crate::api::schema::*;
use crate::app::App;

const RECEIPT_CAPACITY: usize = 4096;
const MAX_DEADLINE_MS: u64 = 60 * 60 * 1000;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

pub(crate) struct ConditionalPluginOperations {
    pub(crate) server_incarnation: String,
    server_now_unix_ms: u64,
    receipts: HashMap<String, SavedOperation>,
}

#[cfg(test)]
#[path = "conditional_tests.rs"]
mod tests;

#[derive(Clone)]
struct SavedOperation {
    params: ConditionalPluginSetStateParams,
    digest: String,
    result: ConditionalPluginState,
}

impl Default for ConditionalPluginOperations {
    fn default() -> Self {
        let mut bytes = [0u8; 16];
        let server_incarnation = match getrandom::fill(&mut bytes) {
            Ok(()) => format!("server_{:032x}", u128::from_be_bytes(bytes)),
            Err(err) => {
                tracing::error!(%err, "conditional plugin API disabled: OS randomness unavailable");
                String::new()
            }
        };
        Self {
            server_incarnation,
            server_now_unix_ms: 0,
            receipts: HashMap::new(),
        }
    }
}

impl ConditionalPluginOperations {
    fn observe_clock(&mut self) -> Result<u64, &'static str> {
        self.server_now_unix_ms = self.server_now_unix_ms.max(current_unix_ms()?);
        Ok(self.server_now_unix_ms)
    }

    fn check_deadline(&mut self, deadline: u64) -> Result<(), &'static str> {
        check_deadline(deadline, self.observe_clock()?)
    }
}

fn digest(value: &impl serde::Serialize) -> Result<String, &'static str> {
    let bytes = serde_json::to_vec(value).map_err(|_| "invalid_params")?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn current_unix_ms() -> Result<u64, &'static str> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "clock_unavailable")?
        .as_millis();
    u64::try_from(millis).map_err(|_| "clock_unavailable")
}

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn check_deadline(deadline: u64, now: u64) -> Result<(), &'static str> {
    if deadline > MAX_SAFE_INTEGER || deadline > now.saturating_add(MAX_DEADLINE_MS) {
        Err("invalid_deadline")
    } else if deadline <= now {
        Err("expired")
    } else {
        Ok(())
    }
}

fn installation_fingerprint(
    plugin: &InstalledPluginInfo,
    pane: &PluginManifestPane,
) -> Result<String, &'static str> {
    if plugin.source.kind == PluginSourceKind::Github
        && !plugin.source.resolved_commit.as_deref().is_some_and(|sha| {
            matches!(sha.len(), 40 | 64) && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    {
        return Err("plugin_commit_unavailable");
    }
    let root =
        std::fs::canonicalize(&plugin.plugin_root).map_err(|_| "plugin_manifest_unavailable")?;
    let root = root.to_str().ok_or("plugin_manifest_unavailable")?;
    let manifest =
        std::fs::read(&plugin.manifest_path).map_err(|_| "plugin_manifest_unavailable")?;
    // Installed timestamps are bookkeeping, not installation identity.
    digest(&serde_json::json!({
        "version": 1,
        "plugin_id": plugin.plugin_id,
        "source": {
            "kind": plugin.source.kind,
            "owner": plugin.source.owner,
            "repo": plugin.source.repo,
            "subdir": plugin.source.subdir,
            "requested_ref": plugin.source.requested_ref,
            "resolved_commit": plugin.source.resolved_commit,
        },
        "root": root,
        "manifest_sha256": format!("{:x}", Sha256::digest(manifest)),
        "platform": std::env::consts::OS,
        "platforms": plugin.platforms,
        "enabled": plugin.enabled,
        "entrypoint": pane,
    }))
}

fn state_response(id: String, state: ConditionalPluginState) -> String {
    encode_success(id, ResponseResult::ConditionalPluginState { state })
}

impl App {
    fn conditional_target_indices(
        &self,
        target: &ConditionalPluginTarget,
    ) -> Result<(usize, usize), &'static str> {
        if self
            .conditional_plugin_operations
            .server_incarnation
            .is_empty()
        {
            return Err("server_incarnation_unavailable");
        }
        if target.server_incarnation != self.conditional_plugin_operations.server_incarnation {
            return Err("stale_server_incarnation");
        }
        let (ws_idx, pane_id) = self
            .parse_current_public_pane_id(&target.anchor_pane_id)
            .ok_or("stale_target")?;
        if self.public_workspace_id(ws_idx) != target.workspace_id {
            return Err("stale_target");
        }
        let ws = &self.state.workspaces[ws_idx];
        let tab_idx = ws.find_tab_index_for_pane(pane_id).ok_or("stale_target")?;
        if self.public_tab_id(ws_idx, tab_idx).as_deref() != Some(target.tab_id.as_str())
            || ws.terminal_id(pane_id).map(|id| id.as_str())
                != Some(target.anchor_terminal_id.as_str())
        {
            return Err("stale_target");
        }
        if ws.tabs[tab_idx]
            .panes
            .values()
            .any(|pane| pane.plugin_provenance_unknown)
        {
            return Err("plugin_provenance_unknown");
        }
        Ok((ws_idx, tab_idx))
    }

    fn conditional_plugin_state(
        &mut self,
        target: ConditionalPluginTarget,
        plugin_id: &str,
        entrypoint: &str,
        nonce: &str,
    ) -> Result<
        (
            ConditionalPluginState,
            InstalledPluginInfo,
            PluginManifestPane,
        ),
        &'static str,
    > {
        if super::normalize_plugin_id(plugin_id).as_deref() != Some(plugin_id)
            || normalize_action_id(entrypoint).as_deref() != Some(entrypoint)
            || !valid_token(nonce)
        {
            return Err("invalid_params");
        }
        let (ws_idx, tab_idx) = self.conditional_target_indices(&target)?;
        self.refresh_installed_plugins()
            .map_err(|_| "plugin_registry_load_failed")?;
        let plugin = self
            .state
            .installed_plugins
            .get(plugin_id)
            .cloned()
            .ok_or("plugin_not_found")?;
        if !super::plugin_manifest_available(&plugin) {
            return Err("plugin_manifest_unavailable");
        }
        let pane = plugin
            .panes
            .iter()
            .find(|pane| pane.id == entrypoint)
            .cloned()
            .ok_or("plugin_pane_not_found")?;
        let fingerprint = installation_fingerprint(&plugin, &pane)?;
        let tab = &self.state.workspaces[ws_idx].tabs[tab_idx];
        let mut owned_pane = None;
        let mut conflict = None;
        for (pane_id, state) in &tab.panes {
            let Some(record) = self.state.plugin_panes.get(pane_id) else {
                continue;
            };
            if record.plugin_id != plugin_id || record.entrypoint != entrypoint {
                continue;
            }
            let Some(ownership) = record.ownership.as_ref().filter(|ownership| {
                ownership.ownership_nonce == nonce
                    && ownership.installation_fingerprint == fingerprint
                    && ownership.terminal_id == state.attached_terminal_id
            }) else {
                conflict = Some("foreign_plugin_pane".to_owned());
                continue;
            };
            if owned_pane.is_some() {
                conflict = Some("multiple_plugin_panes".to_owned());
            }
            owned_pane = Some(ConditionalOwnedPane {
                pane_id: self
                    .public_pane_id(ws_idx, *pane_id)
                    .ok_or("stale_target")?,
                terminal_id: ownership.terminal_id.to_string(),
            });
        }
        if self
            .conditional_plugin_operations
            .receipts
            .values()
            .any(|saved| {
                saved.params.target.server_incarnation == target.server_incarnation
                    && saved.params.target.workspace_id == target.workspace_id
                    && saved.params.target.tab_id == target.tab_id
                    && saved.params.plugin_id == plugin_id
                    && saved.params.entrypoint == entrypoint
                    && saved
                        .result
                        .saved_operation
                        .as_ref()
                        .is_some_and(|receipt| {
                            receipt.outcome == ConditionalPluginOutcome::Uncertain
                        })
            })
        {
            conflict = Some("uncertain_operation".to_owned());
        }
        if !plugin.enabled {
            conflict = Some("plugin_disabled".to_owned());
        } else if ensure_platform_supported(
            effective_platforms(&pane.platforms, &plugin.platforms),
            "plugin pane",
        )
        .is_err()
        {
            conflict = Some("unsupported_platform".to_owned());
        }
        Ok((
            ConditionalPluginState {
                api_version: 1,
                server_now_unix_ms: self.conditional_plugin_operations.observe_clock()?,
                target,
                installation_fingerprint: fingerprint,
                owned_pane,
                conflict,
                saved_operation: None,
                live_state_unavailable: false,
            },
            plugin,
            pane,
        ))
    }

    pub(in crate::app::api) fn handle_conditional_plugin_state(
        &mut self,
        id: String,
        params: ConditionalPluginStateParams,
    ) -> String {
        if !valid_token(&params.ownership_nonce)
            || params
                .operation_id
                .as_ref()
                .is_some_and(|id| !valid_token(id))
        {
            return encode_error(
                id,
                "invalid_params",
                "invalid operation id or ownership nonce",
            );
        }
        let saved = params
            .operation_id
            .as_ref()
            .and_then(|operation_id| {
                self.conditional_plugin_operations
                    .receipts
                    .get(operation_id)
            })
            .cloned();
        if saved.as_ref().is_some_and(|saved| {
            saved.params.plugin_id != params.plugin_id
                || saved.params.entrypoint != params.entrypoint
                || saved.params.ownership_nonce != params.ownership_nonce
        }) {
            return encode_error(id, "operation_scope_mismatch", "operation scope differs");
        }
        let target = params
            .target
            .bind(&self.conditional_plugin_operations.server_incarnation);
        let live = (|| {
            let _guard =
                crate::persist::plugin_registry::installation_guard(&params.plugin_id, false)
                    .map_err(|_| "plugin_installation_busy")?;
            self.conditional_plugin_state(
                target,
                &params.plugin_id,
                &params.entrypoint,
                &params.ownership_nonce,
            )
        })();
        let mut state = match live {
            Ok((mut state, _, _)) => {
                state.saved_operation = saved.and_then(|saved| saved.result.saved_operation);
                state
            }
            Err(code) => {
                if let Some(saved) = saved {
                    let mut state = saved.result;
                    state.owned_pane = None;
                    state.conflict = Some(code.to_owned());
                    state.live_state_unavailable = true;
                    state
                } else {
                    return encode_error(id, code, "conditional plugin state unavailable");
                }
            }
        };
        match self.conditional_plugin_operations.observe_clock() {
            Ok(now) => state.server_now_unix_ms = now,
            Err(code) => {
                state.server_now_unix_ms = self.conditional_plugin_operations.server_now_unix_ms;
                state.live_state_unavailable = true;
                state.conflict = Some(code.to_owned());
            }
        }
        state_response(id, state)
    }

    pub(in crate::app::api) fn handle_conditional_plugin_set_state(
        &mut self,
        id: String,
        params: ConditionalPluginSetStateParams,
    ) -> String {
        let digest = match digest(&params) {
            Ok(digest) => digest,
            Err(code) => return encode_error(id, code, "cannot canonicalize operation"),
        };
        // Historical lookup must precede deadline, installation and target checks.
        if let Some(saved) = self
            .conditional_plugin_operations
            .receipts
            .get(&params.operation_id)
        {
            return if saved.digest == digest {
                state_response(id, saved.result.clone())
            } else {
                encode_error(
                    id,
                    "operation_id_reused",
                    "operation id has different content",
                )
            };
        }
        if !valid_token(&params.operation_id) || !valid_token(&params.ownership_nonce) {
            return encode_error(
                id,
                "invalid_params",
                "invalid operation id or ownership nonce",
            );
        }
        if let Err(code) = self
            .conditional_plugin_operations
            .check_deadline(params.not_after_unix_ms)
        {
            return encode_error(id, code, "operation deadline rejected");
        }
        let _guard =
            match crate::persist::plugin_registry::installation_guard(&params.plugin_id, false) {
                Ok(guard) => guard,
                Err(_) => {
                    return encode_error(id, "plugin_installation_busy", "plugin installation busy")
                }
            };
        let (mut state, plugin, pane) = match self.conditional_plugin_state(
            params.target.clone(),
            &params.plugin_id,
            &params.entrypoint,
            &params.ownership_nonce,
        ) {
            Ok(state) => state,
            Err(code) => return encode_error(id, code, "conditional plugin target rejected"),
        };
        if let Some(code) = state.conflict.as_deref() {
            return encode_error(id, code, "conditional plugin state conflicts");
        }
        if state.installation_fingerprint != params.installation_fingerprint {
            return encode_error(id, "installation_changed", "plugin installation changed");
        }
        if state.owned_pane != params.expected_owned_pane {
            return encode_error(
                id,
                "owned_pane_changed",
                "owned pane differs from precondition",
            );
        }
        if self.conditional_plugin_operations.receipts.len() >= RECEIPT_CAPACITY {
            return encode_error(
                id,
                "receipt_capacity_exhausted",
                "receipt capacity exhausted",
            );
        }
        if let Err(code) = self
            .conditional_plugin_operations
            .check_deadline(params.not_after_unix_ms)
        {
            return encode_error(id, code, "operation expired before mutation");
        }
        let unchanged = params.open == state.owned_pane.is_some();
        state.saved_operation = Some(ConditionalPluginReceipt {
            operation_id: params.operation_id.clone(),
            request_digest: digest.clone(),
            outcome: if unchanged {
                ConditionalPluginOutcome::Unchanged
            } else {
                ConditionalPluginOutcome::Uncertain
            },
            owned_pane: state.owned_pane.clone(),
            error_code: (!unchanged).then(|| "execution_intent".to_owned()),
        });
        self.conditional_plugin_operations.receipts.insert(
            params.operation_id.clone(),
            SavedOperation {
                params: params.clone(),
                digest,
                result: state.clone(),
            },
        );
        if !unchanged {
            let effect = if params.open {
                match self.open_conditional_plugin_pane(&id, &params, &plugin, pane) {
                    Ok(owned_pane) => {
                        state.owned_pane = Some(owned_pane);
                        Ok(ConditionalPluginOutcome::Opened)
                    }
                    Err(code) => Err(code),
                }
            } else if let Some(owned) = state.owned_pane.as_ref() {
                let result = self
                    .conditional_plugin_operations
                    .check_deadline(params.not_after_unix_ms)
                    .and_then(|()| {
                        self.close_pane(
                            id.clone(),
                            &PaneTarget {
                                pane_id: owned.pane_id.clone(),
                            },
                        )
                        .map_err(|_| "plugin_pane_close_uncertain")
                    });
                match result {
                    Ok(()) => {
                        state.owned_pane = None;
                        Ok(ConditionalPluginOutcome::Closed)
                    }
                    Err(code) => Err(code),
                }
            } else {
                Err("owned_pane_missing")
            };
            if let Some(receipt) = &mut state.saved_operation {
                receipt.owned_pane = state.owned_pane.clone();
                match effect {
                    Ok(outcome) => {
                        receipt.outcome = outcome;
                        receipt.error_code = None;
                    }
                    Err(code) => {
                        receipt.error_code = Some(code.to_owned());
                        state.conflict = Some("uncertain_operation".to_owned());
                    }
                }
            }
            if let Some(saved) = self
                .conditional_plugin_operations
                .receipts
                .get_mut(&params.operation_id)
            {
                saved.result = state.clone();
            }
        }
        state_response(id, state)
    }

    fn open_conditional_plugin_pane(
        &mut self,
        id: &str,
        params: &ConditionalPluginSetStateParams,
        plugin: &InstalledPluginInfo,
        mut pane: PluginManifestPane,
    ) -> Result<ConditionalOwnedPane, &'static str> {
        let (ws_idx, anchor) = self
            .parse_current_public_pane_id(&params.target.anchor_pane_id)
            .ok_or("stale_target")?;
        let context = self.plugin_context_for_pane(ws_idx, anchor, "plugin-pane");
        let cwd = std::path::PathBuf::from(&plugin.plugin_root);
        let env = self
            .plugin_pane_launch_env(plugin, &pane.id, &cwd, HashMap::new(), &context)
            .map_err(|_| "plugin_environment_unavailable")?;
        pane.command[0] = crate::plugin_command::program_for_cwd(&pane.command[0], &cwd)
            .display()
            .to_string();
        let (rows, cols) = self.state.estimate_pane_size();
        self.conditional_plugin_operations
            .check_deadline(params.not_after_unix_ms)?;
        let (tab_idx, new_pane) = self.state.workspaces[ws_idx]
            .split_pane_argv_command(
                anchor,
                ratatui::layout::Direction::Horizontal,
                rows.max(4),
                cols.max(10),
                Some(cwd),
                &pane.command,
                env,
                self.state.pane_scrollback_limit_bytes,
                self.state.host_terminal_theme,
                self.state.host_terminal_appearance,
                false,
            )
            .ok_or("plugin_pane_open_uncertain")?
            .map_err(|_| "plugin_pane_open_uncertain")?;
        let pane_id = new_pane.pane_id;
        let terminal_id = new_pane.terminal.id.clone();
        let response = self.finish_plugin_pane_open(
            id.to_owned(),
            ws_idx,
            None,
            Some(tab_idx),
            new_pane,
            plugin.plugin_id.clone(),
            pane,
        );
        let record = self
            .state
            .plugin_panes
            .get_mut(&pane_id)
            .ok_or("ownership_record_missing")?;
        record.ownership = Some(crate::app::state::ConditionalPluginOwnership {
            ownership_nonce: params.ownership_nonce.clone(),
            installation_fingerprint: params.installation_fingerprint.clone(),
            terminal_id: terminal_id.clone(),
        });
        let response = serde_json::from_str::<SuccessResponse>(&response)
            .map_err(|_| "plugin_pane_open_uncertain")?;
        let ResponseResult::PluginPaneOpened { plugin_pane } = response.result else {
            return Err("plugin_pane_open_uncertain");
        };
        Ok(ConditionalOwnedPane {
            pane_id: plugin_pane.pane.pane_id,
            terminal_id: terminal_id.to_string(),
        })
    }
}
