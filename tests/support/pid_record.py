"""Read PID publications from subprocess fixtures."""


def read_pid(path):
    """Return a complete positive PID, or None while publication is pending."""
    try:
        content = path.read_text()
    except FileNotFoundError:
        return None
    if not content.endswith("\n"):
        return None
    digits = content[:-1]
    if not digits.isascii() or not digits.isdecimal() or int(digits) <= 0:
        raise ValueError(f"malformed PID record {path}: {content!r}")
    return int(digits)
