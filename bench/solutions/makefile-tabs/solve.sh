set -e
python3 - <<'PY'
p = "Makefile"
s = open(p).read()
s = s.replace(".PHONY: all build serve clean", ".PHONY: all check build serve clean")
s = s.replace("all: build\n", "all: check build\n\ncheck:\n\t$(PYTHON) tools/lint.py pages\n")
open(p, "w").write(s)
PY
make all
