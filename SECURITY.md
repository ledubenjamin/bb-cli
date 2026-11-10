# Security

Security fixes target the latest stable release. Pre-release builds and older versions may require an upgrade; no long-term maintenance branches are currently promised.

Report a suspected vulnerability using [GitHub private vulnerability reporting](https://github.com/ledubenjamin/bb-cli/security/advisories/new). This requires the repository owner to enable the feature when opening the repository. If the form is unavailable, contact the owner through the contact information on their GitHub profile before sharing sensitive details. Do not open a public issue with an exploitable vulnerability or credentials.

Include the affected version and platform, impact, and a minimal reproduction with synthetic credentials. Avoid including actual access tokens, OAuth secrets, private log content, or keyring exports.

`bb` stores saved credentials in the native OS keyring and accepts environment-injected tokens for automation. `BB_CONFIG_DIR` isolates configuration and its keyring namespace; it is not a place to store tokens. Local logs can contain sensitive application output, so handle the log cache and exported logs accordingly.

Release downloads include SHA-256 checksums for integrity. Checksums distributed alongside an archive do not independently authenticate a compromised release source. Installers verify checksums and the binary version before replacing an installed executable.
