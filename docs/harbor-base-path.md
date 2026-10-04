# Harbor dashboard mount

The dashboard derives its API, token exchange, handoff, attachment and event-stream
URLs from its document path. Root access keeps the existing URLs. Harbor serves
`/storyhook/` using a **strip** mount to the loopback daemon on port 3456.

Harbor must preserve the public Host header. Authentication remains StoryHook's:
unauthenticated public API requests must return 401. Do not rewrite Host to
localhost or enable a proxy authentication bypass.

Validation: `node --test scripts/tests/dashboard-base-path.test.mjs`.
Build: `CARGO_BUILD_JOBS=2 CARGO_NET_OFFLINE=true python3 scripts/build-number.py -- cargo build --release`.

Activation uses a fresh-inode `install -m 755` of the built binary, followed by
`story daemon restart` and the normal `story plugin reinstall`. The supported
graceful restart drains accepted daemon work before startup reconciliation of
external worker runs. It can wait for a long-running verifier; never assume all
work is external or use force-stop merely to accelerate deployment.
