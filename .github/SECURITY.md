# Security policy

## Supported versions

Only the latest release gets security fixes. Upgrade before reporting if you can.

## Reporting a vulnerability

Please do not open a public issue. Report it privately through [GitHub's security advisories](https://github.com/jordond/dfdu/security/advisories/new), or email contact@jordond.dev if you can't use GitHub.

Include the dfdu version (`dfdu --version`), your OS, and steps to reproduce. You should get a reply within a week. Once a fix is released, the advisory is published with credit to you, unless you'd rather stay anonymous.

## Scope

Of particular interest:

- Deleting, trashing or reading anything outside what the user selected, including through symlinks, mount points or races between scan and delete
- The desktop app's privileged helper (SMAppService on macOS, pkexec on Linux), such as running it from an unexpected client or getting it to act on unexpected paths
- Remote mode over ssh
- The install script and release artifacts
