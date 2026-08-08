# Leyden — build and install to a personal (per-user) prefix.
#
# No sudo: this is a one-user, one-machine app, so everything lands under
# ~/.local, which is already on PATH and XDG_DATA_DIRS. Override PREFIX for a
# system install (make PREFIX=/usr/local install, with sudo).

PREFIX  ?= $(HOME)/.local
BINDIR   = $(PREFIX)/bin
DATADIR  = $(PREFIX)/share
APPID    = dev.miguelrincon.Leyden

.PHONY: all build run test check install dev-install refresh-icon-cache uninstall clean

all: build

build:
	cargo build --release

run:
	cargo run

test:
	cargo test

check:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test

install: build dev-install
	install -Dm755 target/release/leyden $(BINDIR)/leyden
	@echo "Installed to $(PREFIX). Launch 'Leyden' from the app grid, or run 'leyden'."

# Everything except the release binary: the .desktop entry and the icons.
dev-install:
	install -Dm644 data/$(APPID).desktop $(DATADIR)/applications/$(APPID).desktop
	install -Dm644 data/icons/hicolor/scalable/apps/$(APPID).svg \
		$(DATADIR)/icons/hicolor/scalable/apps/$(APPID).svg
	install -Dm644 data/icons/hicolor/symbolic/apps/$(APPID)-symbolic.svg \
		$(DATADIR)/icons/hicolor/symbolic/apps/$(APPID)-symbolic.svg
	@$(MAKE) --no-print-directory refresh-icon-cache
	-update-desktop-database -q $(DATADIR)/applications

# A stale icon-theme.cache hides newly installed icons, and one can exist with no
# index.theme beside it — the old guard checked for the index, so on such a
# system the cache was never refreshed and a new icon stayed invisible.
#
# The `touch` is what actually matters: GTK compares the directory's mtime
# against the cache's and rescans when the directory is newer, so bumping it
# retires a stale cache even if the rebuild below does nothing. The rebuild is
# best effort — gtk-update-icon-cache can fail on a hicolor directory populated
# by other apps ("the generated cache was invalid"), and that must never fail an
# install.
refresh-icon-cache:
	@if [ -d $(DATADIR)/icons/hicolor ]; then \
		touch $(DATADIR)/icons/hicolor; \
		gtk-update-icon-cache -q -t -f $(DATADIR)/icons/hicolor 2>/dev/null || \
			echo "note: icon cache rebuild skipped; the directory timestamp was refreshed instead"; \
	fi

uninstall:
	rm -f $(BINDIR)/leyden
	rm -f $(DATADIR)/applications/$(APPID).desktop
	rm -f $(DATADIR)/icons/hicolor/scalable/apps/$(APPID).svg
	rm -f $(DATADIR)/icons/hicolor/symbolic/apps/$(APPID)-symbolic.svg
	@$(MAKE) --no-print-directory refresh-icon-cache
	-update-desktop-database -q $(DATADIR)/applications
	@echo "Uninstalled from $(PREFIX)."

clean:
	cargo clean
