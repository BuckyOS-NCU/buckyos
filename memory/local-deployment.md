此部署宿主机的 500 GB ext4 数据盘 UUID 为 `a08bd4c5-4244-4ed6-a5cc-cf134af698e2`，挂载到 `/mnt/data`，通过 `/etc/fstab` 按 UUID 自动挂载。盘上已有 `app/`、`docker/` 目录，使用新子目录存放构建数据或备份，避免覆盖既有内容。

宿主机 Docker 数据目录仍为 `/var/lib/docker`，位于 50 GB 根分区。大型临时数据和备份应放到 `/mnt/data`；数据盘挂载不会自动改变 Docker 数据目录。
