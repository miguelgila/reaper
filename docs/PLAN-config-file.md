# Plan: Cross-OS Configuration File

**Goal**: Replace the Debian-specific `/etc/default/containerd` env-file approach with a Reaper-owned config file at `/etc/reaper/reaper.conf` that works on any Linux distribution.

---

## Problem

Configuration is currently injected via environment variables into containerd's process using:
1. `/etc/default/containerd` — a Debian/Ubuntu convention
2. A systemd drop-in (`EnvironmentFile=-/etc/default/containerd`)

This doesn't work on RHEL/Fedora/SUSE/Arch/Alpine where `/etc/default/` doesn't exist or isn't the convention. The systemd drop-in is also unnecessary coupling — Reaper can read its own config.

## Current REAPER_* Environment Variables

Both binaries read configuration purely from `std::env::var()`:

| Variable | Binary | Default | Purpose |
|----------|--------|---------|---------|
| `REAPER_RUNTIME_ROOT` | both | `/run/reaper` | State directory root |
| `REAPER_RUNTIME_PATH` | shim | auto-detect | Path to runtime binary |
| `REAPER_RUNTIME_LOG` | runtime | (none) | Runtime log file path |
| `REAPER_SHIM_LOG` | shim | (none) | Shim log file path |
| `REAPER_OVERLAY_BASE` | runtime | `/run/reaper/overlay` | Overlay base directory |
| `REAPER_OVERLAY_NS` | runtime | `/run/reaper/shared-mnt-ns` | Namespace bind-mount path |
| `REAPER_OVERLAY_LOCK` | runtime | `/run/reaper/overlay.lock` | Overlay lock file |
| `REAPER_DNS_MODE` | runtime | `host` | DNS mode: host, kubernetes, k8s |
| `REAPER_FILTER_ENABLED` | runtime | `true` | Enable sensitive file filtering |
| `REAPER_FILTER_MODE` | runtime | `append` | Filter mode: append or replace |
| `REAPER_FILTER_PATHS` | runtime | (none) | Colon-separated extra filter paths |
| `REAPER_FILTER_ALLOWLIST` | runtime | (none) | Colon-separated allowlist paths |
| `REAPER_FILTER_DIR` | runtime | `/run/reaper/overlay-filters` | Placeholder file directory |

## Solution

### Config file: `/etc/reaper/reaper.conf`

Simple `KEY=VALUE` format (same as existing env vars):

```
# Reaper runtime configuration
# Lines starting with # are comments. Blank lines are ignored.
# Values here are overridden by environment variables of the same name.

REAPER_DNS_MODE=kubernetes
REAPER_RUNTIME_LOG=/run/reaper/runtime.log
# REAPER_OVERLAY_BASE=/run/reaper/overlay
# REAPER_FILTER_ENABLED=true
```

### Load order (last wins)

1. Built-in defaults (in Rust code, unchanged)
2. Config file values (set as env vars if not already set)
3. Environment variables (override everything)

This means env vars always win — backward compatible with anyone already using systemd env injection.

### Config file search

1. `REAPER_CONFIG` env var (explicit override)
2. `/etc/reaper/reaper.conf` (standard location)
3. No file found → silently continue with env vars / defaults

---

## Implementation

### Phase 1: Shared config loader (`src/config.rs`)

New file shared by both binaries via `#[path]` attribute (no lib crate needed).

```rust
// ~40 lines: read file, parse KEY=VALUE, set env if not already set
pub fn load_config() {
    let path = std::env::var("REAPER_CONFIG")
        .unwrap_or_else(|_| "/etc/reaper/reaper.conf".to_string());
    // read file, skip comments/blanks, split on first '=', set_var if not present
}
```

Key design:
- Uses `std::env::set_var` only if the key is NOT already set (env wins)
- No dependencies — just `std::fs` and `std::env`
- Silently skips if file doesn't exist (not an error)
- Logs a warning for malformed lines (if tracing is initialized — but config loads before tracing, so just silently skip malformed lines)

### Phase 2: Wire into both binaries

**`src/bin/reaper-runtime/main.rs`**: Call `config::load_config()` as the very first line of `main()`, before tracing setup (since `REAPER_RUNTIME_LOG` comes from config).

**`src/bin/containerd-shim-reaper-v2/main.rs`**: Call `config::load_config()` as the very first line of `main()`, before version check and tracing setup.

### Phase 3: Update Ansible playbook

Replace the three tasks (write to `/etc/default/containerd`, write systemd drop-in, daemon-reload) with:

1. Create `/etc/reaper/` directory
2. Write `/etc/reaper/reaper.conf` with template
3. Remove the systemd `EnvironmentFile` drop-in (no longer needed)

### Phase 4: Update documentation

- Update `CLAUDE.md` to mention `/etc/reaper/reaper.conf`
- Update `deploy/kubernetes/README.md` if it references the old approach

---

## File Changes

| File | Action | Description |
|------|--------|-------------|
| `src/config.rs` | **Create** | Shared config file loader (~40 lines) |
| `src/bin/reaper-runtime/main.rs` | **Edit** | Add `mod config; config::load_config();` at top of main() |
| `src/bin/containerd-shim-reaper-v2/main.rs` | **Edit** | Add `mod config; config::load_config();` at top of main() |
| `deploy/ansible/install-reaper.yml` | **Edit** | Write `/etc/reaper/reaper.conf` instead of `/etc/default/containerd` |
| `CLAUDE.md` | **Edit** | Document config file location |

---

## What We're NOT Doing

- **TOML/YAML config**: Adds a parsing dependency for flat key-value data
- **containerd runtime options**: Requires custom protobuf, containerd-specific
- **Distro detection**: Fragile, same result as just owning our config path
- **Removing env var support**: Env vars still override config file (backward compat)
