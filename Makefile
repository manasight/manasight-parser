# manasight-parser Makefile
# Pre-commit contract for this repo.
#
# Targets:
#   make precommit          Full pre-commit checklist (fmt check, clippy, tests).
#   make precommit-trivial  Formatting floor only.
#   make coverage           Tarpaulin coverage report.
#   make fmt                Auto-format Rust code (helper, not a gate).

.PHONY: precommit precommit-trivial coverage fmt

precommit:
	cargo fmt --all -- --check
	cargo clippy --all-targets --all-features -- -D warnings
	cargo test --all-features

precommit-trivial:
	cargo fmt --all -- --check

coverage:
	cargo tarpaulin --all-features --ignore-tests

fmt:
	cargo fmt --all
