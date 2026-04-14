# Paper Tests

Paper-mode acceptance is currently exercised primarily through the kernel-backed host tests in `apps/runtime/src/host.rs` so the same audited control path is covered as the operator UI and CLI.

Current host-level coverage includes:

- manual paper entry with broker-side stop-loss and take-profit brackets
- manual paper entry remaining blocked while disarmed until the runtime is explicitly armed
- repeated paper manual-entry regression coverage through the same runtime host path
- paper scale-in dispatch when the loaded strategy allows adding size
- flatten, close-position, and cancel-working-order flows
- operator no-new-entry gating and degraded-feed no-new-entry blocking
- startup and reconnect review detection across position-only, working-orders-only, and mixed-exposure scenarios
- startup and reconnect operator decisions for `close_position`, `leave_broker_protected`, and `reattach_bot_management`
- a broader host-level paper release sweep that combines repeated healthy-session entry/gating behavior with startup-review resolution plus cancel/close operator actions

This directory remains the place for any broader black-box paper/demo campaigns that should live outside the runtime-host crate-level acceptance suite.
