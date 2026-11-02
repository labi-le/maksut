# syntax=docker/dockerfile:1
FROM rust:1.98.1-bookworm AS build
WORKDIR /app
ENV CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse \
    CARGO_INCREMENTAL=0
COPY Cargo.toml Cargo.lock ./
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    mkdir src \
    && printf 'fn main() {}\n' > src/main.rs \
    && cargo build --release --locked --bin maksut \
    && rm -rf src target/release/maksut target/release/deps/maksut* target/release/.fingerprint/maksut-*
COPY docs/communication-guide.md ./docs/communication-guide.md
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    touch src/main.rs \
    && cargo build --release --locked --bin maksut \
    && rm -rf target/release/deps target/release/build target/release/.fingerprint

FROM node:22-bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates ffmpeg python3 python3-venv \
    && rm -rf /var/lib/apt/lists/* \
    && python3 -m venv /opt/yt-dlp \
    && /opt/yt-dlp/bin/pip install --no-cache-dir 'yt-dlp[default,curl-cffi]==2026.7.4' \
    && mkdir -p /data /tmp/maksut \
    && chown 1000:1000 /data /tmp/maksut
# yt-dlp does not enable Node by default; generic extraction is unsafe for embedded URLs.
RUN printf '%s\n' '--js-runtimes node' '--ies default,-generic' > /etc/yt-dlp.conf
ENV PATH="/opt/yt-dlp/bin:${PATH}" \
    HOME=/tmp/maksut \
    DATABASE_URL=sqlite:///data/maksut.db \
    DOWNLOAD_DIR=/tmp/maksut \
    YTDLP_BIN=yt-dlp \
    PYTHON_BIN=/opt/yt-dlp/bin/python \
    TWITTER_HELPER=/opt/maksut/twitter_images.py \
    STICKER_SETS_FILE=/opt/maksut/sticker_sets.txt \
    STICKER_DESCRIPTIONS_FILE=/opt/maksut/sticker_descriptions.json
COPY --from=build /app/target/release/maksut /usr/local/bin/maksut
COPY tools/twitter_images.py /opt/maksut/twitter_images.py
COPY sticker_sets.txt /opt/maksut/sticker_sets.txt
COPY sticker_descriptions.json /opt/maksut/sticker_descriptions.json
USER 1000:1000
WORKDIR /data
CMD ["/usr/local/bin/maksut"]
