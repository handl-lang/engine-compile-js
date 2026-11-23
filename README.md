# engine-compile-js

`handl-plugin-compile-js` — JavaScript compiler for the Handl arrow
calculus. Part of the `engine-compile-*` compiler-backend family: lowers Handl
`Term` arrow terms to JavaScript source.

## Development

```bash
cargo run --manifest-path xtask/Cargo.toml -- gate   # fmt → clippy → test → doc
```

`xtask/` provides the gate tasks (`fmt`, `clippy`, `test`, `doc`, `check`,
`gate`); `ci.sh` is the same gate entry for the bupha shell runner. Deps
are git deps on `branch = "main"`; locally the workspace-root
`.cargo/config.toml` patches them to sibling checkouts (dev-only, never
committed).

## License

MIT.
