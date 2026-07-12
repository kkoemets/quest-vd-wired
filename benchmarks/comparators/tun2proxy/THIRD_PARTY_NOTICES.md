# Third-party notices

## tun2proxy

- Official repository: <https://github.com/tun2proxy/tun2proxy>
- Crate version: `0.8.2`
- Pinned `v0.8.2` revision: `eed123fbbec06295bf83f9be36d5a0f64ed9a8cb`
- Revision date inspected: 2026-06-08
- License: MIT
- Upstream `LICENSE` SHA-256: `8cddc80ccbbb14a8a3d7fee1fc1795d7fcd647f4c7063ad95246f9ff24b407c7`

The exact revision is locked in both `Cargo.toml` and `Cargo.lock`. Its
transitive Rust dependency versions and source checksums are recorded in
`Cargo.lock`; this experiment is not release packaging for the product.

## Known adoption blockers

- `socks5-impl 0.8.7` is an unconditional tun2proxy dependency and declares
  `GPL-3.0-or-later`. A linked comparator executable is therefore not a
  permissive-only artifact and must not be shipped with the Apache-2.0 product.
- `daemonize 0.5.0` is reported by `RUSTSEC-2025-0069` as unmaintained.
- `paste 1.0.15`, pulled through the Linux netlink graph, is reported by
  `RUSTSEC-2024-0436` as unmaintained.
- `tun 0.8.13` declares `WTFPL`, but its published crate contains no standalone
  license file. Its licensing evidence requires manual review even for an
  isolated experiment.

CI may build and test this benchmark in isolation and may record narrowly
scoped audit exceptions for the two unmaintained warnings. It must not suppress
the GPL finding globally, merge this graph into the host workspace, generate a
release binary from it, or treat a synthetic benchmark as adoption evidence.

Direct dependency license expressions from the locked Cargo metadata are:

| Package | Locked version | License expression |
|---|---:|---|
| anyhow | 1.0.103 | MIT OR Apache-2.0 |
| chrono | 0.4.45 | MIT OR Apache-2.0 |
| clap | 4.6.1 | MIT OR Apache-2.0 |
| serde | 1.0.228 | MIT OR Apache-2.0 |
| serde_json | 1.0.150 | MIT OR Apache-2.0 |
| sha2 | 0.10.9 | MIT OR Apache-2.0 |
| tempfile (tests only) | 3.27.0 | MIT OR Apache-2.0 |
| tokio | 1.52.3 | MIT |
| tun | 0.8.13 | WTFPL |
| tun2proxy | 0.8.2 | MIT |

The published `tun` 0.8.13 crate declares `WTFPL` in its manifest but does not
include a standalone license file. A full transitive SBOM and license audit is
therefore still an adoption gate; this experiment is not a redistributable
product artifact.

MIT License

Copyright (c) @ssrlive, B. Blechschmidt and contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
