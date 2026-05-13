# CrossMTP Agent Guide

This file applies to the entire repository.

## Project Status

This project is currently in the planning / early implementation stage.

At the time this file was written, the primary source of truth is:

* `docs/cross-mtp-dev-plan.md`

If code, docs, or tasks conflict with older assumptions, align the work with that plan unless the user explicitly overrides it.

## Product Direction

CrossMTP is an Android MTP desktop transfer app.

Current delivery order:

1. macOS MVP
2. Linux expansion
3. Windows expansion

Do not treat Windows as a simple port of the macOS/Linux backend. The current project direction assumes:

* macOS MVP: `libmtp`-based
* Linux: likely reuses the same family of backend patterns
* Windows: separate WPD backend work

## MVP Priority

For the macOS MVP, optimize for reliability over breadth.

The MVP should focus on:

* device connection detection
* directory browsing
* upload
* download
* drag-and-drop upload
* progress reporting
* cancellation
* understandable error messages
* conflict handling for duplicate file names

The MVP should not expand scope unless the user explicitly asks for it.

Out of scope by default:

* full dual-pane file manager UX
* rename / delete / create-folder flows
* media preview or thumbnails
* large pre-emptive directory caching
* multi-device support
* background auto-reconnect
* Windows-specific implementation work

## Architecture Expectations

Prefer a separation between:

1. MTP Session Layer
2. Transfer Orchestrator Layer
3. Presentation Layer

When implementing backend abstractions:

* do not force all platforms into an unrealistically identical API
* prefer capability-driven design where platform differences are explicit
* keep transfer state transitions explicit and inspectable

Transfer logic should model state clearly. At minimum, work should account for states such as:

* `queued`
* `validating`
* `transferring`
* `cancelling`
* `completed`
* `failed`
* `cancelled`

## Engineering Rules

Prefer:

* small, testable increments
* failure-aware designs
* explicit state machines over implicit async behavior
* minimal caching with clear invalidation rules

Avoid:

* broad platform-generalized abstractions too early
* UI-first work that outruns transfer reliability
* claiming "instant", "zero-latency", or "automatic recovery" behavior unless implemented and tested

Do not introduce large feature scope increases while working on the macOS MVP unless the user asks for them directly.

## Failure Handling Requirements

This project is highly sensitive to failure scenarios. Work should explicitly consider:

* cable disconnect during transfer
* device lock during transfer
* missing MTP authorization on the device
* duplicate file name conflicts
* insufficient target storage
* user cancellation
* app interruption during transfer

When implementing or reviewing behavior, prioritize:

* clear user-visible error states
* no UI freeze
* no ambiguous completion state
* safe queue termination or continuation behavior

## Testing Expectations

Do not treat CI/build success as sufficient proof of correctness.

Real-device testing matters for this project. When claiming work is complete, note:

* what was tested
* what was not tested
* which failure scenarios remain unverified

If no real-device test was run, say that explicitly.

## Documentation Rules

Prefer Korean for product planning and project documentation unless an existing file is clearly English-first.

Code identifiers, API names, and concise technical comments can remain in English.

When changing scope, milestones, or MVP boundaries, update the relevant docs so they remain aligned with implementation.

## Implementation Bias

When in doubt, choose the option that makes the macOS MVP more shippable, debuggable, and honest about its limitations.
