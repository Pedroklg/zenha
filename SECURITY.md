# Security Policy

## Reporting a Vulnerability

Please **do not** report security vulnerabilities through public GitHub issues, discussions, or pull requests.

Instead, report vulnerabilities privately via GitHub's private vulnerability reporting:

- Open a draft security advisory at: <https://github.com/Pedroklg/zenha/security/advisories/new>

Include as much detail as possible to help reproduce and assess the issue:

- Affected component (`zenha` desktop client, `zenha-server`, installer, or deployment config)
- Version or commit hash
- Clear steps to reproduce or proof of concept
- Potential impact

## Scope

### In Scope

- The Zenha Windows desktop client (`zenha-client` / `zenha.exe`) and core library (`zenha-core`), including `zenha://` URI handling, file transfer parsing/limits, and credential storage
- The token server (`zenha-server`), including invite code verification, Argon2 password handling, and JWT generation
- Shared protocol validation (`zenha-protocol`)
- Official release artifacts and installer script (`installer/zenha.iss`)
- Default server deployment configurations (`deploy/`)

### Out of Scope

- Vulnerabilities solely in upstream dependencies (`livekit-server`, `libwebrtc`, `eframe`, etc.) unless they are uniquely exposed or worsened by Zenha's integration
- Denial-of-service or unauthorized testing against self-hosted Zenha/LiveKit instances you do not own
- Social engineering or physical access attacks

## Supported Versions

Security fixes are applied to the `main` branch and released in the latest minor/patch version.
