# Security Policy

## Supported Versions

We support only the latest release. Update to the latest version before
you report a bug.

## Reporting a Vulnerability

Do not open a public issue for a security problem.

Report vulnerabilities through GitHub private vulnerability reporting:

https://github.com/junixdev/chargecap/security/advisories/new

## Scope

The `chargecapd` daemon runs as root and writes to the SMC (System
Management Controller). A bug in `chargecapd` or in the socket protocol
can affect your system. These bugs are in scope for a security report.

## Response Time

We aim to send a first reply within 7 days of your report.
