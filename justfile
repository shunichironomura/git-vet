lint:
    RUSTFLAGS="-Dwarnings" cargo clippy --all-targets --all-features
    RUSTFLAGS="-Dwarnings" cargo clippy --all-targets --no-default-features
    cargo fmt --check
    RUSTDOCFLAGS="-Dwarnings" cargo doc --no-deps
    cargo check

test:
    cargo test

coverage:
    cargo llvm-cov --html
    @echo "Coverage report generated at target/llvm-cov/html/index.html"

coverage-open:
    cargo llvm-cov --html --open

coverage-lcov:
    cargo llvm-cov --lcov --output-path lcov.info

coverage-clean:
    cargo llvm-cov clean
