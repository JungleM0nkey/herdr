use super::*;

struct Fixture {
    app: App,
    root: std::path::PathBuf,
    params: ConditionalPluginSetStateParams,
}

impl Fixture {
    fn new() -> Self {
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("conditional")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        let root = std::env::temp_dir().join(format!(
            "herdr-conditional-{}",
            crate::terminal::TerminalId::alloc()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let command = if cfg!(windows) {
            r#"["ping.exe", "-n", "60", "127.0.0.1"]"#
        } else {
            r#"["sh", "-c", "sleep 60"]"#
        };
        std::fs::write(root.join("herdr-plugin.toml"), format!(
            "id = \"example.conditional\"\nname = \"Conditional\"\nversion = \"0.1.0\"\nmin_herdr_version = \"0.6.10\"\n\n[[panes]]\nid = \"board\"\ntitle = \"Board\"\ncommand = {command}\n"
        )).unwrap();
        let plugin = super::super::load_plugin_manifest(root.to_str().unwrap(), true).unwrap();
        app.state
            .installed_plugins
            .insert(plugin.plugin_id.clone(), plugin.clone());
        let anchor = app.state.workspaces[0].tabs[0].root_pane;
        let target = ConditionalPluginTarget {
            server_incarnation: app.conditional_plugin_operations.server_incarnation.clone(),
            workspace_id: app.public_workspace_id(0),
            tab_id: app.public_tab_id(0, 0).unwrap(),
            anchor_pane_id: app.public_pane_id(0, anchor).unwrap(),
            anchor_terminal_id: app.state.workspaces[0]
                .terminal_id(anchor)
                .unwrap()
                .to_string(),
        };
        let fingerprint = installation_fingerprint(&plugin, &plugin.panes[0]).unwrap();
        Self {
            app,
            root,
            params: ConditionalPluginSetStateParams {
                operation_id: "operation-open".into(),
                target,
                plugin_id: plugin.plugin_id,
                entrypoint: "board".into(),
                installation_fingerprint: fingerprint,
                ownership_nonce: "native-owner-1".into(),
                open: true,
                expected_owned_pane: None,
                not_after_unix_ms: current_unix_ms().unwrap() + 60_000,
            },
        }
    }

    fn query(&self) -> ConditionalPluginStateParams {
        let target = &self.params.target;
        ConditionalPluginStateParams {
            target: ConditionalPluginStateTarget {
                server_incarnation: Some(target.server_incarnation.clone()),
                workspace_id: target.workspace_id.clone(),
                tab_id: target.tab_id.clone(),
                anchor_pane_id: target.anchor_pane_id.clone(),
                anchor_terminal_id: target.anchor_terminal_id.clone(),
            },
            plugin_id: self.params.plugin_id.clone(),
            entrypoint: self.params.entrypoint.clone(),
            ownership_nonce: self.params.ownership_nonce.clone(),
            operation_id: Some(self.params.operation_id.clone()),
        }
    }

    fn set(&mut self, params: ConditionalPluginSetStateParams) -> serde_json::Value {
        serde_json::from_str(
            &self
                .app
                .handle_conditional_plugin_set_state("set".into(), params),
        )
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn state(response: &serde_json::Value) -> ConditionalPluginState {
    serde_json::from_value(response["result"]["state"].clone()).unwrap()
}

#[test]
fn conditional_plugin_strict_params_reject_unknown_fields() {
    let fixture = Fixture::new();
    let request = Request {
        id: "strict".into(),
        method: Method::ConditionalPluginSetState(fixture.params.clone()),
    };
    let value = serde_json::to_value(&request).unwrap();
    for path in ["params", "target"] {
        let mut value = value.clone();
        let object = if path == "params" {
            &mut value["params"]
        } else {
            &mut value["params"]["target"]
        };
        object["cwd"] = serde_json::json!("arbitrary");
        assert!(serde_json::from_value::<Request>(value).is_err());
    }
    let mut value = value;
    value["params"]["open"] = serde_json::json!("true");
    assert!(serde_json::from_value::<Request>(value).is_err());
}

#[test]
fn conditional_plugin_preconditions_never_mutate_or_focus() {
    let mut fixture = Fixture::new();
    let focus = fixture.app.state.current_pane_focus_target();
    let original = fixture.params.clone();
    let mut cases = Vec::new();
    let mut params = original.clone();
    params.target.server_incarnation = "old-boot".into();
    cases.push((params, "stale_server_incarnation"));
    let mut params = original.clone();
    params.target.tab_id = "wrong-tab".into();
    cases.push((params, "stale_target"));
    let mut params = original.clone();
    params.target.anchor_terminal_id = "wrong-terminal".into();
    cases.push((params, "stale_target"));
    let mut params = original.clone();
    params.target.workspace_id = "1".into();
    cases.push((params, "stale_target"));
    let mut params = original.clone();
    params.installation_fingerprint = "changed".into();
    cases.push((params, "installation_changed"));
    let mut params = original.clone();
    params.ownership_nonce = "".into();
    cases.push((params, "invalid_params"));
    let mut params = original.clone();
    params.not_after_unix_ms = current_unix_ms().unwrap() - 1;
    cases.push((params, "expired"));
    let mut params = original.clone();
    params.not_after_unix_ms = MAX_SAFE_INTEGER + 1;
    cases.push((params, "invalid_deadline"));
    let mut params = original.clone();
    params.not_after_unix_ms = current_unix_ms().unwrap() + MAX_DEADLINE_MS + 60_000;
    cases.push((params, "invalid_deadline"));
    for (params, code) in cases {
        let response = fixture.set(params);
        assert_eq!(response["error"]["code"], code);
        assert_eq!(fixture.app.state.workspaces[0].tabs[0].panes.len(), 1);
        assert_eq!(fixture.app.state.current_pane_focus_target(), focus);
        assert!(fixture
            .app
            .conditional_plugin_operations
            .receipts
            .is_empty());
    }
    let anchor = fixture.app.state.workspaces[0].tabs[0].root_pane;
    fixture.app.state.workspaces[0]
        .pane_state_mut(anchor)
        .unwrap()
        .plugin_provenance_unknown = true;
    assert_eq!(
        fixture.set(original)["error"]["code"],
        "plugin_provenance_unknown"
    );
}

#[test]
fn conditional_plugin_fingerprint_is_stable_and_covers_installation() {
    let fixture = Fixture::new();
    let original = fixture.app.state.installed_plugins[&fixture.params.plugin_id].clone();
    let first = installation_fingerprint(&original, &original.panes[0]).unwrap();
    let mut plugin = original.clone();
    plugin.source.installed_unix_ms = Some(17);
    assert_eq!(
        installation_fingerprint(&plugin, &plugin.panes[0]).unwrap(),
        first
    );
    plugin.enabled = false;
    assert_ne!(
        installation_fingerprint(&plugin, &plugin.panes[0]).unwrap(),
        first
    );
    let mut pane = original.panes[0].clone();
    pane.command.push("changed".into());
    assert_ne!(installation_fingerprint(&original, &pane).unwrap(), first);
    std::fs::write(fixture.root.join("herdr-plugin.toml"), "changed manifest").unwrap();
    assert_ne!(
        installation_fingerprint(&original, &original.panes[0]).unwrap(),
        first
    );
}

#[tokio::test]
async fn conditional_plugin_open_deduplicate_recover_close_and_foreign_conflict() {
    let mut fixture = Fixture::new();
    let focus = fixture.app.state.current_pane_focus_target();
    let params = fixture.params.clone();
    let first = fixture.set(params.clone());
    let opened = state(&first);
    assert_eq!(
        opened.saved_operation.as_ref().unwrap().outcome,
        ConditionalPluginOutcome::Opened
    );
    assert_eq!(fixture.app.state.workspaces[0].tabs[0].panes.len(), 2);
    assert_eq!(fixture.app.state.current_pane_focus_target(), focus);
    assert_eq!(fixture.set(params.clone()), first);
    let mut changed = params.clone();
    changed.not_after_unix_ms += 1;
    assert_eq!(fixture.set(changed)["error"]["code"], "operation_id_reused");
    let mut foreign = params.clone();
    foreign.operation_id = "foreign".into();
    foreign.ownership_nonce = "other-owner".into();
    assert_eq!(fixture.set(foreign)["error"]["code"], "foreign_plugin_pane");
    let mut query = fixture.query();
    query.target.tab_id = "gone".into();
    let recovered: serde_json::Value = serde_json::from_str(
        &fixture
            .app
            .handle_conditional_plugin_state("query".into(), query),
    )
    .unwrap();
    let recovered = state(&recovered);
    assert!(recovered.live_state_unavailable);
    assert_eq!(recovered.saved_operation, opened.saved_operation);
    assert!(recovered.server_now_unix_ms >= opened.server_now_unix_ms);
    let mut close = params.clone();
    close.operation_id = "operation-close".into();
    close.open = false;
    close.expected_owned_pane = opened.owned_pane.clone();
    let closed = fixture.set(close.clone());
    assert_eq!(
        state(&closed).saved_operation.unwrap().outcome,
        ConditionalPluginOutcome::Closed
    );
    assert_eq!(fixture.app.state.workspaces[0].tabs[0].panes.len(), 1);
    assert_eq!(fixture.app.state.current_pane_focus_target(), focus);
    assert_eq!(fixture.set(close), closed);
    fixture
        .app
        .state
        .installed_plugins
        .get_mut(&params.plugin_id)
        .unwrap()
        .enabled = false;
    assert_eq!(fixture.set(params), first);
}

#[test]
fn conditional_plugin_legacy_and_restored_ownership_is_never_adopted() {
    let mut fixture = Fixture::new();
    let anchor = fixture.app.state.workspaces[0].tabs[0].root_pane;
    fixture.app.state.plugin_panes.insert(
        anchor,
        crate::app::state::PluginPaneRecord {
            plugin_id: fixture.params.plugin_id.clone(),
            entrypoint: fixture.params.entrypoint.clone(),
            ownership: None,
        },
    );
    assert_eq!(
        fixture.set(fixture.params.clone())["error"]["code"],
        "foreign_plugin_pane"
    );
    fixture.app.handle_plugin_unlink(
        "unlink".into(),
        PluginUnlinkParams {
            plugin_id: fixture.params.plugin_id.clone(),
        },
    );
    assert!(
        fixture.app.state.workspaces[0]
            .pane_state(anchor)
            .unwrap()
            .plugin_provenance_unknown
    );
}

#[test]
fn conditional_plugin_boot_and_receipt_capacity_are_hard_fences() {
    let mut fixture = Fixture::new();
    let other = ConditionalPluginOperations::default();
    assert_ne!(
        fixture.params.target.server_incarnation,
        other.server_incarnation
    );
    let mut params = fixture.params.clone();
    params.open = false;
    let response = fixture.set(params.clone());
    assert_eq!(
        state(&response).saved_operation.unwrap().outcome,
        ConditionalPluginOutcome::Unchanged
    );
    let saved = fixture.app.conditional_plugin_operations.receipts[&params.operation_id].clone();
    for index in 1..RECEIPT_CAPACITY {
        fixture
            .app
            .conditional_plugin_operations
            .receipts
            .insert(format!("filled-{index}"), saved.clone());
    }
    params.operation_id = "too-many".into();
    assert_eq!(
        fixture.set(params)["error"]["code"],
        "receipt_capacity_exhausted"
    );
    assert_eq!(fixture.app.state.workspaces[0].tabs[0].panes.len(), 1);
}

#[tokio::test]
async fn conditional_plugin_dispatch_errors_are_uncertain_and_never_retried() {
    let mut fixture = Fixture::new();
    let plugin = fixture
        .app
        .state
        .installed_plugins
        .get_mut(&fixture.params.plugin_id)
        .unwrap();
    plugin.panes[0].command = vec!["herdr-conditional-missing-executable-12781823".into()];
    fixture.params.installation_fingerprint =
        installation_fingerprint(plugin, &plugin.panes[0]).unwrap();
    let params = fixture.params.clone();
    let response = fixture.set(params.clone());
    let receipt = state(&response).saved_operation.unwrap();
    assert_eq!(receipt.outcome, ConditionalPluginOutcome::Uncertain);
    assert_eq!(fixture.set(params.clone()), response);
    let mut different = params;
    different.operation_id = "new-attempt".into();
    assert_eq!(
        fixture.set(different)["error"]["code"],
        "uncertain_operation"
    );
    assert_eq!(fixture.app.state.workspaces[0].tabs[0].panes.len(), 1);
}

#[test]
fn conditional_plugin_query_bootstraps_without_mutating_and_checks_receipt_scope() {
    let mut fixture = Fixture::new();
    let mut query = fixture.query();
    query.target.server_incarnation = None;
    let response: serde_json::Value = serde_json::from_str(
        &fixture
            .app
            .handle_conditional_plugin_state("bootstrap".into(), query.clone()),
    )
    .unwrap();
    assert_eq!(state(&response).target, fixture.params.target);
    assert!(fixture
        .app
        .conditional_plugin_operations
        .receipts
        .is_empty());
    let mut params = fixture.params.clone();
    params.open = false;
    fixture.set(params);
    query.ownership_nonce = "wrong-owner".into();
    let response: serde_json::Value = serde_json::from_str(
        &fixture
            .app
            .handle_conditional_plugin_state("wrong-owner".into(), query),
    )
    .unwrap();
    assert_eq!(response["error"]["code"], "operation_scope_mismatch");
}

#[test]
fn conditional_plugin_actor_clock_never_rewinds_deadline_fences() {
    let mut fixture = Fixture::new();
    let future = current_unix_ms().unwrap() + 120_000;
    fixture.app.conditional_plugin_operations.server_now_unix_ms = future;
    assert!(
        fixture
            .app
            .conditional_plugin_operations
            .observe_clock()
            .unwrap()
            >= future
    );
    let query = fixture.query();
    let response: serde_json::Value = serde_json::from_str(
        &fixture
            .app
            .handle_conditional_plugin_state("clock".into(), query),
    )
    .unwrap();
    assert!(state(&response).server_now_unix_ms >= future);
    assert_eq!(
        fixture.set(fixture.params.clone())["error"]["code"],
        "expired"
    );
    assert!(fixture
        .app
        .conditional_plugin_operations
        .receipts
        .is_empty());
}
