# porchlight

A tiny relay that forwards webhook calls from our build server to the team chat.
It runs as a single Python process and keeps no state of its own.

## How it works

The relay listens on port 8086. When the build server posts a JSON event to
`/hooks/build`, porchlight checks the shared token, formats a short message and
sends it on to every chat room listed in `rooms.toml`.

Rooms that should recieve only failures can set `only_failures = true`.

## Running it

```sh
python3 -m porchlight --rooms rooms.toml --port 8086
```

Set `PORCHLIGHT_TOKEN` in the environment first. Requests without the token are
answered with 401 and dropped.

## Testing

```sh
python3 -m unittest discover -s tests
```

## Notes for operators

- The relay retries a chat delivery twice before giving up and logging it.
- If a room has been removed on the chat side, you will recieve a warning in
  the log on the first failed delivery; remove the room from `rooms.toml`.
- Messages longer than 2,000 characters are cut and end with an ellipsis.
