# Piik Documentation

[About Piik](../README.md) · [中文介绍](../README.zh-CN.md) · [中文上手指南](./guide/getting-started.zh-CN.md)

For using Piik, troubleshooting or self-hosting, start with the
[reader documentation](./guide/README.md) · [中文文档中心](./guide/README.zh-CN.md).
This page indexes developer and maintainer references.

## Developer And Maintainer Entry Points

| Task | Guide |
| --- | --- |
| Try or change the source code | [Run from source](#run-from-source) |
| Find a module, interface, lifecycle or source entry point | [Engineering maps and source index](./architecture/modules.md#repository-layout) |
| Find the shared product, design and lifecycle rules | [Standards index](./standards/README.md) |
| Deploy or configure a service | [Self-hosting](./operations/self-hosting.md) · [Configuration](./standards/configuration.md) |
| Build App packages and native capture | [App development](../crates/piik-app/DEVELOPMENT.md) |
| Build, publish or recover a maintained release | [Maintainer release tooling](./operations/release.md) |
| Send a useful bug report | [Diagnostics and export](./standards/configuration.md#diagnostics) |
| Translate Piik or improve wording | [Translation guide](./guide/translating.md) · [中文翻译教程](./guide/translating.zh-CN.md) |
| Check release and platform readiness | [Current status](./status.md) |

The introduction, getting-started, App, self-hosting and translation guides are available in English and Chinese.
Technical references keep one shared version.

## Run from source

You can try the Browser application locally on Windows with Node from
[.node-version](../.node-version), npm from
[package.json](../package.json), and Rust at the `rust-version` in [Cargo.toml](../Cargo.toml).
Run these commands from the repository root.

Build once, then choose a run target:

```sh
npm ci
npm run build:app      # Browser UI, Rust binaries and the native capture helper
```

`npm run build:app` also stages the capture helper and tunnel beside the App, so
the App and the Browser have the same capabilities afterwards. Add `--no-native`
to skip the C++ helper when only the Browser UI matters.

### Browser

Two terminals. The UI proxies `/api`, `/healthz` and `/signal` to the server, so
the ports are fixed by that pairing:

```sh
npm run dev            # UI on http://localhost:8787
```
```powershell
npm run dev:server     # server on 8788, public origin 8787
```

Open [localhost:8787](http://localhost:8787), start sharing, and open the
invitation in another tab to try watching. This localhost invitation stays on
your computer; remote invitations need a reachable origin as described in
[configuration](./standards/configuration.md). Browser capture requires
`localhost` or HTTPS, which this address satisfies.

`npm run dev:server` sets `PORT` and `PUBLIC_BASE_URL` itself; extra arguments
pass through to the server, e.g. `npm run dev:server -- --debug`.

### App

```sh
npm run app
```

The App is the same product with its own room authority: no second terminal and
no proxy. `npm run app -- --no-build` restarts it from the current build, and
`--fresh` clears the per-user configuration first.

The App has no console, so a startup failure looks like a window that never
opens. When that happens, run it with diagnostics instead of guessing:

```powershell
npm run app -- --no-build --debug --log-dir build\app-diag
Get-Content build\app-diag\client.log -Tail 20
```

A configuration written by a different build is rejected at startup — the
product keeps one current version and does not migrate — so `--fresh` is the
recovery for that case. See [App development](../crates/piik-app/DEVELOPMENT.md)
for packaging and the helper layout.

## Work on Piik

[Contributing](../CONTRIBUTING.md) covers the workflow and choosing focused checks.
Run everything a change must pass with one command:

```sh
npm run check
```

It runs these in increasing cost, stopping at the first failure:

| Step | What it proves |
| --- | --- |
| `check:web` | Types, unit tests and the production build |
| `check:docs` | Documentation links and ownership |
| `check:size` | Per-file size limits |
| `check:rust` | Format, Clippy with warnings denied, workspace tests, native capture build and probe |
| `check:hygiene` | Tracked paths, whitespace and the two checks above |

Run a single step when iterating; `check:web` and `check:rust` are the two that
catch most mistakes. Browser, network and hardware acceptance live in the
`gate:*` commands because they need real devices or a reachable network;
[Contributing](../CONTRIBUTING.md) explains when to run them.

Use the [naming and copy guide](./standards/naming.md) for product names,
commands, role labels and Chinese/English voice.
Piik-owned code uses [MIT](../LICENSE); see the
[licensing guide](../licenses/README.md) for third-party components.

## Understand the product

| Topic | Its reference |
| --- | --- |
| Invitations, room codes and passwords | [Rooms and access](./standards/rooms-access.md) |
| How viewers connect | [Routing and transport](./standards/routing-transport.md) |
| Screen capture, quality controls and sound | [Media quality](./standards/media-quality.md) |
| Host and viewer behavior | [Presentation and lifecycle](./standards/presentation-lifecycle.md) |
| How the system is put together and why | [Architecture](./architecture/README.md) |
| Measured behavior and platform limits | [Verification status](./verification-status.md) |

## Shared conventions

| When changing… | Follow this owner |
| --- | --- |
| Module responsibilities or interfaces | [Engineering and contract map](./architecture/modules.md) |
| Product/tool names, role labels or copy | [Naming and copy](./standards/naming.md) |
| UI colour, illustration, motion or layout | [Visual language](./standards/visual-language.md) |
| Overlays, titles or state indicators | [Media status](./standards/media-status.md) |
| Build identity, protocols or stored formats | [Versions and identity](./standards/versioning.md) |
| Documentation or a duplicated rule | [Documentation ownership](./standards/documentation.md) |
| Checks, PRs, integration or cleanup | [Contributing](../CONTRIBUTING.md) |

For ongoing work, use [TODO](./todo.md). [Project memory](./project-memory.md)
provides the compact product map; [status](./status.md) indexes current source
and deployment boundaries. Completed work lives in Git history and pull requests.
