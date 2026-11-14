## What and why

<!-- What does this change, and why? Link the issue. -->

## How it was tested

<!-- Commands you ran: tools/ci/local.sh --quick (or --changed / --full), corpus tests, fuzz, manual checks. -->

## Checklist

- [ ] `tools/ci/local.sh` passes for the areas I touched (there are no automatic GitHub checks)
- [ ] New or changed behaviour has a test; a crash fix has a regression file
- [ ] Documentation is updated (`docs/`, rustdoc, CHANGELOG for user-visible changes)
- [ ] Camera database values are backed by sample files (CC0) and `flashraw camera-db validate` passes
- [ ] I did not copy code or data from projects with an incompatible licence, and I license this under MIT OR Apache-2.0
