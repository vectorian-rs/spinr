FROM --platform=linux/arm64 rust:1 AS build-env
WORKDIR /app
COPY . /app
RUN rustup target add aarch64-unknown-linux-gnu && \
    cargo build --target=aarch64-unknown-linux-gnu --bin spinr

FROM gcr.io/distroless/cc-debian13:latest-arm64 AS spinr
COPY --from=build-env /app/target/aarch64-unknown-linux-gnu/debug/spinr /
ENTRYPOINT ["/spinr"]
