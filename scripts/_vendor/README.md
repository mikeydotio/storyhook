# Vendored script dependencies

Tomli 2.4.1 provides TOML parsing for the lifecycle audit on Python 3.9–3.10.
Python 3.11+ uses standard-library `tomllib`. No network or package installation
is needed to collect evidence, generate the report, or run its contracts.

- Upstream: https://github.com/hukkin/tomli
- Release: https://pypi.org/project/tomli/2.4.1/
- Source wheel: `tomli-2.4.1-py3-none-any.whl`
- Wheel SHA-256: `0d85819802132122da43cb86656f8d1f8c6587d54ae7dcaf30e90533028b49fe`
- License: [MIT](tomli/LICENSE), preserved with the source copyright headers.
- Local modifications: none. The four Python modules and `py.typed` are copied
  byte-for-byte; the wheel's license is placed beside them.

When updating, verify the pinned wheel checksum from PyPI, preserve its license,
replace the source modules together, and run the lifecycle audit contracts with
both system Python and Python 3.11+. The contract includes TOML pointer syntax,
malformed/duplicate keys, read-only collection, and deterministic rendering.
