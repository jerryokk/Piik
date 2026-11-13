# Third-Party Licenses

Piik-owned code uses the root [MIT license](../LICENSE). Dependencies retain
their own licenses; MIT does not replace their copyright notices or terms.

The [pinned source index](./upstream.json) records exact upstream commits and
SHA-256 values for Cloudflared, the WebRTC SDK, and npm packages or Rust crates
whose published package omits license files. When the upstream repository has no
license file either, the entry pins the canonical SPDX/Mozilla text of the
declared license and names that repository revision.
The retained text normalizes line endings and incidental whitespace only.
The Cloudflared text includes its root and dependency LICENSE, COPYING, and
NOTICE files, including platform variants. Generate its dependency inventory
with `go mod vendor` at the pinned upstream revision. The protobuf
text also preserves Google's BSD-licensed varint attribution.
The Windows capture adapter statically links the pinned WebRTC SDK, including
its libvpx encoder. The App's native notices retain the SDK's collected
third-party license texts; these dependencies do not become MIT-licensed merely
because Piik's adapter is MIT-licensed.

`npm run dev` and `npm run build:web` generate `/third-party-licenses.txt`
offline from installed, lockfile-matched Web runtime dependencies.
The Server release and App assembly each add the Rust standard library notice
and, from offline `cargo metadata`, the license expression and texts of every
third-party crate linked into that binary for `x86_64-pc-windows-msvc` (normal
dependencies only; build scripts, proc-macros and dev-dependencies are not
shipped). The App package also carries the pinned Cloudflared notices. Missing
license texts or changed pinned contents stop packaging.
Any copied or modified upstream file must retain its attribution and mark
the modification. Prefer public API composition over a maintained source fork.

Dependency updates are explicit rebuilds, not live updates in a running binary.
Review relevant upstream advisories and actual reachable dependencies at release;
an affected serious vulnerability warrants an earlier update. Mature media code
is not exempt from vulnerability checks or focused upgrade verification.

Update the corresponding source text and index when a pinned runtime changes.
No license-scanning dependency is needed.
