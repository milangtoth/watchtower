FROM python:3.12-slim
ENV PORT=8080 HOST=0.0.0.0 PYTHONUNBUFFERED=1
WORKDIR /app
COPY requirements-discovery.txt ./
RUN pip install --no-cache-dir -r requirements-discovery.txt && playwright install --with-deps chromium
COPY . .
EXPOSE 8080
VOLUME ["/data"]
CMD ["python","atlas.py","--no-browser","--data","/data"]

