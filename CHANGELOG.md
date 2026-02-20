# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.5] - 2026-02-20

### Features

- Add --release flag to playground for pre-built binaries  (7a0af59)

### Documentation

- Update RELEASING.md with GitHub App setup and design decisions (1e26bec)
## [0.2.4] - 2026-02-19

### Bug Fixes

- Trigger release workflow via workflow_dispatch (c8cc0ce)
- Push tag separately to trigger release workflow (e7636d7)
- Use GitHub App token for release workflows (ea4efd9)
## [0.2.1] - 2026-02-18

### Features

- Add git-cliff changelog generation to release pipeline  (243599c)
## [0.2.0] - 2026-02-18

### Features

- Versioning, release pipeline, and install-from-release  (29262c2)
- Add playground setup script and fix PTY exit handling  (e994dda)
- Add volume mount support for Kubernetes volumes  (e6b557f)
- Implement UID/GID switching with privilege dropping  (07cf0ae)
- Add sensitive file filtering to overlay filesystem  (49035f0)
- Unify deployment with Ansible for both Kind and production (34c2887)
- Implement Phase 2 - Ansible playbooks for production deployment (16fdd72)
- Implement core install-reaper.sh script (44d368b)
- Add Makefile for CI-parity development workflow (3f8a0df)
- Add commit id and timestamp to integration test output (30be322)
- Add integration test to validate defunct procs (5dc8941)
- Add PTY/terminal support for interactive containers + exec implementation\n\n- Add terminal flag to ContainerState and pass --terminal from shim to runtime\n- Implement PTY allocation in do_start() when terminal=true (kubectl run -it)\n- Relay stdin FIFO → PTY master and PTY master → stdout FIFO for interactive I/O\n- Add exec lifecycle: exec state management, exec_with_pty, exec_without_pty\n- Add exec integration tests and EXEC_IMPLEMENTATION_PLAN.md\n- Update kind-integration.sh and Cargo.toml\n- Remove CLAUDE.MD files (8847a4e)
- Add shared mount namespace with overlayfs for host protection (4ff7a89)
- Implement container stdout/stderr capture for kubectl logs integration (1bd4b32)
- Complete Milestone 5 - Kubernetes Integration (78b495b)
- Start Milestone 4 - Advanced Features (7614708)
- Implement Milestone 3 - Direct Command Execution (edde35e)
- Add unit tests for state module, refactor runtime CLI, build musl binary for minikube (abc9396)
- **runtime**: Scaffold minimal OCI-like runtime CLI for containerd (create/start/state/kill/delete) with basic state store (69f25a7)

### Bug Fixes

- ECHILD race, 14 new integration tests, test script refactoring  (d8d19fe)
- Pod stuck in Terminating due to missing setsid() in non-terminal mode  (183c246)
- Correct GitHub username in README badges  (d417f85)
- Use CI-safe binary path to avoid permission issues in GitHub Actions (017aa93)
- Ansible installer compatibility and script syntax errors (c002cde)
- Override ANSIBLE_STDOUT_CALLBACK environment variable (4b9f7e3)
- Add ansible.cfg for cross-version compatibility (2602594)
- Build static musl binaries for Kind and show Ansible errors (b63de5a)
- Build binaries before Ansible installer in integration tests (06eda60)
- Skip overlay integration tests when namespace support unavailable (dc59cff)
- Serialize overlay config tests with mutex to prevent race conditions (7f8bfa3)
- Ensure proper test isolation for overlay config tests (56f654c)
- Show detailed diagnostics when integration tests fail (e1fe093)
- Skip overlay in unit tests and fix clippy warnings (74c3cc9)
- Use temp directories for overlay paths in integration tests (6aedb5b)
- Increase PID polling timeout on Linux and add overlay debug logging (958f6e9)
- Signal ExitSignal in shutdown() so shim processes exit (d4e8c74)
- Reap zombie monitoring daemons in shim (afb4d2e)
- Use c_ulong for TIOCSCTTY ioctl request type on all platforms (e161686)
- Make DNS check pod always succeed in kind-integration.sh (e3a601d)
- Prevent zombie processes by reaping daemon in do_start() (8a3944b)
- Increase workload wait() timeout from 60s to 1h\n\nThe 60-second polling timeout in the shim's wait() was causing\ninteractive containers (kubectl run -it) to be killed after ~1 minute.\nIncreased to 1 hour to match the exec wait timeout. (28c13c7)
- Enforce overlay isolation - remove /tmp bind-mount and make overlay mandatory (d8bd982)
- Resolve libc dependency and unused variable in overlay tests (e390f2b)
- Use as_raw_fd() for nix::unistd::read which still expects RawFd (c9b70b4)
- Update overlay module for nix 0.28 API compatibility (1eaf46b)
- Update all packages (d13b8e9)
- Update bytes crate to 1.11.1 for RUSTSEC-2026-0007 (e10d615)
- Sandbox wait() blocking, PID race condition, and stale pod cleanup (af8f4cb)
- Return STOPPED status for sandbox containers to enable pod cleanup (2202f08)
- Add 5s timeout to kill() method to prevent pod cleanup hangs (b159d2b)
- Add 30s timeout to wait() polling loop to prevent pod cleanup hangs (e79796a)
- Simplify grep pattern to be more robust (f74ef7e)
- Use precise line deletion for reaper-v2 removal (bb06e75)
- Correct grep pattern for runc section matching (2b018fe)
- Make reaper-v2 deletion more precise to preserve runc section (228a88f)
- Remove duplicate reaper-v2 sections to prevent TOML parse errors (3c150e2)
- Use minimal containerd config for kind to resolve control plane instability (2bc7e7c)
- Resolve zombie process accumulation in reaper shim (d43e94f)
- Ensure static musl binaries for kind to avoid glibc version mismatch (6e164b9)
- Add retry logic and enhanced logging to kind integration tests (b39f56a)
- Add service account wait and project documentation for CI (cd1bbb7)
- Build Linux binaries for kind cluster testing (d3cebb4)
- Update kind integration setup for proper reaper-v2 configuration (5a18ccf)
- Update test_config_with_root_user to reflect disabled user switching (3dbc29d)
- Integration workflow - wait for API server and handle validation errors (0682643)
- Integration workflow - fix containerd config directory and improve error handling (7e14998)
- Fix clippy (a233ba4)
- Resolve unused CommandStatus::Stopped warning (fb2773c)
- Remove src/ from gitignore, add src/main.rs, remove Windows from CI (c6573fe)

### Refactoring

- Use Makefile for building binaries in integration tests (b6f5a03)
- Complete migration to unified Ansible installer (11d24ac)
- Use install-reaper.sh in integration test suite (041d7b3)
- Consolidate integration tests and scripts into common locations (e3721d2)
- Replace kind-integration.sh with structured test harness (7eb03cf)
- Improve DNS validation in kind-integration.sh (95dbb67)
- Preserve kind-generated containerd config and extend with sed (79dbf3a)
- Use sed-based configuration for kind containerd setup (f1ad843)
- Consolidate coverage into test workflow to eliminate redundant builds (4025607)
- Optimize coverage workflow with build job and cache sharing (c5547fb)
- Implement proper OCI shim architecture invoking reaper-runtime (a3ab6a3)

### Documentation

- Reorganize documentation for better user experience (8e55744)
- Document CRD evaluation done (758335a)
- Add CLAUDE.md with CI/CD and integration testing context (49da7ab)
- Update progress tracker to reflect Ansible approach (e8098fc)
- Revise Phase 2 to use Ansible instead of DaemonSet (4b42d65)
- Update documentation for install-reaper.sh (c260f55)
- Add installation script implementation plan (b30bd03)
- Update documentation to reflect recent changes (exec, PTY, overlay improvements) (3ad5580)
- Mark Milestone 5 as completed and update next steps (221d845)
- Update SHIMV2_DESIGN.md to reflect Milestone 3 completion (01edc73)
- Update SHIMV2_DESIGN.md with current implementation status (6a68f5a)
- Clarify OCI allows root processes (uid=0) (fbdb1ec)
- Document uid/gid requirements for OCI compatibility (6099e29)
- Clarify stdout/stderr handling (7f4cc19)
- Document integration tests for core binary execution (ca6cf88)
- Clarify reaper-runtime usage and Kubernetes integration status (25e9a1a)
- **runtime**: Document reaper-runtime CLI, containerd config, and Kubernetes RuntimeClass example (7a80005)
- Add CI badges and clarify Docker usage; fix coverage container networking (6ada821)

### Testing

- Improve unit test coverage and lower tarpaulin threshold (b2a634d)
- Parameterize ensure_etc_files_in_namespace and add unit tests (d9ac1d6)
- Add unit tests for overlay helper functions to improve coverage (f481abe)
- Add end-to-end Kubernetes integration test script (1f26135)
- Add integration tests for containerd shim v2 (8e0ca2d)
- **integration**: Add core binary execution tests (2099b07)
- **integration**: Add minikube and kind integration scripts and CI workflow for runtime validation (95375c3)

### CI/CD

- Add comprehensive log capture and artifact upload to integration workflow (3d5b266)
- Remove doc tests (binary-only crate); update README CI section (c3d8afc)
- **coverage**: Enforce 75% minimum and fail on Codecov upload errors (c1c0913)
- Add Codecov token to coverage workflow (75d5b2a)
- Run cargo-audit in build workflow; document audit in README (1a96f59)

### Reverts

- Sandbox status change breaks container initialization (e55b3aa)

### Miscellaneous

- Declutter repo root directory structure  (8ced09d)
- Add cobertura.xml to gitignore (28608b5)
- Add comprehensive logging and debugging output to integration test (b5d79e6)
- Increase wait() timeout to 60s and slow kubectl polling to 5s intervals (8f78d6f)
- Update GitHub Actions artifact actions from v3 to v4 (12a6e5a)
- Improve doc (a3afda1)
- Remove target-linux from git tracking (6d4789e)
- Remove accidental extra state modules (f4ca852)

