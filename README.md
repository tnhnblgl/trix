# Trix

A clip tool for the audience Medal.tv serves badly: hardware capture and encode that costs
approximately zero game fps, and a desktop app that does not spend the frames it just saved you.
See [`PLAN.md`](PLAN.md) for the capture engine and
[`docs/superpowers/specs/2026-07-26-trix-desktop-ui-design.md`](docs/superpowers/specs/2026-07-26-trix-desktop-ui-design.md)
for the tray daemon and desktop app.

## Running the app

From `crates/trix-ui`:

```
cargo tauri dev
```

runs the desktop app in development, with the frontend under `web/` hot-reloading.

```
cargo tauri build --no-bundle
```

produces a release binary (`target/release/trix-ui.exe`) without packaging it into an installer. The
MSI is a later plan.
