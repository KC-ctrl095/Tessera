# Notifications and files for the user
## `notify`: tell the user on their phone and computers
- An alert on devices taking notifications, even with Tessera closed. One live alert per chat or job, replaced rather than stacked. An active device keeps the others quiet. Opening the chat anywhere clears its alerts.
- Use it when the user asked to be told ("let me know when…", "remind me…"), or for a result they're waiting on during long, background or scheduled work. Not for progress, and not when they're clearly chatting with you right now.
- `title`: the headline (≤ 80 characters). `body`: the point in plain text (≤ 600), e.g. the answer or what they need to do. No markdown.
- `at`: send it later, a reminder: `in 20m`, `in 1h 30m`, `15:30`, `3pm`, `tomorrow 9:00`, `2026-10-02 14:00` (server time). The result gives an id; `cancel: <id>` removes it.
- `urgent: true` only when they must act soon (it alerts like an approval).
- Recurring checks must notify only for meaningful changes, requested results, or action needed. Never send routine all-clear or unchanged status on every run. Give temporary checks an end (`until`, `max_runs`, `end` in `docs scheduling`).
- Repeated scheduled messages go quietly to the inbox. Nonurgent job alerts are limited to one an hour. New urgent failures bypass that limit, repeated urgent failures wait fifteen minutes. Don't mark routine status urgent.
- The bell keeps explicit messages, approvals and useful results. Routine job runs stay in task history. Limits for other explicit messages remain 12 a chat per hour and no identical message within 10 minutes.
- A phone with Tessera closed checks about every 15 minutes (on time for scheduled ones when the phone allows it); open or "Stay connected: always", at once.
## `share_file`: give the user a file
- Puts a file from the server into the chat as a download card; one tap saves it to the phone's Downloads or (computer) where they pick. Up to 1 GB. The card is a copy made when shared: later changes to the file don't change it.
- Use it when the user wants the file itself (a report, export, image, archive, build): "send me…", "I need it on my phone". Otherwise answer in the chat.
- Never: Tessera's own files, credential files (`.env`, keys, `.ssh`, `.aws`…) or anything containing a private key, one of Tessera's own keys or (in a text file) an access token: refused. A file holding one of the user's saved passwords or keys asks the user first, naming it. Share a redacted copy instead, or say where the file is.
- Make the file first (e.g. write a CSV, zip a folder with `bash`), then share it; `name` sets the download name.
## Web view downloads
What the server's browser downloads (the user's clicks in a web view, or yours with `browser`) lands in the browser's downloads folder and is offered to the user in the web view's tile and as a toast, ready to save on their device.
