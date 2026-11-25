# X Roadmap: sample-app-v1

## Existing work accepted by the author

- [x] SETUP: Repository scaffolding accepted

## Independent proposals

- [ ] UI: Keyboard controls
  Goal: Add explicit keyboard shortcuts to the task list
  Acceptance: Keyboard interaction tests cover focus, selection and cancellation
  Depends: SETUP
- [ ] API: Worker status
  Goal: Return observed worker state from the status endpoint
  Acceptance: Status tests cover running, failed and stopped jobs without invented progress
  Depends: SETUP
- [ ] DOCS: Contributor guide
  Goal: Document how to run the existing checks and create an isolated local change
  Acceptance: Every documented command matches repository configuration
  Depends: SETUP

## Later dependent work

- [ ] CONNECT: Connect the task list to worker status
  Goal: Show the observed endpoint state in the task list
  Acceptance: An integration test covers a worker stopping while the task list is open
  Depends: UI, API
