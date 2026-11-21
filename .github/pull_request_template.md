## Why

<!-- The problem this solves, or the reason for the change. Link the issue if there is one. -->

## What changes

<!-- The approach, and anything a reviewer should know: a breaking change, a protocol change, a merge order for stacked pull requests. -->

## How it was tested

<!-- Commands you ran and what they showed. A claim that something is faster needs before and after numbers and how they were measured. -->

## Checklist

- [ ] `make ci` passes (format, clippy, tests, schema check), and for a change under `clients/`, that client's checks and `make scenarios` against it
- [ ] Bug fixes and behaviour changes come with a test that fails without them
- [ ] Protocol changes: `popsub_protocol` and its tests updated, `make schema` run, `docs/DESIGN.md` section 4 updated, nothing removed or renamed within the protocol version
- [ ] Delivery behaviour changes: `docs/DESIGN.md` section 6 and the README's guarantees updated
- [ ] New settings: `settings.rs`, `config/default.toml` and `docs/DESIGN.md` section 9 updated
- [ ] Client behaviour changes: a scenario that fails without them, and every client library and driver updated
- [ ] User-visible changes: a line in `CHANGELOG.md`
- [ ] Docstrings follow `docs/STYLE.md`, and `grep -rnE --include='*.rs' '^\s*//[^/!]' crates` prints nothing
