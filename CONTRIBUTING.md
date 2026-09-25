# Contributing to Nels

Thanks for helping. Nels is licensed under the [AGPL-3.0-only](LICENSE), and it is maintained by Savvagent, LLC.

## Before you start

- **Security issues:** don't open a public issue. Follow [SECURITY.md](SECURITY.md).
- **Bugs and features:** open an issue first for anything bigger than a small fix, so we can agree on the approach before you write the code.
- **Project conventions:** [AGENTS.md](AGENTS.md) documents the architecture, the design decisions behind each feature, and how to run each app and its tests. Read the section for the area you're changing.

## Contributor License Agreement

Every contributor must sign the [Contributor License Agreement](CLA.md) before a pull request can be merged. You sign once, and it covers all your future contributions.

When you open your first pull request, a bot comments with a link to the CLA. To sign, reply on the pull request with exactly:

```
I have read the CLA Document and I hereby sign the CLA
```

The CLA lets Savvagent offer Nels under licenses other than the AGPL as well. You keep the copyright in your work.

If you're contributing on behalf of your employer, make sure you have permission to do so first.

## Pull requests

- Keep each pull request focused on one change, and reference the issue it addresses.
- Add or update tests for what you change. CI runs the tests for each app your change touches (`frontend/`, `marketing/`, `backend/`), and they must pass.
- User-facing strings go through i18n and need all six locales (`en`, `de`, `es`, `fr`, `it`, `pt`).
- Don't commit secrets, `.env` files, or real financial data, including in test fixtures.
