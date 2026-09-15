#!/usr/bin/env bash

# story_daemon_http_port — read daemon status on stdin and print its local
# HTTP port. Refuse stopped or malformed status with no output and exit 1.
story_daemon_http_port() {
  # Version annotations can add words. Only the first line describes the
  # listener; later diagnostics can contain unrelated URLs and ports.
  awk '
    NR == 1 && $1 == "storyhook" && $2 == "daemon" {
      for (i = 4; i + 2 <= NF; i++) {
        if ($i != "running" || $(i + 1) != "at") continue
        url = $(i + 2)
        if (url !~ /^http:\/\/127[.]0[.]0[.]1:[0-9]+\/?$/) exit 1
        sub(/^http:\/\/127[.]0[.]0[.]1:/, "", url)
        sub(/\/$/, "", url)
        if (url + 0 < 1 || url + 0 > 65535) exit 1
        printf "%d\n", url
        found = 1
        exit
      }
    }
    END { if (!found) exit 1 }
  '
}
