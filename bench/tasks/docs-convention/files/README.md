# notekit

Small command-line tools for a folder of markdown notes.

```sh
python3 -m notekit list                 # every note with its title
python3 -m notekit list --tag ideas     # only notes tagged "ideas"
python3 -m notekit show groceries       # print notes/groceries.md
python3 -m notekit --dir ~/journal list # another notes folder
```

## Notes

A note is a `.md` file directly in the notes folder (`./notes` unless you pass
`--dir`). Its name is the file name without `.md`. The first line may list tags:

```
tags: work, ideas
# Better standups
...
```

Tags are case-insensitive: `Ideas` and `ideas` are the same tag. Notes without a
`tags:` line have no tags.

See CONTRIBUTING.md before adding a command.
