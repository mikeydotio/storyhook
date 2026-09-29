#!/usr/bin/env python3
"""Run SH-844's real system-attributes regression without editing host files.

This explicit, network-capable check is separate from the offline gate. It
builds upstream Git with a private system attributes path, then runs the same
Rust production-path regression as the ordinary attribute-source matrix.
Use --archive to run with an already downloaded, checksum-pinned archive.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import urllib.request

VERSION = "2.54.0"
SHA256 = "f689162364c10de79ef89aa8dbf48731eb057e34edbbd20aca510ce0154681a3"
ROOT = Path(__file__).resolve().parents[1]


def main():
    """Build the pinned fixture and require a real, non-skipped regression."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="sh844-system-", dir="/tmp") as scratch:
        root = Path(scratch)
        archive = root / "git.tar.xz"
        if args.archive:
            archive.write_bytes(args.archive.read_bytes())
        else:
            url = f"https://www.kernel.org/pub/software/scm/git/git-{VERSION}.tar.xz"
            with urllib.request.urlopen(url, timeout=60) as response:
                archive.write_bytes(response.read())
        if hashlib.sha256(archive.read_bytes()).hexdigest() != SHA256:
            raise RuntimeError("upstream Git archive checksum mismatch")
        with tarfile.open(archive) as source:
            source.extractall(root, filter="data")
        source = root / f"git-{VERSION}"
        prefix = root / "installation"
        attributes = prefix / "etc/gitattributes"
        attributes.parent.mkdir(parents=True)
        subprocess.run([
            "make", "-j", "4", "git", "NO_CURL=YesPlease", "NO_GETTEXT=YesPlease",
            "NO_TCLTK=YesPlease", "NO_OPENSSL=YesPlease", "NO_PERL=YesPlease",
            "CFLAGS=-O2 -fno-common -Werror", f"prefix={prefix}",
            f"sysconfdir={attributes.parent}",
        ], cwd=source, check=True)
        bindir = prefix / "bin"
        bindir.mkdir()
        (bindir / "git").symlink_to(source / "git")
        built = subprocess.run([
            "cargo", "test", "--test", "merge_attributes", "--no-run", "--message-format=json",
        ], cwd=ROOT, check=True, stdout=subprocess.PIPE, text=True)
        executables = [item["executable"] for line in built.stdout.splitlines()
                       if (item := json.loads(line)).get("reason") == "compiler-artifact"
                       and item.get("target", {}).get("name") == "merge_attributes"
                       and item.get("executable")]
        if len(executables) != 1:
            raise RuntimeError(f"expected one merge_attributes executable: {executables}")
        env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        env.update(PATH=str(bindir) + os.pathsep + env["PATH"], HOME=str(root / "home"),
                   XDG_CONFIG_HOME=str(root / "xdg"), TMPDIR="/tmp",
                   GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1",
                   SH844_ATTRIBUTE_SOURCE="system", SH844_SYSTEM_ATTRIBUTES=str(attributes))
        Path(env["HOME"]).mkdir()
        Path(env["XDG_CONFIG_HOME"]).mkdir()
        subprocess.run([executables[0], "--exact", "attribute_child", "--nocapture"],
                       cwd=ROOT, env=env, check=True)


if __name__ == "__main__":
    main()
