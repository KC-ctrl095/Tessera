// Where Tessera lives and how it is installed. The address comes from
// `repository` in the workspace Cargo.toml (vite.config.ts reads it), the one
// place it is set; the installer keeps a copy at the top of scripts/install.sh.

/** Tessera's public home, e.g. its GitHub page. */
export const REPO_URL: string = __TESSERA_REPO__;

/** The one command that installs (or updates) Tessera on a computer. */
export const INSTALL_COMMAND = `curl -fsSL ${REPO_URL}/releases/latest/download/install.sh | sh`;

/** The same on Windows, in PowerShell (scripts/install.ps1). */
export const INSTALL_COMMAND_WINDOWS = `irm ${REPO_URL}/releases/latest/download/install.ps1 | iex`;

/** The install command for the computer this app runs on. */
export const installCommandHere = (): string => (/win/i.test(navigator.platform) ? INSTALL_COMMAND_WINDOWS : INSTALL_COMMAND);

/** The getting-started part of the project's page. */
export const QUICK_START_URL = `${REPO_URL}#quick-start`;
