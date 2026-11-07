# Mobile architecture

## Fit the shell's content area

Design for the applet's content viewport on a narrow phone. Apell applies the device safe-area insets and reserves space for its title bar and controls. Let the applet fill the remaining area. Start with a single column, let labels and content wrap, and give the main content one scroll owner. In flex or grid layouts, let that region shrink with `min-height: 0`. Keep the focused field and primary action reachable as the keyboard reduces the viewport.

Give the applet an internal Back action when it has nested screens. Apell's Close returns to the applet library. Test landscape, larger text, and keyboard focus on the actual host. Add wider layouts when the extra space helps the task.

## Choose where work runs

- Use a frontend-only applet when view-owned work and host storage meet the requirements.
- Use a Rust backend for applet commands and jobs that should outlive the view. Native pickers, sensors, model selection, audio, and speech still belong to the attached view.
- Close and Reload detach the current view. Its bridge, scoped handles, and view-owned jobs are revoked. An accepted guest command can continue while the host process runs. The OS can suspend or terminate that process.
- On attachment, subscribe to relevant change events before fetching a snapshot. Treat events as a reason to refresh because they can be missed while detached. Let the snapshot restore the screen from authoritative applet state.
- Persist records, drafts, and job state that must survive process termination. Recover stale pending work as interrupted and expose the appropriate resume, retry, or discard action. Release view subscriptions, timers, and object URLs with the view.

Companion illustrates a view attaching to an existing chat backend and recovering interrupted replies. [Companion source](https://github.com/agentic-rs/apell/tree/main/examples/chatbot) · [Runs and views](https://github.com/agentic-rs/apell/blob/main/docs/architecture.md#runs-and-views)
