FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p popsub \
 && mkdir -p /out/data \
 && cp target/release/popsub /out/popsub

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /out/popsub /usr/local/bin/popsub
COPY --from=build --chown=65532:65532 /out/data /data
WORKDIR /app
ENV SERVER_HOST=0.0.0.0 \
    SERVER_PORT=8080 \
    STORAGE_PATH=/data \
    SERVER__HEALTH_ADDRESS=0.0.0.0:9090
VOLUME ["/data"]
EXPOSE 8080 9090
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/popsub"]
CMD ["server"]
