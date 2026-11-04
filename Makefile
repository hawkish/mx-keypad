# Local tasks: build, install and manage the LaunchAgent.
#
# Releasing is not one of them: CI publishes. Bump the version in Cargo.toml,
# refresh Cargo.lock, run `make notes` and write them, then commit the lot as
# "📦 release: vX.Y.Z" and push. The release job in .github/workflows/ci.yml
# tags and publishes once the build is green.
#
# Recipes stay one command per line: macOS ships GNU Make 3.81, which has no
# .ONESHELL, so anything spanning lines has to survive a fresh shell per line.

SHELL := bash
.SHELLFLAGS := -eu -o pipefail -c

LABEL := dk.mortenhogh.mx-keypad
TARGET = gui/$$(id -u)/$(LABEL)
BIN := result/bin/mx-keypad
LOG := $(HOME)/Library/Logs/mx-keypad.log
CONFIG ?= $(HOME)/.config/mx-keypad/config.json

# Cargo.toml is the single source of truth for the version — flake.nix and the
# release job both read it the same way.
VERSION := $(shell awk -F'"' '/^\[package\]/{p=1;next} /^\[/{p=0} p && /^version/{print $$2; exit}' Cargo.toml)
TAG := v$(VERSION)
NOTES ?= .github/release-notes/$(TAG).md
TEMPLATE := .github/release-notes/template.md

.PHONY: help
help:
	@echo "make build      nix build, into ./result"
	@echo "make check      validate CONFIG and render every tile"
	@echo "make test       cargo test and clippy in the dev shell"
	@echo "make install    build, then install and start the LaunchAgent"
	@echo "make restart    restart the running LaunchAgent"
	@echo "make stop       stop the LaunchAgent until the next login or install"
	@echo "make uninstall  stop the LaunchAgent and remove it"
	@echo "make status     show whether the LaunchAgent is running"
	@echo "make log        follow the LaunchAgent's log"
	@echo "make notes      scaffold $(NOTES) from the commits since the last tag"
	@echo
	@echo "CONFIG defaults to $(CONFIG)"

.PHONY: build
build:
	nix build

.PHONY: check
check: build
	$(BIN) check --config "$(CONFIG)"

.PHONY: test
test:
	nix develop -c cargo test
	nix develop -c cargo clippy --all-targets -- -D warnings

# install stops a running agent, copies and signs the binary, and starts the
# new one, so it also covers "rebuild and restart".
.PHONY: install
install: check
	$(BIN) install --config "$(CONFIG)"

.PHONY: restart
restart:
	launchctl kickstart -k $(TARGET)

.PHONY: stop
stop:
	launchctl bootout $(TARGET)

.PHONY: uninstall
uninstall: build
	$(BIN) uninstall

.PHONY: status
status:
	@launchctl print $(TARGET) 2>/dev/null | awk '/^\t(state|pid|runs|last exit code) =/' | grep . || echo "$(LABEL) is not loaded"

.PHONY: log
log:
	tail -n 20 -f "$(LOG)"

# The commit subjects are a starting point, not the notes — rewrite them into
# prose that says what changed and why. Everything below them comes from
# $(TEMPLATE), which is plain markdown you can edit directly.
#
# Commit the result with the release commit: the release job refuses to publish
# without it.
.PHONY: notes
notes: $(TEMPLATE)
	@[ ! -e "$(NOTES)" ] || { echo "notes: $(NOTES) already exists" >&2; exit 1; }
	@mkdir -p "$(dir $(NOTES))"
	@{ echo "## Features"; echo; \
	   git log --reverse --format='- %s' "$$(git describe --tags --abbrev=0)..HEAD"; echo; \
	   sed 's/{{TAG}}/$(TAG)/g' "$(TEMPLATE)"; } > "$(NOTES)"
	@echo "wrote $(NOTES) — edit it, then commit it with the release commit"
