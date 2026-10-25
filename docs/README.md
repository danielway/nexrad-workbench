# Engineering docs

Reference material for working on NEXRAD Workbench. For building and running
the app, see [CONTRIBUTING.md](../CONTRIBUTING.md). For what each radar product
shows and how the app handles it, see [PRODUCTS.md](PRODUCTS.md).

| Document | What it covers |
|----------|----------------|
| [ARCHITECTURE.md](../ARCHITECTURE.md) | The map of the codebase: modules, data flow, key types, async model, caching, UI layout |
| [ARCHITECTURE_DIAGRAMS.md](ARCHITECTURE_DIAGRAMS.md) | The same structure as diagrams, plus runtime sequence and flow diagrams |
| [CORE_SHELL.md](CORE_SHELL.md) | The binding rule: functional core, thin shell, and how to test against it |
| [RENDERING.md](RENDERING.md) | The GPU pipeline: raw gate values, shaders, color tables, 3D globe and volume rendering |
| [STREAMING.md](STREAMING.md) | Real-time chunk streaming: sequencing, timing and live-edge prediction |
| [TIMING.md](TIMING.md) | The three kinds of time: observed, projected collection and projected availability |
| [INDEXEDDB.md](INDEXEDDB.md) | The browser cache: schema, eviction and the transaction rules |
| [PRODUCT.md](PRODUCT.md) | The product spec and decision record for the timeline, playback and acquisition model |
| [activity index](../tools/activity_index/README.md) | The nightly job that builds the storm-activity index on gh-pages |

Images used by the top-level README are in [media/](media/).
