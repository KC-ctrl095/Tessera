# Build helpers for the handbook site.
#
# make         build the site into site/
# make serve   preview it on http://localhost:8000
# make clean   remove the build output

PYTHON ?= python3
PAGES := $(wildcard pages/*.md)
OUT := site

.PHONY: all build serve clean

all: build

build: $(OUT)/index.html

$(OUT)/index.html: $(PAGES) tools/render.py
	@mkdir -p $(OUT)
	$(PYTHON) tools/render.py pages $(OUT)

serve: build
	cd $(OUT) && $(PYTHON) -m http.server 8000

clean:
	rm -rf $(OUT)
