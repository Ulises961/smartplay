# Contributing

Thanks for helping. Contributions are accepted under the project's licence, GPL-3.0-or-later.

## Set up

```bash
./scripts/fetch-deps.sh     # Navidrome's plugin SDK into .deps/, once per clone
lefthook install            # enables the commit checks below (https://lefthook.dev)
```

## Check your work

```bash
lefthook run local-verify   # format, clippy, tests and the WASM plugin build: what CI runs
```

Format with `rustfmt --edition 2021 $(git ls-files 'crates/**/*.rs')`, not `cargo fmt`: cargo also formats path
dependencies, and `.deps/` is not ours. The plugin builds only for `wasm32-wasip1`, so plain `cargo test` skips it
(`cargo test --workspace --exclude smartplay-plugin`).

## Commits

[Conventional commits](https://www.conventionalcommits.org): `type(scope): description`, with type one of `feat`,
`fix`, `docs`, `style`, `refactor`, `test`, `chore`, `perf`, and a subject of at most 100 characters. Scopes in use:
`core`, `plugin`, `engine`, `ci`, `deps`, `hooks`. Keep commits small, each one building and passing the tests. The
commit-msg hook checks the format.

## Pull requests

`main` only moves through pull requests, and the hooks refuse a commit on `main` or a push that updates it.

1. Branch from an up-to-date `main`: `git switch main && git pull --ff-only && git switch -c <type>/<short-description>`.
2. Push the branch and open a pull request. Its title is a plain sentence saying what changed; the description says
   what and why.
3. CI has to pass. Pull requests are squash-merged and the branch is deleted.

## Privacy

smartplay has no telemetry and no accounts of its own. Do not add any, and keep credentials, hostnames and real library
data out of the repository (tests use synthetic names).
