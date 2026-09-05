# Scheduled Tasks

Diapason has two scheduling layers:

- Session-scoped tasks for the current interactive session via narrow reminder
  phrases
- Durable tasks via `diapason schedule` that persist on disk, run while Diapason
  is open, and can keep running after restart when the local scheduler daemon
  is installed

Use session tasks for quick polling while you are actively in chat. Use durable
tasks when you want Diapason to keep checking or reminding you outside the
current session.

## Session-scoped scheduling

Session scheduling is attached to the active interactive session only.
Natural-language one-shot reminders are intentionally narrow:

- `remind me at 3pm to push the release branch`
- `in 45 minutes, check whether the integration tests passed`
- `what scheduled tasks do I have?`
- `cancel <job id|name>`

Examples:

```text
remind me at 15:00 to push the release branch
in 45 minutes, check whether the integration tests passed
```

Behaviour:

- Tasks stay in memory only and disappear when the interactive Diapason session
  exits
- Diapason checks for due tasks once per second
- Due prompts are injected only at idle boundaries, after the current turn
  finishes, so human input stays higher priority
- Times are interpreted in your local timezone
- A session can hold at most 50 scheduled tasks
- One-shot tasks scheduled at `:00` or `:30` may fire up to 90 seconds early

Diapason also exposes one built-in session tool, `cron`, for parity with the
scheduler runtime. It takes an `action`:

- `cron` with `action="create"` — schedule a prompt (cron expression,
  `delay_minutes`, or `run_at`)
- `cron` with `action="list"` — show scheduled prompts
- `cron` with `action="delete"` — remove one by id

(The legacy names `cron_create`, `cron_list`, and `cron_delete` still route to
`cron` as aliases.)

## Durable scheduling

Durable tasks are managed with `diapason schedule`. They are not tied to a chat
session.

Examples:

```bash
diapason schedule create --prompt "check the deployment" --every 10m
diapason schedule create --prompt "review the nightly build" --cron "0 9 * * 1-5"
diapason schedule create --reminder "push the release branch" --at "15:00"
diapason schedule list
diapason schedule delete 1a2b3c4d
```

Command summary:

- `diapason schedule create` creates a durable prompt task or reminder
- `diapason schedule list` lists durable tasks
- `diapason schedule delete <id>` deletes a durable task
- `diapason schedule serve` runs the local scheduler daemon
- `diapason schedule install-service` installs a user service for the daemon
- `diapason schedule uninstall-service` removes the installed user service

Behaviour:

- Task definitions are stored under the Diapason config directory in
  `scheduled_tasks/tasks/`
- Runtime state and claim files are stored under the Diapason data directory in
  `scheduled_tasks/`
- Prompt tasks run by spawning a fresh `diapason exec` process in the configured
  workspace
- Durable tasks are polled while an interactive Diapason session is open
- `diapason schedule serve` or an installed service keeps durable tasks running
  when Diapason is not open
- Reminder tasks surface a local Diapason notification without invoking the
  model
- macOS uses a user LaunchAgent
- Linux uses a `systemd --user` service
- Windows durable scheduler service management is not yet supported

One-shot durable tasks keep their run state after firing so
`diapason schedule list` can still show the last outcome.

## Config and disable flags

Scheduled tasks are disabled by default.

```toml
[automation.scheduled_tasks]
enabled = false
```

Set `DIAPASON_DISABLE_CRON=1` to disable all Diapason scheduler entry points,
including reminder interception, scheduler tools, and `diapason schedule`.

## Security model

Diapason supports its internal scheduler, but still blocks OS-level task
schedulers such as `crontab` and `at` from agent-issued shell commands.

- Use reminders for session-scoped work
- Use `diapason schedule` for durable local automation
- Do not route automation through raw shell scheduling commands
