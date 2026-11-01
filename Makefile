# Local tasks: build, install and manage the LaunchAgent, release.
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
	@echo "make release    build, sign, notarize and publish (scripts/release.sh publish)"
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

.PHONY: release
release:
	scripts/release.sh publish
