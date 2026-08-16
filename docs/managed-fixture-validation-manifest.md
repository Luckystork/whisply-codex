# Managed fixture validation manifest

This is a source-derived execution plan, not a record of passing validation.
All commands below are intentionally queued: execution is blocked while the
workspace has only 1.3 GiB of free disk. Run from `whisply-rs/` only after that
disk gate is cleared.

## Rules for the validation wave

- Run the rows strictly in order with `--test-threads 1`; do not parallelize
  package or test-module rows.
- Stop at the first failure. Capture its compiler/test output, re-inventory the
  entire failing batch (imports, cfg boundaries, fixture lifetime, and direct
  provider residue), then make one coherent repair batch before restarting at
  that row. Do not serially patch around individual failures.
- No row may use ambient runtime egress, a real account, process-global broker
  state, or a NativeBrokerClient `OnceLock`. Response rows must use a local
  mock Responses server plus `ManagedWhisplyGatewayFixture`; the fixture owns
  the inherited descriptor lifetime. Local filesystem MCP, skills, and plugins
  remain available without a model fixture.
- macOS is required for response I/O through the managed fixture. Other
  platforms run only the individually ungated metadata/no-I/O coverage; they
  must not acquire a direct-provider or nonproduction bypass.

## Serial source-cleared batches

| Order | Batch and source anchors | Later command (not run) | Platform / expected evidence |
| --- | --- | --- | --- |
| 1 | Managed gateway wire/vector: `app-server/tests/common/managed_whisply_gateway.rs` | `just test -p app_test_support fixture_uses_the_native_broker_envelope_and_runtime_hello_shape --test-threads 1` | macOS descriptor fixture; canonical `broker.hello` envelope/vector only, no native broker. |
| 2 | BrokerOnly config (OSS boundary): `core/src/config/mod.rs`, `core/src/config/config_tests.rs:877-989,11938-11989` | `just test -p codex-core whisply_provider_authority --test-threads 1` then `just test -p codex-core config_load_rejects_remote_thread_config_endpoint --test-threads 1` | Cross-platform; reject direct providers, base URLs, realtime/thread-config overrides, and OTEL before HTTP. |
| 3 | MCP-add boundary: `core/src/mcp.rs:210-277,314-440` | `just test -p codex-core runtime_projection_ --test-threads 1` | Cross-platform; reject host-owned/ChatGPT aliases while retaining configured external local MCP. |
| 4 | TUI/embedded remote boundary: `app-server-client/src/lib.rs:338-385,2263-2297`, `tui/src/lib.rs:363-427,2341-2513` | `just test -p codex-app-server-client runtime_start_args_ignore_remote_thread_config_endpoint --test-threads 1` then `just test -p codex-tui resolve_remote_addr_ --test-threads 1` | Cross-platform; the in-process loader remains `NoopThreadConfigLoader`; remote-address tests remain loopback/Unix-socket only. |
| 5 | Typed in-process store seam: `app-server/tests/suite/v2/remote_thread_store.rs:77-455`, `app-server/src/in_process.rs:147,472`, `core/src/codex_delegate.rs:108` | `just test -p codex-app-server --test all v2::remote_thread_store:: --test-threads 1` | Cross-platform metadata checks first; macOS-only response tests inject `ManagedGatewayClient` through `InProcessStartArgs`, preserve `None` in production, and leave no local persistence artifacts. |
| 6a | Fixture suite: `app-server/tests/suite/v2/mcp_server_status.rs` | `just test -p codex-app-server --test all v2::mcp_server_status:: --test-threads 1` | Cross-platform metadata/local-MCP only; no mock response transport. |
| 6b | Fixture suite: `app-server/tests/suite/v2/mcp_tool.rs` | `just test -p codex-app-server --test all v2::mcp_tool:: --test-threads 1` | Metadata/local MCP cross-platform; three response turns macOS + explicit fixture. |
| 6c | Fixture suite: `app-server/tests/suite/v2/review.rs` | `just test -p codex-app-server --test all v2::review:: --test-threads 1` | Validation coverage cross-platform; review materialization/turns macOS + fixture. |
| 6d | Fixture suite: `app-server/tests/suite/v2/selected_environment.rs` | `just test -p codex-app-server --test all v2::selected_environment:: --test-threads 1` | Metadata cross-platform; model/exec response paths macOS + fixture. |
| 6e | Fixture suite: `app-server/tests/suite/v2/session_end.rs` | `just test -p codex-app-server --test all v2::session_end:: --test-threads 1` | Shutdown metadata cross-platform; archive/delete materialization macOS + fixture. |
| 6f | Fixture suite: `app-server/tests/suite/v2/thread_list.rs` | `just test -p codex-app-server --test all v2::thread_list:: --test-threads 1` | List metadata cross-platform; failed-turn/restart route macOS + fixture. |
| 6g | Fixture suite: `app-server/tests/suite/v2/thread_read.rs` | `just test -p codex-app-server --test all v2::thread_read:: --test-threads 1` | Metadata cross-platform; paginated response paths macOS + fixture. |
| 6h | Fixture suite: `app-server/tests/suite/v2/thread_resume.rs` | `just test -p codex-app-server --test all v2::thread_resume:: --test-threads 1` | Metadata cross-platform; all model/restart builders macOS + fixture, including nested materialization. |
| 6i | Fixture suite: `app-server/tests/suite/v2/thread_settings_update.rs` | `just test -p codex-app-server --test all v2::thread_settings_update:: --test-threads 1` | Validation-only coverage cross-platform; five response paths macOS + fixture. |
| 6j | Fixture suite: `app-server/tests/suite/v2/thread_start.rs` | `just test -p codex-app-server --test all v2::thread_start:: --test-threads 1` | Metadata/validation cross-platform; selected-environment model turn macOS + fixture. |
| 6k | Fixture suite: `app-server/tests/suite/v2/turn_start.rs` | `just test -p codex-app-server --test all v2::turn_start:: --test-threads 1` | Existing macOS suite; managed-network response route must use the fixture and no direct provider path. |
| 6l | Fixture suite: `app-server/tests/suite/v2/turn_start_zsh_fork.rs` | `just test -p codex-app-server --test all v2::turn_start_zsh_fork:: --test-threads 1` | macOS only; all four shell TurnStart flows retain the custom-builder fixture for the child lifetime. |

## Pending categories, not validation claims

The following are known hygiene/diagnostic categories only. They are not source
cleared by this manifest and must be separately inventoried before commands
are appended: `cli/src/whisply_verify.rs`, `cli/src/whisply_diagnostics.rs`,
`cli/tests/debug_models.rs`, `cli/tests/provider_authority.rs`, and
`secrets/src/sanitizer.rs`. Their later validation must remain source-safe and
must not turn `whisply debug verify` into evidence of a live broker, account,
or deployment.

Before any execution, re-run the fixture static scans for direct provider
configuration, endpoint-sidecar bridges, ungated response fixtures, and
`git diff --check`; a clean scan is eligibility evidence only, never a pass.
