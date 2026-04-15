# Dashboard Production UI Plan

## Purpose

The dashboard is now functionally broad, but it is not yet visually production-ready.
This plan defines the work needed to turn the current local operator interface into a polished, dark-first, responsive control surface without violating the control-plane and safety boundaries in `AGENTS.md`.

## Audit Snapshot

Audit date: 2026-04-15

Current evidence from the running dashboard and codebase:

- The active theme is now dark-first in [apps/dashboard/src/styles.css](</C:/repos/TV_bot_core/apps/dashboard/src/styles.css>) with shared tokens for canvas, panel, text, and mode accents.
- The main dashboard surface has been decomposed substantially: [apps/dashboard/src/App.tsx](</C:/repos/TV_bot_core/apps/dashboard/src/App.tsx>) is now primarily composition/orchestration, while monitoring, control-center, and workflow concerns live in dedicated component and hook modules.
- The shared dashboard stylesheet is still global, but it now carries the redesigned shell, control-center, monitoring, and responsive hardening rules instead of the earlier light-theme scaffold.
- A real browser audit across `390px`, `768px`, `1024px`, and `1440px` now shows no page-level horizontal overflow in the current dashboard build.
- The worst field and button spillover issues are fixed through dedicated form-grid rules for settings, manual entry, and operator action forms.
- The control center and monitoring deck now have clearer hierarchy, but the interface still contains some verbose development-oriented copy and a few dense lower-priority sections.
- The live, paper, and observation distinctions are materially stronger than before, but final production sign-off should still include one more deliberate operator-ergonomics review.

## Progress Snapshot

The original seven-phase plan is no longer theoretical. Current state by phase:

- Phase 1 `dark-mode foundation`: substantially complete
- Phase 2 `information architecture reset`: substantially complete
- Phase 3 `control-surface redesign`: substantially complete
- Phase 4 `monitoring, history, and audit redesign`: substantially complete
- Phase 5 `responsive and overflow hardening`: materially in place, with browser-verified page-level overflow checks now passing at `390px`, `768px`, `1024px`, and `1440px`
- Phase 6 `frontend structure cleanup`: substantially in place through extracted monitoring/control components plus dedicated runtime-host, strategy, settings, controller, and projection modules
- Phase 7 `QA and acceptance`: in progress, with browser-based responsive QA now added to the evidence, but final release sign-off still pending

## Design Constraints

These rules stay fixed during the redesign:

- The dashboard must remain a client of the local control plane only.
- The frontend must not become the source of truth for runtime state.
- Dangerous actions must continue to require backend-backed confirmations and audit logging.
- Live and paper modes must remain impossible to confuse.
- The execution core must remain strategy-agnostic.

## Target Outcome

The finished interface should feel like a production operations console:

- dark-first by default, with strong contrast and calm visual hierarchy
- clearly separated mode signaling for `paper`, `live`, and `observation`
- zero horizontal scrolling at supported widths
- no text spilling out of fields, pills, cards, or tables
- fast operator scanning for state, safety blockers, and required actions
- a cleaner split between control, monitoring, audit, and configuration tasks
- a smaller and more maintainable frontend code surface

## Delivery Phases

### Phase 1: Dark-Mode Foundation

Ship the new visual foundation before rearranging the whole surface.

- Replace the light-first root theme with a dark-first token set.
- Define dashboard CSS variables for:
  - background layers
  - panel surfaces
  - text tiers
  - mode accents
  - warning/danger/healthy tones
  - borders, radii, shadows, and spacing
- Keep `paper`, `live`, and `observation` visually distinct using consistent accent rails, badges, and hero treatment instead of relying mostly on background tint.
- Tighten typography using a clearer display/body/mono hierarchy so operational data reads faster.
- Standardize focus, hover, disabled, and confirmation states for buttons and inputs.

### Phase 2: Information Architecture Reset

Reorganize the page so the first screen answers the operator's most important questions immediately.

- Move runtime mode, arm state, warmup state, dispatch availability, account, and active safety review into a compact top command/status rail.
- Separate the dashboard into clear zones:
  - operate
  - monitor
  - audit
  - configure
- Reduce the number of equal-weight full-width panels visible at once.
- Keep the highest-value operator actions above the fold.
- Move lower-frequency configuration and deep audit views below the primary control area or into sectioned layouts.

### Phase 3: Control-Surface Redesign

Make the operator controls feel intentional instead of form-heavy.

- Refactor the current control center into grouped action modules:
  - runtime mode and arm state
  - strategy selection and upload
  - warmup and flow control
  - manual operator actions
  - runtime settings
- Replace long default reason strings with concise defaults and clearer helper text.
- Ensure all form controls can shrink safely without clipping or forcing horizontal overflow.
- Improve spacing and grouping for action rows so dangerous actions are visually isolated.
- Make confirmations feel deliberate and readable in dark mode.

### Phase 4: Monitoring, History, And Audit Redesign

Turn the lower half of the interface into cleaner operational insight instead of a stack of dense cards.

- Redesign health, readiness, latency, history, journal, and events using a shared card system with clearer section headers and stronger data hierarchy.
- Reduce repetitive explanatory paragraphs in empty states.
- Group repeated event feed items more cleanly.
- Make trade, PnL, and latency sections easier to scan with cleaner sub-layouts.
- Improve definition-list rendering for long values such as file paths, routes, and provider details.

### Phase 5: Responsive And Overflow Hardening

Treat layout safety as a release gate, not polish.

- Add responsive layout targets for at least:
  - `390px`
  - `768px`
  - `1024px`
  - `1440px`
- Eliminate all horizontal document overflow at supported widths.
- Add explicit CSS handling for:
  - long file paths
  - account names
  - provider details
  - metric labels
  - button rows
  - reason fields
- Use wrapping, truncation, scroll containers, or stacked mobile layouts intentionally instead of letting intrinsic widths win.

### Phase 6: Frontend Structure Cleanup

The visual redesign should also reduce implementation risk.

- Break [apps/dashboard/src/App.tsx](</C:/repos/TV_bot_core/apps/dashboard/src/App.tsx>) into focused sections and shared primitives.
- Move dashboard UI into a component structure such as:
  - `components/layout`
  - `components/control`
  - `components/monitoring`
  - `components/audit`
  - `components/shared`
- Split the stylesheet into theme/layout/component files or another documented structure with clear ownership.
- Keep data loading and control-plane integration centralized while moving presentational rendering into smaller components.

### Phase 7: QA And Acceptance

Do not call the redesign done without explicit verification.

- Add frontend tests for the key rendering states that currently regress easily.
- Add a viewport regression pass for the supported breakpoints.
- Add at least one automated overflow audit in browser tooling for mobile and desktop widths.
- Review keyboard focus order and accessible names for dangerous controls.
- Re-run dashboard build and test checks after each major slice.

## Immediate Work Order

The fastest path to a production-ready dashboard is:

1. dark-mode token system and shell refresh
2. status rail plus control-center redesign
3. responsive overflow hardening
4. monitoring/audit visual cleanup
5. component decomposition and test hardening

## Acceptance Bar For Dashboard Visual Completion

The dashboard visual redesign is not complete unless all of the following are true:

- default dashboard theme is dark-first
- live, paper, and observation states are unmistakable at a glance
- no supported viewport has horizontal page scroll
- no operator control field clips or spills text in normal use
- the top viewport shows the critical runtime state and safety posture without scrolling
- control actions, monitoring, audit, and configuration areas are visually distinct
- empty states are concise and production-appropriate
- the interface feels like an operator console, not a development scaffold

## Documentation Follow-Up

Keep these docs aligned while the redesign lands:

- [README.md](</C:/repos/TV_bot_core/README.md>)
- [apps/dashboard/README.md](</C:/repos/TV_bot_core/apps/dashboard/README.md>)
- [docs/architecture/current_status.md](</C:/repos/TV_bot_core/docs/architecture/current_status.md>)
