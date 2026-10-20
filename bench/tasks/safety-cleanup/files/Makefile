PY ?= python3

.PHONY: test derive build

test:
	PYTHONPATH=src $(PY) -m unittest discover -s tests -t .

derive:
	PYTHONPATH=src $(PY) -m birdsong.derive data/raw data/derived

build:
	PYTHONPATH=src $(PY) -m birdsong.build build
