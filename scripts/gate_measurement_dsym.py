"""Account for Cargo's packed debug alias without traversing the link."""

import os
from pathlib import Path
import re
import stat

from verifier_state import Refusal


def account_alias(parent_fd, relative_path, observed):
    """Validate one same-profile physical dSYM destination; count only link blocks."""
    relative_path = Path(relative_path)
    match = re.fullmatch(r'debug/([A-Za-z0-9_-]+)\.dSYM', relative_path.as_posix())
    if match is None or not stat.S_ISLNK(observed.st_mode):
        raise Refusal(f'unsupported Cargo dSYM alias: {relative_path}')
    name = relative_path.name
    text = os.readlink(name, dir_fd=parent_fd)
    if re.fullmatch(r'deps/' + re.escape(match[1]) + r'-[0-9a-f]{16}\.dSYM', text) is None:
        raise Refusal(f'unsupported Cargo dSYM destination: {relative_path}')
    device = os.fstat(parent_fd).st_dev
    if observed.st_dev != device:
        raise Refusal(f'Cargo dSYM alias device changed: {relative_path}')
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC

    def directory_state(fd, child):
        value = os.stat(child, dir_fd=fd, follow_symlinks=False)
        if not stat.S_ISDIR(value.st_mode) or value.st_dev != device:
            raise Refusal(f'Cargo dSYM destination is not a physical directory: {relative_path}')
        return value

    def check_directory(value, expected):
        if (not stat.S_ISDIR(value.st_mode)
                or (value.st_dev, value.st_ino) != (expected.st_dev, expected.st_ino)):
            raise Refusal(f'Cargo dSYM directory identity changed: {relative_path}')

    deps = directory_state(parent_fd, 'deps')
    deps_fd = os.open('deps', flags, dir_fd=parent_fd)
    try:
        check_directory(os.fstat(deps_fd), deps)
        destination = text.split('/')[1]
        bundle = directory_state(deps_fd, destination)
        bundle_fd = os.open(destination, flags, dir_fd=deps_fd)
        try:
            check_directory(os.fstat(bundle_fd), bundle)
            final_text = os.readlink(name, dir_fd=parent_fd)
            final = os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
            def link_state(value):
                return (value.st_dev, value.st_ino, stat.S_IFMT(value.st_mode), value.st_ctime_ns)
            if final_text != text or link_state(final) != link_state(observed):
                raise Refusal(f'Cargo dSYM alias identity changed: {relative_path}')
            check_directory(directory_state(deps_fd, destination), bundle)
            check_directory(directory_state(parent_fd, 'deps'), deps)
        finally:
            os.close(bundle_fd)
    finally:
        os.close(deps_fd)
    return observed.st_blocks * 512
