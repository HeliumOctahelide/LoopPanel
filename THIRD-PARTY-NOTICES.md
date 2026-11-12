# Third-Party Notices

**English** | [简体中文](THIRD-PARTY-NOTICES.zh-CN.md)

LoopPanel uses these direct Rust dependencies. Exact transitive versions are recorded in `Cargo.lock`.

| Component | Version | License |
|---|---:|---|
| anyhow | 1.0.104 | MIT or Apache-2.0 |
| image | 0.25.10 | MIT or Apache-2.0 |
| libloading | 0.9.0 | ISC |
| minijinja | 2.21.0 | Apache-2.0 |
| resvg | 0.47.0 | MIT or Apache-2.0 |
| serde | 1.0.229 | MIT or Apache-2.0 |
| toml | 0.9.12+spec-1.1.0 | MIT or Apache-2.0 |
| toml_parser | 1.1.3+spec-1.1.0 | MIT or Apache-2.0 |
| windows-sys | 0.61.2 | MIT or Apache-2.0 |

Optional CPU temperature support embeds the `IntelMSR` and `AMDFamily17` modules from [PawnIO.Modules 0.2.10](https://github.com/namazso/PawnIO.Modules/tree/0.2.10), loading one according to the CPU vendor. Both are LGPL-2.1-or-later. This source tree retains their `.bin` and `.p` files and `COPYING`. The release's `licenses/PawnIO.Modules-0.2.10/` also includes the full official tagged source archive, including `include/pawnio.inc` and build tasks.

Official module package: [release_0_2_10.zip](https://github.com/namazso/PawnIO.Modules/releases/download/0.2.10/release_0_2_10.zip).

SHA-256:

```text
971c7c974c538b62ac020e0442fa99d0423417bfb496dfe9a4a43ccc0abc0e63
```

Corresponding source: [0.2.10 tag archive](https://github.com/namazso/PawnIO.Modules/archive/refs/tags/0.2.10.zip), distributed as `PawnIO.Modules-0.2.10-source.zip`.

SHA-256:

```text
afb96a3d6f562350d3cd0b0af1ca3dc5c3d53ff6dc4d28b15d23f015b2a4030d
```

PawnIO 2.x and `PawnIOLib.dll` are optional runtime dependencies installed separately by the user; they are not bundled in LoopPanel releases.

Device startup timing, UYVY conversion, and frame transport are based on the Windows USBPcap capture implementation in [Fakinvisibility/b360gt-driver](https://github.com/Fakinvisibility/b360gt-driver/tree/94ae7f2a710123b582ca3fa806d85ee1c684e287), MIT License, Copyright (c) 2026 B360GT contributors.

`THIRD-PARTY-LICENSES.txt` collects license texts shipped with the locked dependencies.
