# Contributing

Thanks for taking the time to help.

## Before you start

- For bugs, open an issue with steps to reproduce and the output of
  `FERRY_LOG=ferry=debug ferry`.
- For new features or larger changes, open an issue first so we can agree on the
  approach before you spend time on code.
- Small fixes (typos, docs, obvious bugs) can go straight to a PR.

## Development setup

You need Rust 1.92 or newer and the system libraries GPUI uses on Linux. On
Debian or Ubuntu:

    sudo apt-get install build-essential pkg-config libasound2-dev \
      libfontconfig-dev libfreetype-dev libssl-dev libvulkan1 libwayland-dev \
      libx11-xcb-dev libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev libzstd-dev

Run what CI runs with:

    cargo fmt --check
    cargo clippy --locked --all-targets -- -D warnings
    cargo test --locked

The live SFTP and S3 tests skip themselves unless a server is running. To run
them, start the local servers in `testing/` (see `testing/README.md`).

## Pull requests

- Branch from `main` and name it `<type>/<topic>`, for example `fix/s3-empty-prefix`.
- Keep PRs focused on one change. Separate refactors from behavior changes.
- PR titles follow [Conventional Commits](https://www.conventionalcommits.org/)
  (`feat:`, `fix:`, `docs:`, `perf:`, `refactor:`, `test:`, `ci:`, `chore:`). We squash
  merge, so the title becomes the commit message and the changelog entry.
- Add tests for bug fixes and new behavior.
- For UI changes, attach a before and after screenshot. `--features snapshot`
  can render one without opening a window (see the README).
- For performance changes, include numbers measured the way the README's
  Performance section describes.
- Update the README and the changelog's Unreleased section if users will notice
  the change.
- Mark breaking changes with `!` (`feat!: ...`), for example a change to the
  bookmarks file format, and explain the migration in the PR.

## Review

A maintainer will try to respond within a week. CI must pass before merge. If a PR goes
quiet for a month we may close it, and you are welcome to reopen.

## License

By contributing you agree that your contributions are licensed under the MIT license.
