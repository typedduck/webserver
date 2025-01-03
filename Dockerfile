ARG IMAGE=scratch

FROM rust:1.83 AS builder
ARG FEATURES=default
ARG TARGET=x86_64-unknown-linux-musl
WORKDIR /usr/src/app
RUN if [ -z "$TARGET" ]; then \
        echo "TARGET is not set" && exit 1; \
    fi \
    && rustup target add $TARGET
COPY . .
RUN if [ -z "$FEATURES" ]; then \
        echo "FEATURES is not set" && exit 1; \
    fi \
    && cargo build --quiet --target $TARGET --features $FEATURES --release

FROM $IMAGE
ARG TARGET=x86_64-unknown-linux-musl
WORKDIR /app
VOLUME /app/public
COPY --from=builder /usr/src/app/target/$TARGET/release/webserver .

EXPOSE 8080
EXPOSE 8081
CMD ["./webserver"]
