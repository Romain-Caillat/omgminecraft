# Construit le binaire statique (front Vue embarqué). Il n'y a pas d'image à
# déployer : on extrait /omgminecraft et on le lance via systemd sur la machine
# qui héberge Minecraft (voir README).
#
#   docker build -t omgminecraft-build .
#   docker run --rm omgminecraft-build > omgminecraft && chmod +x omgminecraft

FROM node:24-alpine AS web
WORKDIR /w
COPY web/package.json web/package-lock.json* ./
RUN if [ -f package-lock.json ]; then npm ci; else npm install; fi
COPY web/ .
RUN npm run build

FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /b
COPY Cargo.toml Cargo.lock* ./
COPY src/ src/
COPY --from=web /w/dist web/dist
RUN cargo test --release --locked --target x86_64-unknown-linux-musl \
 && cargo build --release --locked --target x86_64-unknown-linux-musl \
 && cp target/x86_64-unknown-linux-musl/release/omgminecraft /omgminecraft

FROM alpine:3
COPY --from=build /omgminecraft /omgminecraft
COPY --from=build /b/Cargo.lock /Cargo.lock
COPY --from=web /w/package-lock.json /package-lock.json
CMD ["cat", "/omgminecraft"]
