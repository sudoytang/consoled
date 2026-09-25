FROM debian:12-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        login \
        passwd \
        adduser \
        openssl \
        python3 \
        procps \
        libpam-modules \
        libpam-runtime \
        ca-certificates \
        bash \
    && rm -rf /var/lib/apt/lists/*

COPY target/release/consoled /usr/local/sbin/consoled
COPY integration/run_tests.py /tests/run_tests.py
COPY integration/entrypoint.sh /entrypoint.sh
RUN chmod 755 /usr/local/sbin/consoled /tests/run_tests.py /entrypoint.sh

ENTRYPOINT ["/entrypoint.sh"]
