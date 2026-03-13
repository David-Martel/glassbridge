# Contributing

## Local setup

```powershell
npm install
npm run bootstrap:tools
npm run hooks:install
```

Use the root scripts instead of ad hoc workspace commands so the CargoTools
bridge, local `.cargo` configuration, `sccache`, and staging behavior stay
consistent.

## Validation workflow

- `npm run validate:staged` for the fast pre-commit path
- `npm run validate:release` for the pre-push path
- `npm run test:desktop` for desktop shell validation
- `npm run secrets:repo` and `npm run secrets:history` for publication hygiene

## Secret hygiene

- Local device registry files under `data/` are ignored and must never be committed.
- Release artifacts under `staging/desktop/` are generated locally and ignored.
- Pre-commit and pre-push hooks run secret scans before validation.

## Pull requests

- Keep commits semantically grouped.
- Update docs when behavior, workflows, or operator UX changes.
- Include exact validation commands in the PR description for hardware-facing changes.
