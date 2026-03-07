# Contributing to manasight-parser

Contributions are welcome! Whether it's a bug report, feature suggestion, or pull request, we appreciate your help.

## Reporting Bugs

Open a [GitHub issue](https://github.com/manasight/manasight-parser/issues) with:

- A clear description of the problem
- Steps to reproduce (if applicable)
- Relevant log output or error messages

## Submitting Pull Requests

1. Fork the repository and create a feature branch
2. Make your changes
3. Ensure all checks pass (see below)
4. Open a pull request against `main`

## Development Setup

```bash
git clone https://github.com/manasight/manasight-parser.git
cd manasight-parser

# Run tests
cargo test --all-features

# Lint
cargo clippy --all-targets --all-features -- -D warnings

# Format
cargo fmt --all
```

## Code of Conduct

This project follows the [Contributor Covenant Code of Conduct](CODE_OF_CONDUCT.md).
