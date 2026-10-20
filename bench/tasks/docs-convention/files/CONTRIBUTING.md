# Contributing to notekit

Thanks for helping. A few conventions keep notekit easy to find your way around;
please follow them in every change.

## Commands

- Each command lives in its own module: `notekit/commands/<name>.py`.
- The module starts with a docstring. Its first line is the command's help text
  (what `python3 -m notekit --help` shows), one short sentence.
- The module has a `run(args) -> int` function that returns the exit code, and an
  `add_arguments(parser)` function when the command takes arguments or options.
- Register the command in `COMMANDS` in `notekit/commands/__init__.py`. Keep the
  entries in alphabetical order.
- Read notes through `notekit.notes` (`load_notes`, `Note.has_tag`) instead of
  opening the files yourself, so every command agrees on what a note and a tag are.

## Errors

Errors go to stderr as `notekit: error: <message>` and the command exits with
code 2. `notekit.errors.fail(message)` does both: `return fail("...")`.
Normal output goes to stdout.

## Tests

Every command has tests in `tests/test_<name>.py`, written with `unittest`.
`tests/helpers.py` gives you a temporary notes folder and a way to run the CLI.
Run everything with:

```sh
python3 -m unittest discover -s tests -t .
```

## Changelog

Every user-visible change gets a line in CHANGELOG.md under `## Unreleased`.
A new command is written as:

```
- Added `<name>` command: <what it does>.
```
