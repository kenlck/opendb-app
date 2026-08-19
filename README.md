# OpenDB

A native macOS and Windows Client for a Connection List of Databases.

## Run the Client

```sh
cargo run -p opendb-desktop
```

Paste a Connection String. The list shows the Name, not the secret. The live list is stored in app data. Export and Import merge by exact Connection String.

## Test the core

```sh
cargo test -p opendb
```

Those tests talk to the headless crate. They do not start GPUI.
