# Synthetic audio fixture only; no host devices, profiles or networking at runtime.
FROM ubuntu:26.04
RUN apt-get update && apt-get install -y --no-install-recommends pulseaudio libpulse0 \
    && rm -rf /var/lib/apt/lists/*
COPY run.sh /run-fixture.sh
USER 1000:1000
ENTRYPOINT ["/bin/bash", "/run-fixture.sh"]
