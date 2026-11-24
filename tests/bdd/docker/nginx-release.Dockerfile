FROM nginx:1.27-alpine

RUN mkdir -p /srv/releases /var/log/bdd
COPY tests/bdd/docker/nginx-release/nginx.conf /etc/nginx/nginx.conf
EXPOSE 80
