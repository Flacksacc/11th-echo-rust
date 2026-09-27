"""Build the optional Windows x64 runtime from pinned, hash-verified inputs.

Requires Python 3.12 and uv on the build machine only. Run after obtaining
Photon/Kestrel redistribution rights. End users need only an NVIDIA driver.
"""
import argparse
import hashlib
import importlib.metadata
import json
from pathlib import Path
import shutil
import subprocess
import sys
import urllib.parse
import urllib.request
import zipfile

PYTHON_URL = "https://www.python.org/ftp/python/3.12.10/python-3.12.10-embed-amd64.zip"
PYTHON_SHA256 = "4acbed6dd1c744b0376e3b1cf57ce906f9dc9e95e68824584c8099a63025a3c3"


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, default=Path("target/ultra-runtime"))
    parser.add_argument("--url-base", required=True, help="HTTPS directory where the runtime archive will be published")
    args = parser.parse_args()
    if sys.platform != "win32" or sys.version_info[:2] != (3, 12):
        parser.error("Build on Windows using Python 3.12")
    parsed = urllib.parse.urlparse(args.url_base)
    if parsed.scheme != "https" or not parsed.netloc or parsed.query or parsed.fragment:
        parser.error("--url-base must be an HTTPS directory URL")
    source = Path(__file__).resolve().parent
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    # Use a fresh directory; never recursively delete a user-provided build path.
    import tempfile
    bundle = Path(tempfile.mkdtemp(prefix="build-", dir=output))
    archive = output / "python-3.12.10-embed-amd64.zip"
    if not archive.exists():
        urllib.request.urlretrieve(PYTHON_URL, archive)
    if digest(archive) != PYTHON_SHA256:
        raise RuntimeError("Embedded Python checksum mismatch")
    with zipfile.ZipFile(archive) as inputs:
        for entry in inputs.infolist():
            if Path(entry.filename).name != entry.filename:
                raise RuntimeError("Unexpected embedded Python archive layout")
        inputs.extractall(bundle)
    (bundle / "python312._pth").write_text("python312.zip\n.\nlib\nimport site\n", encoding="utf-8")
    shutil.copyfile(source / "helper.py", bundle / "helper.py")
    subprocess.run([
        "uv", "pip", "install", "--python", str(bundle / "python.exe"),
        "--target", str(bundle / "lib"), "--require-hashes", "--only-binary", ":all:",
        "--requirements", str(source / "requirements.lock"),
    ], check=True)
    # Preserve all supplied license files in their packages and collect metadata
    # into a readable notice alongside Echo's explicit model attribution.
    notices = [
        "Echo optional Parakeet Ultra GPU runtime",
        "Photon/Kestrel components redistributed under the publisher's M87 Labs agreement.",
        "Parakeet Ultra weights: Moondream / M87 Labs, based on NVIDIA Parakeet V3; CC-BY-4.0.",
        "https://huggingface.co/moondream/parakeet-ultra",
        "Model license: https://creativecommons.org/licenses/by/4.0/",
        "Embedded Python license: LICENSE.txt",
        "Package license files are preserved beneath lib/.",
    ]
    for distribution in sorted(importlib.metadata.distributions(path=[str(bundle / "lib")]), key=lambda dist: dist.metadata["Name"].lower()):
        notices.append(f"\n{distribution.metadata['Name']} {distribution.version}\n{distribution.metadata.get('License-Expression') or distribution.metadata.get('License') or 'See supplied package notices / redistribution agreement.'}")
    (bundle / "THIRD_PARTY_NOTICES.txt").write_text("\n".join(notices), encoding="utf-8")
    files = []
    pending = output / "runtime.zip"
    with zipfile.ZipFile(pending, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=1, allowZip64=True) as packaged:
        for path in sorted(bundle.rglob("*")):
            if path.is_symlink():
                raise RuntimeError("Runtime inputs contain a symlink")
            if not path.is_file() or "__pycache__" in path.parts:
                continue
            name = path.relative_to(bundle).as_posix()
            files.append({"path": name, "size": path.stat().st_size, "sha256": digest(path)})
            packaged.write(path, name)
    checksum = digest(pending)
    name = f"echo-ultra-runtime-{checksum[:16]}.zip"
    packaged_path = output / name
    pending.replace(packaged_path)
    manifest = {"protocol": 1, "url": args.url_base.rstrip("/") + "/" + name, "size": packaged_path.stat().st_size, "sha256": checksum, "files": files}
    manifest_path = output / "runtime-manifest.json"
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(f"Runtime: {packaged_path}\nManifest: {manifest_path}\nDevelopment runtime directory: {bundle}")


if __name__ == "__main__":
    main()
