FROM docker.stackable.tech/stackable/ubi8-rust-builder AS builder

FROM registry.access.redhat.com/ubi8/ubi-minimal AS operator

ARG VERSION
ARG RELEASE="1"

LABEL name="Stackable OPA Bundle Builder" \
  maintainer="info@stackable.de" \
  vendor="Stackable GmbH" \
  version="${VERSION}" \
  release="${RELEASE}" \
  summary="Build and serve Open Policy Agent bundles." \
  description="Build and serve Open Policy Agent bundles."

# Update image
RUN microdnf install -y yum \
  && yum -y update-minimal --security --sec-severity=Important --sec-severity=Critical \
  && yum clean all \
  && microdnf clean all

COPY LICENSE /licenses
COPY --from=builder /app/stackable-opa-bundle-builder /

RUN groupadd -g 1000 stackable && adduser -u 1000 -g stackable -c 'Stackable OPA Bundle Builder' stackable

USER stackable:stackable

ENTRYPOINT ["/stackable-opa-bundle-builder"]
CMD ["run"]
