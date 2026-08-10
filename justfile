set shell := ["pwsh", "-NoLogo", "-NoProfile", "-Command"]

default: check

fmt:
    vx cargo fmt --all -- --check

lint:
    vx cargo clippy --all-targets --all-features --locked -- -D warnings

test:
    vx cargo test --all-features --locked

check: fmt lint test

ci: check
    vx cargo build --release --locked

build-release:
    vx cargo build --release --locked

build-target target:
    vx cargo build --release --locked --target {{target}}

package target version: (build-target target)
    pwsh -NoLogo -NoProfile -File scripts/package-release.ps1 -Target "{{target}}" -Version "{{version}}" -OutputDir dist

release-please-dry-run:
    vx release-please release-pr --repo-url=loonghao/bazaardb-cli --dry-run
