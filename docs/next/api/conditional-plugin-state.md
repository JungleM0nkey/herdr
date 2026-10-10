# Conditional plugin pane commands

The JSON-only `plugin.pane.state.v1` and `plugin.pane.set_state.v1` methods are
advertised by `ping.capabilities.conditional_plugin_pane_v1` and the endpoint
method list. They do not change generation-1 binary codecs. Older servers do
not support these methods; never fall back to legacy plugin pane commands.

A read-only state request binds an exact current workspace, tab, anchor pane,
and attached terminal. Numeric aliases and previous pane aliases are rejected.
Omit `server_incarnation`, or set it to `null`, only while discovering a target:

```json
{
  "id": "inspect",
  "method": "plugin.pane.state.v1",
  "params": {
    "target": {
      "server_incarnation": null,
      "workspace_id": "w1",
      "tab_id": "w1:t1",
      "anchor_pane_id": "w1:p1",
      "anchor_terminal_id": "term_example"
    },
    "plugin_id": "example.board",
    "entrypoint": "board",
    "ownership_nonce": "locally-generated-owner-nonce",
    "operation_id": null
  }
}
```

The `conditional_plugin_state` result wraps `state`: `api_version: 1`, a concrete
`target`, `server_now_unix_ms`, `installation_fingerprint`, `owned_pane`,
`conflict`, `saved_operation`, and `live_state_unavailable`. A matching pane
without this nonce, installation fingerprint, and attached terminal is a
conflict. A tab containing any restored or unlinked pane is refused because its
plugin provenance is unknown. Create a fresh local tab instead of adopting a
pane by command or label; moving a restored pane does not remove this fence.

A mutation takes exactly `operation_id`, the concrete `target`, `plugin_id`,
`entrypoint`, `installation_fingerprint`, `ownership_nonce`, `open`,
`expected_owned_pane`, and `not_after_unix_ms`:

```json
{
  "id": "open",
  "method": "plugin.pane.set_state.v1",
  "params": {
    "operation_id": "immutable-operation-id",
    "target": {
      "server_incarnation": "server_example",
      "workspace_id": "w1",
      "tab_id": "w1:t1",
      "anchor_pane_id": "w1:p1",
      "anchor_terminal_id": "term_example"
    },
    "plugin_id": "example.board",
    "entrypoint": "board",
    "installation_fingerprint": "sha256-from-state",
    "ownership_nonce": "locally-generated-owner-nonce",
    "open": true,
    "expected_owned_pane": null,
    "not_after_unix_ms": 1791670000000
  }
}
```

`expected_owned_pane` is either `null` or the exact
`{ "pane_id": "...", "terminal_id": "..." }` from state. `null` requires that
no matching plugin pane exists in the target tab. Opening creates a right split
without changing focus. No caller-supplied command, cwd, environment, placement,
or remote machine is accepted. Closing affects only the exact owned pane.
Unknown fields are rejected, including in nested targets.

The fingerprint covers source, full resolved Git commit, canonical root,
manifest content, host platform, enabled state, and entrypoint fields. It
excludes installation timestamps. Cooperating installers and uninstallers hold
an exclusive per-plugin lock across checkout replacement or removal;
conditional requests take a nonblocking shared lock and report
`plugin_installation_busy` instead of waiting. Registry locks are never held
across installer-to-server RPC. Direct edits to local plugin files are outside
that cooperative lock; installed local plugin code remains trusted.

The server stores execution intent before dispatch and keeps at most 4,096
receipts per boot. It rejects new operations at capacity without evicting old
receipts or performing an effect. An identical operation ID and complete typed
request returns its historical result before checking current state or expiry;
reusing an ID with different content is rejected. Each receipt contains
`operation_id`, `request_digest`, `outcome`, `owned_pane`, and `error_code`.
Outcomes are `opened`, `closed`, `unchanged`, and `uncertain`. Any failure after
dispatch begins is uncertain and is never retried automatically. Another
command for that plugin entrypoint in the same tab is refused while an
uncertain operation remains.

The request digest is lowercase SHA-256 of `serde_json::to_vec` of the complete
typed mutation struct, in its declared field order (as shown above). The nested
target order is also as shown; an owned pane orders `pane_id`, then
`terminal_id`. Missing optional fields normalize to explicit null values.

Read state with an `operation_id` to recover its saved receipt. Plugin,
entrypoint, and nonce must match. If live state is unavailable, the saved
receipt is still returned with `live_state_unavailable: true` and an explicit
conflict; historical ownership is not presented as current ownership.
Operation IDs and nonces must be 1–128 ASCII letters, digits, `-`, `_`, `.`, or
`:`. Deadlines must be safe JavaScript integers, later than the server clock,
and at most one hour away. The deadline is checked immediately before mutation.
The actor retains a boot-local high-water mark of observed wall-clock time;
deadline checks and returned `server_now_unix_ms` both use it, so a clock
rollback cannot reopen an expired operation.

The 128-bit OS-random server incarnation changes on every restart or live
handoff; receipts and conditional ownership are not persisted across boots.
An RNG failure disables the conditional API instead of weakening this fence.
A saved operation's absence after its deadline, observed using
`server_now_unix_ms` on the same incarnation, establishes that an earlier
synchronous dispatch has completed and any later queued dispatch will expire.
Absence before the deadline, a different server, or a replaced socket proves
no such outcome. The native client must also ensure no dispatch is still queued
locally and must not resend an expired operation with a new deadline.

These methods provide targeting and cooperative ownership, not caller
authentication. The native application must authorize each command and retain
its own durable receipt ledger.
