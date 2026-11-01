#!/usr/bin/env bash
# Deletes the hosted runner's preinstalled SDKs that this benchmark never uses. A stage1 build and a cold grok-build target do not fit beside them.
set -euo pipefail
df -h /
sudo rm -rf /usr/share/dotnet /usr/local/lib/android /opt/ghc /usr/local/.ghcup /opt/hostedtoolcache/CodeQL /usr/local/share/boost /usr/share/swift
sudo docker image prune -af > /dev/null
df -h /
