
# build musl static release
build-musl-static:
    cargo zigbuild --release --target=x86_64-unknown-linux-musl
