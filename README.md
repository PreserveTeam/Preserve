# Preserve

Preserve is a small, native Windows application for downloading and installing supported preserved games. It verifies every downloaded file, installs declared prerequisites, supports concurrent and resumable downloads, and records completed installations for compatible clients.

The application is intentionally focused: choose a game, choose a folder, and install it.

## Features

- Native Windows interface built with Rust and egui
- Concurrent, resumable downloads
- SHA-256 verification for every file
- Automatic prerequisite installation
- Multiple simultaneous game installations
- Machine-readable installation index at `%USERPROFILE%\.preserve\index.json`

## Build

Install the stable Rust toolchain for `x86_64-pc-windows-msvc`, then run:

```powershell
./build-release.ps1
```

The script remaps local source paths before compiling so the executable does not expose the build machine's username or checkout location. The executable is written to `target\release\preserve.exe`.

Run the test suite with:

```powershell
cargo test
cargo clippy --all-targets -- -D warnings
```

`PRESERVE_API_URL` can be set at compile time or runtime to use a different compatible API endpoint.

## Project boundaries

This repository contains only the Preserve desktop client. It does not contain game files, manifests, credentials, private server code, or deployment infrastructure. Downloaded games remain subject to their respective owners' terms and copyrights.

## Contributing

Bug reports and focused pull requests are welcome. Please run formatting, tests, and Clippy before submitting changes. See [CONTRIBUTING.md](CONTRIBUTING.md) for details.

## License

Preserve is licensed under the [Apache License 2.0](LICENSE). Bundled font assets retain their original licenses; see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
