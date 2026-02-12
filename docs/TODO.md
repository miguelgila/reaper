# TODO

List of tasks to do, not ordered in any specific way.

[x] Improve testing so we run the full set of tests on MacOS using Linux VMs to mimic what CI does.
[x] Create an installation script and move the relevant code off `scripts/run-integration-tests.sh`
[ ] Evaluate creating a CRD similar to pods/deployments/daemonsets to avoid code that is built for containers (images)
[ ] Ensure volumes are mounted and visible (at least `hostPath()`)
[ ] Ensure uid and gid changes are validated in the integration tests
[ ] Filter out sensitive host files when mounting the overlay
[ ] Add testing on a real kubernetes cluster (look at GKE, EKE, something free)