# TODO

List of tasks to do, not ordered in any specific way.

- [x] Improve testing so we run the full set of tests on MacOS using Linux VMs to mimic what CI does.
- [x] Create an installation script and move the relevant code off `scripts/run-integration-tests.sh`
- [x] Evaluate creating a CRD similar to pods/deployments/daemonsets to avoid code that is built for containers (images)
- [x] Ensure volumes are mounted and visible (at least `hostPath()`)
- [x] Ensure uid and gid changes are validated in the integration tests
- [x] Filter out sensitive host files when mounting the overlay
- [ ] Add testing on a real kubernetes cluster (look at GKE, EKS, something free)
- [x] Add versioning, packaging and release model and processes
- [x] Run a security analysis of the code
- [x] Add examples that use jobs, deployments and daemonsets
- [x] Add complex example (idea openldap server + sssd + something that uses users)
- [x] Add quick-start guide with a playground kind cluster for doing fast testing
- [x] Evaluate if it would make sense to isolate overlays by namespace
- [x] Manage DNS settings, currently relying on host DNS instead of k8s DNS settings.
- [x] Add certain configuration parameters as annotations, so users can influence how Reaper works (DNS, overlay name and mount point, etc.). But ensuring adminsistrator parameters cannot be overriden.
- [ ] Introduce more complex examples, answer this question: can we have a sssd containerd pod expose its socks file so a sample reaper pod can utilize it?
- [ ] Produce RPM an DEB packages compatible with major distributions (SUSE, RHEL, Debian, Ubuntu). This will help with installation and deployment.
- [x] Evaluate if Reaper can be configured using a Kubernetes ConfigMap instead of relying on a node-level config file. (Implemented via `reaper-agent` DaemonSet — PR #27)
- [x] reaper-agent Phase 2: Overlay GC — reconcile overlay namespaces against Kubernetes API, delete overlays for namespaces that no longer exist
- [ ] reaper-agent Phase 2: Binary self-update — watch ConfigMap version field, download and replace shim/runtime binaries
- [ ] reaper-agent Phase 2: Node condition reporting — patch Node object with `ReaperReady` condition
- [x] reaper-agent Phase 2: Mount namespace cleanup — detect and unmount stale `/run/reaper/ns/*` bind-mounts
- [ ] Fix known bugs documented in [docs/BUGS.md](BUGS.md)