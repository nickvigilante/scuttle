# scuttle

scuttle is an unofficial, full-screen terminal client for Coder Agents, written in Rust on Ratatui.
It is a personal project, and it is not built, supported, or endorsed by Coder.

## Building and running

```sh
cargo run --release
```

scuttle reuses the session the `coder` CLI stored at `coder login`, so log in with the CLI first.

## License

MIT. See [LICENSE](LICENSE).

scuttle depends on [unofficial-coder-sdk-rs](https://github.com/nickvigilante/unofficial-coder-sdk-rs), which is licensed under the AGPL-3.0, so a built `scuttle` binary includes AGPL-3.0 code.
