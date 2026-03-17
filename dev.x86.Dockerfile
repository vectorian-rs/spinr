FROM --platform=linux/amd64 rust:1 AS build-env
WORKDIR /app
COPY . /app
RUN rustup target add x86_64-unknown-linux-gnu && \
    cargo build --target=x86_64-unknown-linux-gnu --bin spinr

FROM gcr.io/distroless/cc-debian13:latest-amd64 AS spinr
COPY --from=build-env /app/target/x86_64-unknown-linux-gnu/debug/spinr /
ENTRYPOINT ["/spinr"]
