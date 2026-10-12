ARG AIOS_BASE_IMAGE=paios/aios:latest-amd64
FROM ${AIOS_BASE_IMAGE}
COPY opendan/ /opt/buckyos/bin/opendan/
COPY buckyos-agent/ /opt/buckyos/bin/opendan/buckyos-agent/
COPY sdk/ /opt/buckyos/bin/opendan/buckyos-agent/vendor/buckyos-websdk/
COPY entrypoint.sh /opt/buckyos/entrypoint.sh
COPY aicc-tool /opt/buckyos/bin/opendan/aicc-tool
RUN chmod 755 /opt/buckyos/bin/opendan/aicc-tool /opt/buckyos/entrypoint.sh /opt/buckyos/bin/opendan/opendan \
    /opt/buckyos/bin/opendan/agent_tool /opt/buckyos/bin/opendan/xagent
