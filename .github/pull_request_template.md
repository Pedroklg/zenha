## Summary

<!-- Describe what this PR changes and why. -->

## Type of change

- [ ] Bug fix (`fix`)
- [ ] New feature (`feat`)
- [ ] Refactor / chore (`refactor`, `chore`)
- [ ] Documentation (`docs`)
- [ ] CI / build (`ci`, `build`)

## Verification

- [ ] `cargo fmt --all` and `cargo ci` pass on Windows (`x86_64-pc-windows-msvc`)
- [ ] Manual testing on Windows performed (describe below if audio, video, UI, hotkeys, or tray were changed)

### Manual checks (if applicable)

<!-- List what you tested manually on Windows, or note what still requires manual testing. -->

## Checklist

- [ ] Commit messages follow Conventional Commits
- [ ] Crate boundaries in `CLAUDE.md` are respected (`livekit` stays isolated in `rooms-core`, UI thread stays non-blocking)
- [ ] UI strings are in Brazilian Portuguese; identifiers and `tracing` logs are in English
- [ ] No secrets, `.env` values, or local-only files included
