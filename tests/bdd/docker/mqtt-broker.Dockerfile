FROM python:3.12-alpine

RUN mkdir -p /srv/releases /app
COPY tests/bdd/docker/mqtt-broker/server.py /app/server.py
EXPOSE 1883 8082
CMD ["python3", "-u", "/app/server.py"]
