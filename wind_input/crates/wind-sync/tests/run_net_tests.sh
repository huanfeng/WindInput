#!/usr/bin/env bash
# 起 MinIO 与 dufs（WebDAV）容器 → 跑 wind-sync 网络后端集成测试 → 删容器。
# 用法：run_net_tests.sh            （镜像可用 MINIO_IMAGE / DUFS_IMAGE 覆盖）
set -euo pipefail

MINIO_IMAGE=${MINIO_IMAGE:-minio/minio}
DUFS_IMAGE=${DUFS_IMAGE:-sigoden/dufs}
S3_PORT=${S3_PORT:-19000}
WD_PORT=${WD_PORT:-15000}
here=$(cd "$(dirname "$0")" && pwd)
names=(wind-sync-test-minio wind-sync-test-dufs)

cleanup() {
  docker rm -f "${names[@]}" >/dev/null 2>&1 || true
}
trap cleanup EXIT
cleanup

# 数据放 tmpfs，不在宿主留 root 属主的文件；MinIO 的 /data 下一级目录即桶
docker run -d --name wind-sync-test-minio -p "127.0.0.1:$S3_PORT:9000" \
  -e MINIO_ROOT_USER=minioadmin -e MINIO_ROOT_PASSWORD=minioadmin \
  --tmpfs /data --entrypoint sh "$MINIO_IMAGE" \
  -c 'mkdir -p /data/wind-test && exec minio server /data' >/dev/null
docker run -d --name wind-sync-test-dufs -p "127.0.0.1:$WD_PORT:5000" \
  --tmpfs /data "$DUFS_IMAGE" -A -a 'wind:wind-pass@/:rw' /data >/dev/null

for i in $(seq 1 60); do
  curl -fs "http://127.0.0.1:$S3_PORT/minio/health/live" >/dev/null 2>&1 && break
  sleep 1
  [ "$i" = 60 ] && { echo "MinIO 未就绪" >&2; docker logs wind-sync-test-minio >&2; exit 1; }
done
for i in $(seq 1 30); do
  curl -s -o /dev/null "http://127.0.0.1:$WD_PORT/" && break
  sleep 1
done

export WD_URL="http://127.0.0.1:$WD_PORT" WD_USER=wind WD_PASS=wind-pass
export S3_ENDPOINT="http://127.0.0.1:$S3_PORT" S3_BUCKET=wind-test S3_AK=minioadmin S3_SK=minioadmin
# 容器在本机回环上，别走环境代理
export NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost

cd "$here/.."
cargo test --features net --test net_backends --no-fail-fast -- --ignored --test-threads=4 "$@"
