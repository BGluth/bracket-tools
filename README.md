# bracket-tools

Rust mono-repo for esports tournament tooling, primarily targeting [start.gg](https://www.start.gg/).

## Crates

| Crate | Path | Description |
|---|---|---|
| `bracket-tools-core` | `crates/bracket-tools-core` | Normalized data types and traits |
| `bracket-tools-cache` | `crates/bracket-tools-cache` | Generic sled-based caching layer |
| `bracket-tools-query` | `crates/bracket-tools-query` | Abstract query interface (multi-platform) |
| `bracket-tools-startgg-schema` | `crates/bracket-tools-startgg-schema` | cynic codegen types from start.gg GraphQL schema |
| `bracket-tools-startgg` | `crates/bracket-tools-startgg` | Main SDK: caching, rate-limited start.gg client |

## Tools

| Tool | Path | Description |
|---|---|---|
| `reporter-cli` | `tools/reporter/reporter-cli` | ratatui TUI for live set reporting |
| `reporter-state` | `tools/reporter/reporter-state` | Reporter state management |
| `bracket-tools-daemon` | `tools/daemon` | Background scraper daemon |

## License

Licensed under either of

* Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.


### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
