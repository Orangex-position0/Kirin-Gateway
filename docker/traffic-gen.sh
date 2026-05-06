#!/bin/sh
# 等待 gateway 完全启动
sleep 5

echo "Traffic generator started, sending requests to gateway:8080 ..."

while true; do
  curl -s http://gateway:8080/api/users > /dev/null 2>&1
  curl -s http://gateway:8080/api/orders > /dev/null 2>&1
  curl -s http://gateway:8080/api/test > /dev/null 2>&1
  sleep 0.2
done