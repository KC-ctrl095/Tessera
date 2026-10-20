# Scheduled jobs
`schedule` manages jobs that run a prompt automatically in a fresh session.
- `schedule`: cron (`"0 9 * * 1-5"` = weekdays 09:00 server time) or `@every 30m` / `@every 6h`.
- `prompt` must be self-contained; the run's final message is the report the user sees.
- `unattended: true` (default) runs without approval prompts, never with more permission than the session that created it.
- Creating, changing or deleting a job needs the user's approval.
- Missed runs catch up after downtime; runs never overlap. Results appear as sessions on every device.
- A run interrupted by a server restart resumes by itself, told which tool calls completed, are uncertain or never ran (see `docs context`).
## Give it an end
- Anything temporary ("check until the ticket is answered", "every day this week", "once on Friday") gets an end. Never fake one with a far-off cron (a yearly date) or leave a "temporary" check running.
- `until`: a date (`2026-10-20` = the end of that day, `2026-10-20 18:00`, `in 3d`); no run starts at or after it (exclusive). `"never"` removes it.
- `max_runs`: at most this many runs (`1` = a one-off at the next scheduled time). `0` removes it.
- `end`, called from a job's own run (or its helpers): the goal is met, no more runs. Write the prompt so a run ends it, e.g. "If the ticket has an answer, tell the user with `notify`, then `schedule` `end`." The run still finishes its report.
- When a schedule ends (date, runs, or `end`), the job stays listed as ended; the user can turn it on again.
## What the user hears
- Every run is listed in the user's notification center (the bell), linked to its chat: finished, failed, stopped, paused, interrupted.
- A run that fails, pauses or is interrupted also notifies the user's devices; one that finishes doesn't. To tell the user what a run found, have its prompt call `notify` (only when there's news); see `docs notifications`.
- `schedule list` shows each job's state, its last run and how it went, and its limits.
