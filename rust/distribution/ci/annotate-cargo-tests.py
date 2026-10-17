#!/usr/bin/env python3
"""Best-effort CI annotations: validated libtest failure names, never panic text."""
import os
import re
import stat
import sys

MAX_FILE = 32 * 1024 * 1024
MAX_LINE = 4096
MAX_NAMES = 256
NAME = rb"[A-Za-z_][A-Za-z_0-9]*(?:::[A-Za-z_][A-Za-z_0-9]*)*"
FAILED = re.compile(rb"test (" + NAME + rb") \.\.\. FAILED\Z")
SUMMARY = re.compile(rb"    (" + NAME + rb")\Z")
FOOTER = re.compile(
    rb"test result: FAILED\. [0-9]+ passed; ([0-9]+) failed; [0-9]+ ignored; "
    rb"[0-9]+ measured; [0-9]+ filtered out;(?: finished in [0-9.]+s)?\Z"
)


def workflow_escape(value):
    return value.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


def failed_names(stream):
    observed, summary, result = set(), None, set()
    while True:
        line = stream.readline(MAX_LINE + 1)
        if not line:
            return sorted(result)
        if len(line) > MAX_LINE:
            return []
        line = line.removesuffix(b"\n").removesuffix(b"\r")
        if line.startswith(b"running "):
            observed, summary = set(), None
        match = FAILED.fullmatch(line)
        if match and len(match[1]) <= 512:
            observed.add(match[1])
            if len(observed) > MAX_NAMES:
                return []
        if line == b"failures:":
            summary = []
            continue
        match = SUMMARY.fullmatch(line)
        if summary is not None and match and len(match[1]) <= 512:
            summary.append(match[1])
            if len(summary) > MAX_NAMES:
                return []
            continue
        footer = FOOTER.fullmatch(line)
        if footer:
            if (
                summary and 0 < int(footer[1]) == len(summary) == len(set(summary))
                and set(summary) <= observed
            ):
                result.update(summary)
                if len(result) > MAX_NAMES:
                    return []
            observed, summary = set(), None
        elif summary is not None and line:
            summary = None


def main():
    # The caller creates and owns this temporary file. Refuse links, devices and
    # oversized diagnostics; annotation failure never changes the Cargo result.
    fd = os.open(sys.argv[1], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or not 0 <= info.st_size <= MAX_FILE:
            return
        for name in failed_names(stream):
            print("::error title=Rust test failed::" + workflow_escape(name.decode("ascii")))


if __name__ == "__main__":
    main()
