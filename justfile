set shell := ["bash", "-cu"]
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

# ---------------------------------------------------------------------------
# Cross-platform Cargo recipes.
# ---------------------------------------------------------------------------

# Build the driver executable in debug mode.
build:
    cargo build --locked --bin firebird-plugin

# Build for release (what the GitHub Actions workflow ships).
release:
    cargo build --release --locked --bin firebird-plugin

# Run unit tests.
test:
    cargo test --workspace --locked

# Launch the local REPL that simulates Tabularis JSON-RPC calls over stdio.
repl: build
    cargo run --bin test_plugin

# Run clippy on the workspace.
lint:
    cargo clippy --all-targets --no-deps --locked -- -D warnings

# Format the codebase.
fmt:
    cargo fmt --all

# ---------------------------------------------------------------------------
# Platform-specific recipes (file operations + plugin-dir conventions).
# ---------------------------------------------------------------------------

# Build and copy the driver and manifest into Tabularis's plugin folder.
[linux]
dev-install: build
    mkdir -p ~/.local/share/tabularis/plugins/drivers/firebird
    cp target/debug/firebird-plugin ~/.local/share/tabularis/plugins/drivers/firebird/
    cp .tabularium ~/.local/share/tabularis/plugins/drivers/firebird/
    @echo "Installed to ~/.local/share/tabularis/plugins/drivers/firebird"
    @echo "Restart Tabularis (or toggle the plugin in Settings) to pick up changes."

[macos]
dev-install: build
    mkdir -p "$HOME/Library/Application Support/tabularis/plugins/drivers/firebird"
    cp target/debug/firebird-plugin "$HOME/Library/Application Support/tabularis/plugins/drivers/firebird/"
    cp .tabularium "$HOME/Library/Application Support/tabularis/plugins/drivers/firebird/"
    @echo "Installed to ~/Library/Application Support/tabularis/plugins/drivers/firebird"
    @echo "Restart Tabularis (or toggle the plugin in Settings) to pick up changes."

[windows]
dev-install: build
    $dest = Join-Path $env:APPDATA "tabularis\plugins\drivers\firebird"; \
    New-Item -ItemType Directory -Force -Path $dest | Out-Null; \
    Copy-Item "target\debug\firebird-plugin.exe" $dest; \
    Copy-Item ".tabularium" $dest; \
    Write-Host "Installed to $dest"; \
    Write-Host "Restart Tabularis (or toggle the plugin in Settings) to pick up changes."

# Remove the kind-scoped development install. Manage old flat copies in Settings.
[linux]
uninstall:
    rm -rf ~/.local/share/tabularis/plugins/drivers/firebird

[macos]
uninstall:
    rm -rf "$HOME/Library/Application Support/tabularis/plugins/drivers/firebird"

[windows]
uninstall:
    $dest = Join-Path $env:APPDATA "tabularis\plugins\drivers\firebird"; \
    if (Test-Path $dest) { Remove-Item -Recurse -Force $dest }
