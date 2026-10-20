# birdsong

Finds and tags bird calls in field recordings from our heathland survey.

- `src/birdsong/`: the tagger.
- `data/raw/`: the field recordings (see `data/README.md` before touching anything in `data/`).
- `data/derived/`: features computed from the recordings (`make derive`).
- `build/`: wheels and the compiled FFT helper (`make build`).
- `.cache/`: spectrograms cached by the tagger; safe to delete, they are recomputed when needed.
- `logs/`: the tagger's daily logs.

```sh
make test     # run the tests
make derive   # recompute data/derived from data/raw
```
