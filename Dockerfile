# Builds the Gagent command line tool and the Windows desktop app without installing Rust.
#
#   docker build -t gagent-build .
#   docker run --rm -v "$PWD/out:/out" gagent-build
#
# The output folder then contains gagent.exe (Windows desktop app) and the Linux binaries.
# NOTE: this build file has not been run yet; see TODO.md.
FROM rust:1-bookworm
RUN apt-get update \
 && apt-get install -y --no-install-recommends gcc-mingw-w64-x86-64 \
 && rm -rf /var/lib/apt/lists/* \
 && rustup target add x86_64-pc-windows-gnu
WORKDIR /src
COPY . .
CMD ["sh", "-c", "cargo build --release --target x86_64-pc-windows-gnu && cargo build --release && mkdir -p /out && cp target/x86_64-pc-windows-gnu/release/gagent.exe target/x86_64-pc-windows-gnu/release/gagent-cli.exe target/release/gagent target/release/gagent-cli /out/"]
