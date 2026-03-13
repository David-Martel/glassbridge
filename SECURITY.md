# Security Policy

## Supported scope

This repository is a local-first Android device access toolchain. The most
important security properties are:

- no secrets or credentials committed to git history
- local device registry data kept out of version control
- operator-visible tooling for device approval and command execution

## Reporting

Open a private GitHub security advisory or contact the maintainer directly for
issues that could expose credentials, device identifiers, or unsafe remote
control behavior.

## Secret handling

- `gitleaks` is enforced in pre-commit, pre-push, and CI.
- Generated screenshots, local databases, and staged release binaries are ignored by git.
- `.cargo/config.toml` stays machine-local and is ignored.
