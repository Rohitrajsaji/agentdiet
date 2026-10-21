#!/usr/bin/env python3
"""Package a built agentdiet binary into a reproducible release archive.

    scripts/package.py binary  <target> <path/to/agentdiet[.exe]> <outdir>
    scripts/package.py completions <path/to/agentdiet> <outdir>

The archive is byte-for-byte reproducible: sorted entries, fixed mtime (SOURCE_DATE_EPOCH, or the
last commit time), owner 0/0, fixed modes, gzip without a timestamp. Windows targets get a .zip.
Prints the archive path. Standard library only.
"""
import gzip, hashlib, io, os, subprocess, sys, tarfile, zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DOCS = ["LICENSE", "README.md", "CHANGELOG.md", "SECURITY.md", "THIRD_PARTY_NOTICES"]


def version():
    for line in open(os.path.join(ROOT, "Cargo.toml"), encoding="utf-8"):
        if line.startswith("version = "):
            return line.split('"')[1]
    sys.exit("no version in Cargo.toml")


def epoch():
    if "SOURCE_DATE_EPOCH" in os.environ:
        return int(os.environ["SOURCE_DATE_EPOCH"])
    out = subprocess.run(["git", "-C", ROOT, "log", "-1", "--format=%ct"], capture_output=True, text=True)
    return int(out.stdout.strip() or 0)


def tar_gz(path, prefix, files, mtime):
    """files: list of (name inside prefix, bytes, mode)."""
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.PAX_FORMAT) as tar:
        for name, data, mode in sorted(files):
            info = tarfile.TarInfo(f"{prefix}/{name}")
            info.size, info.mode, info.mtime = len(data), mode, mtime
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            tar.addfile(info, io.BytesIO(data))
    with open(path, "wb") as f, gzip.GzipFile(fileobj=f, mode="wb", mtime=0, filename="") as gz:
        gz.write(raw.getvalue())


def zip_file(path, prefix, files, mtime):
    import time
    stamp = time.gmtime(max(mtime, 315532800))[:6]  # zip cannot store dates before 1980
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        for name, data, mode in sorted(files):
            info = zipfile.ZipInfo(f"{prefix}/{name}", date_time=stamp)
            info.external_attr = (mode & 0xFFFF) << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            z.writestr(info, data)


def read(path):
    with open(path, "rb") as f:
        return f.read()


def main(argv):
    if len(argv) == 4 and argv[0] == "binary":
        target, binary, outdir = argv[1:]
        exe = "agentdiet.exe" if "-windows-" in target else "agentdiet"
        files = [(exe, read(binary), 0o755)]
        files += [(d, read(os.path.join(ROOT, d)), 0o644) for d in DOCS]
        prefix = f"agentdiet-{version()}-{target}"
    elif len(argv) == 3 and argv[0] == "completions":
        binary, outdir = argv[1:]
        files = []
        for shell, name in [("bash", "agentdiet.bash"), ("zsh", "_agentdiet"), ("fish", "agentdiet.fish")]:
            out = subprocess.run([binary, "completions", shell], capture_output=True, check=True).stdout
            files.append((name, out, 0o644))
        prefix, target = f"agentdiet-{version()}-completions", "completions"
    else:
        sys.exit(__doc__)
    os.makedirs(outdir, exist_ok=True)
    windows = "-windows-" in target
    path = os.path.join(outdir, prefix + (".zip" if windows else ".tar.gz"))
    (zip_file if windows else tar_gz)(path, prefix, files, epoch())
    digest = hashlib.sha256(read(path)).hexdigest()
    with open(path + ".sha256", "w", encoding="ascii", newline="\n") as f:  # LF on Windows too
        f.write(f"{digest}  {os.path.basename(path)}\n")
    print(path)


if __name__ == "__main__":
    main(sys.argv[1:])
