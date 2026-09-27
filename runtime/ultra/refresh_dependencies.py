"""Print upstream download pins from the reviewed requirements.lock.

Maintainer-only tool: requires packaging; never resolves new dependency versions.
Review the output before replacing dependencies.json. End users do not run this.
"""
import concurrent.futures
import json
from pathlib import Path
import re
import urllib.request

from packaging.tags import compatible_tags, cpython_tags
from packaging.utils import parse_wheel_filename

TAGS = list(cpython_tags((3, 12), platforms=["win_amd64"])) + list(
    compatible_tags((3, 12), interpreter="cp312", platforms=["win_amd64"])
)
RANK = {tag: index for index, tag in reversed(list(enumerate(TAGS)))}


def wheel(name, version, allowed=None):
    with urllib.request.urlopen(f"https://pypi.org/pypi/{name}/{version}/json", timeout=60) as response:
        metadata = json.load(response)
    candidates = []
    for artifact in metadata["urls"]:
        if artifact["packagetype"] != "bdist_wheel" or artifact["yanked"]:
            continue
        checksum = artifact["digests"]["sha256"]
        if allowed is not None and checksum not in allowed:
            continue
        tags = parse_wheel_filename(artifact["filename"])[3]
        rank = min((RANK[tag] for tag in tags if tag in RANK), default=None)
        if rank is not None:
            candidates.append((rank, artifact))
    if not candidates:
        raise RuntimeError(f"No locked Windows CPython 3.12 wheel for {name} {version}")
    artifact = min(candidates, key=lambda item: (item[0], item[1]["filename"]))[1]
    return dict(name=name, version=version, filename=artifact["filename"],
                url=artifact["url"], size=artifact["size"], sha256=artifact["digests"]["sha256"])


def main():
    lock = Path(__file__).with_name("requirements.lock").read_text(encoding="utf-8")
    blocks = re.split(r"\n(?=[a-zA-Z0-9])", lock)
    jobs = []
    for block in blocks:
        match = re.match(r"([\w-]+)==([^\s\\]+)", block)
        if match:
            jobs.append((*match.groups(), set(re.findall(r"--hash=sha256:([a-f0-9]{64})", block))))
    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
        packages = list(pool.map(lambda job: wheel(*job), jobs))
    torch_url = re.search(r"torch @ (https://\S+)#sha256=([a-f0-9]{64})", lock)
    url, checksum = torch_url.groups()
    request = urllib.request.Request(url, method="HEAD")
    with urllib.request.urlopen(request, timeout=60) as response:
        size = int(response.headers["Content-Length"])
    packages.append(dict(name="torch", version="2.11.0+cu130",
                         filename="torch-2.11.0+cu130-cp312-cp312-win_amd64.whl",
                         url=url, size=size, sha256=checksum))
    result = dict(protocol=1, python=dict(
        filename="python-3.12.10-embed-amd64.zip",
        url="https://www.python.org/ftp/python/3.12.10/python-3.12.10-embed-amd64.zip",
        size=0, sha256="4acbed6dd1c744b0376e3b1cf57ce906f9dc9e95e68824584c8099a63025a3c3"),
        pip=wheel("pip", "26.0.1"), packages=sorted(packages, key=lambda item: item["name"]))
    with urllib.request.urlopen(urllib.request.Request(result["python"]["url"], method="HEAD"), timeout=60) as response:
        result["python"]["size"] = int(response.headers["Content-Length"])
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
