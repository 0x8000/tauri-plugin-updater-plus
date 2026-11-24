FROM python:3.12-alpine

RUN mkdir -p /srv/releases /app
COPY tests/bdd/docker/cdn-gateway/server.py /app/server.py
EXPOSE 8080 8081
CMD ["python3", "-u", "/app/server.py"]
