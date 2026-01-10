# Reaper - OCI Runtime Context

## Project Overview
Reaper is a minimal OCI-compatible container runtime written in Rust. It focuses on running host binaries using OCI container semantics without full containerization (no namespaces/cgroups initially).

## Architecture

### Core Components
- **reaper-runtime**: OCI-compatible CLI (`src/bin/reaper-runtime/main.rs`)
  - Commands: create, start, state, kill, delete
  - State persistence in `/run/reaper/<container-id>/`
  - JSON-based container state management

### Key Files
- `src/bin/reaper-runtime/main.rs` - Main runtime CLI and process execution
- `src/bin/reaper-runtime/state.rs` - Container state persistence
- `tests/integration_basic_binary.rs` - Basic binary execution tests
- `tests/integration_user_management.rs` - uid/gid management tests

## Implementation Details

### OCI Configuration Support
The runtime reads `config.json` from bundle directories with the following structure:
```json
{
  "process": {
    "args": ["/bin/echo", "hello"],
    "env": ["KEY=value"],
    "cwd": "/tmp",
    "user": {
      "uid": 1000,
      "gid": 1000,
      "additionalGids": [10, 20],
      "umask": 22
    }
  }
}
```

### User/Group Management (OCI Compliance)
- **OciUser struct**: uid, gid, additional_gids, umask
- **Privilege dropping sequence**: setgroups → setgid → setuid
- **Implementation**: Uses `Command::pre_exec` hook in child process
- **Security**: Processes inherit runtime UID if no user field specified
- **Root support**: uid=0 allowed per OCI spec (requires runtime to run as root)

### Process Execution
- Uses `std::process::Command` with `CommandExt::pre_exec`
- Stdio handling: stdin=null, stdout/stderr=inherit (passes through to parent)
- State transitions: created → running → stopped

### Dependencies
- `nix` crate with features: `["signal", "process", "user"]`
- Uses `nix::libc` for setgroups (not available in nix::unistd)
- Uses `nix::unistd::{setuid, setgid, Uid, Gid}` for user switching

## Testing Strategy

### Unit Tests (15 total)
- **State management** (10 tests in state.rs): ContainerState, save/load, paths
- **Config parsing** (5 tests in main.rs): OciUser deserialization, backward compatibility

### Integration Tests (8 total)
- **Basic execution** (3 tests): echo, shell scripts, error handling
- **User management** (5 tests): uid/gid switching, umask, additional groups, root user

### Running Tests
```bash
cargo test                    # All tests
cargo test --test integration_basic_binary
cargo test --test integration_user_management
```

## Important Constraints

### Current Scope
- ✅ Binary execution with OCI config syntax
- ✅ Process uid/gid/groups management
- ✅ State persistence and lifecycle
- ❌ Namespaces (not implemented - use host namespaces)
- ❌ Cgroups (not implemented)
- ❌ Kubernetes shim v2 protocol (future work, currently blocked)

### Security Considerations
1. Without user field in config.json, processes run as runtime's effective UID
2. Setting uid=0 requires runtime to have CAP_SETUID or run as root
3. Setting supplementary groups requires appropriate permissions

## Development Workflow

### Git Commits
- User wants control over commits - **do not auto-commit**
- Wait for explicit commit requests

### Code Style
- Rust stable toolchain (pinned via rust-toolchain.toml)
- Run `cargo fmt` and `cargo clippy` before committing
- CI checks: build, test, coverage (75% threshold), audit

### State Management
- State root: `$REAPER_RUNTIME_ROOT` or default `/run/reaper`
- Container directory: `<state_root>/<container_id>/`
- Files: `state.json`, `pid`

## Related Documentation
- OCI Runtime Spec: https://github.com/opencontainers/runtime-spec
- Focus on `config.json` process section and user fields
- Reference: process.user.{uid,gid,additionalGids,umask}

## Next Steps / TODO
- [ ] Add more comprehensive error handling
- [ ] Consider adding namespace support (future)
- [ ] Consider adding cgroup support (future)
- [ ] Implement shim v2 protocol for Kubernetes (blocked - needs research)
- [ ] Add more integration tests for edge cases
